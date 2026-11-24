use super::*;

struct TestDir(PathBuf);
impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn fixture() -> (TestDir, Link, mpsc::UnboundedReceiver<String>) {
    let dir = TestDir(std::env::temp_dir().join(format!("link-signal-status-{}", uuid::Uuid::new_v4())));
    let link = Link::open(&dir.0, 0, Arc::new(|_| Ok(()))).unwrap();
    link.0.cfg.lock().unwrap().enabled = true;
    *link.0.running.lock().unwrap() = Some(CancellationToken::new());
    let receiver = add_peer(&link, "registered-pc", Instant::now());
    (dir, link, receiver)
}

fn add_peer(link: &Link, id: &str, received: Instant) -> mpsc::UnboundedReceiver<String> {
    link.0.cfg.lock().unwrap().peers.push(Peer {
        node_id: id.into(), name: format!("Computer {id}"), key: hex(&[7; 32]), addrs: vec![],
    });
    let (tx, receiver) = mpsc::unbounded_channel();
    let mut state = PeerState {
        tx: Some(tx), session_cancel: Some(CancellationToken::new()), generation: 3,
        signal_on: true, last_seen_ms: now_ms(), ..Default::default()
    };
    state.record_signal_report(3, &json!({"ping": 1, "signal": true}), received);
    link.0.peers.lock().unwrap().insert(id.into(), state);
    receiver
}

#[test]
fn relay_uses_its_own_monotonic_report_clock() {
    let (_dir, link, _receiver) = fixture();
    let received = Instant::now();
    link.0.peers.lock().unwrap().get_mut("registered-pc").unwrap()
        .record_signal_report(3, &json!({"ping": 1, "signal": true}), received);
    let fresh = link.signal_relay_status_at(received + Duration::from_millis(2500));
    assert_eq!(fresh.state, "available");
    assert_eq!(fresh.valid_for_ms, 1500);
    assert_eq!(fresh.peer.unwrap().name, "Computer registered-pc");
    assert_eq!(link.signal_relay_status_at(received + SIGNAL_REPORT_TTL).state, "available");
    // Other authenticated packets prove only the transport is alive.
    assert!(link.0.peers.lock().unwrap().get_mut("registered-pc").unwrap().record_packet(3, now_ms() + 60_000));
    assert_eq!(link.signal_relay_status_at(received + SIGNAL_REPORT_TTL + Duration::from_nanos(1)).state, "unavailable");
    assert_eq!(link.signal_relay_status_at(received - Duration::from_millis(1)).state, "unavailable");
}

#[test]
fn heartbeat_requires_an_explicit_valid_signal_report() {
    for payload in [json!({"ping": 1, "signal": false}), json!({"ping": 1}),
        json!({"ping": 1, "signal": "true"}), json!({"ping": "1", "signal": true}), json!({"signal": true})] {
        let (_dir, link, _receiver) = fixture();
        let tx = link.0.peers.lock().unwrap()["registered-pc"].tx.clone().unwrap();
        let packet = PeerPacket::signed("registered-pc", now_ms(), PacketKind::Heartbeat, 1, payload, &[7; 32]).unwrap();
        link.receive("local", &[7; 32], "registered-pc", 3, packet, &tx);
        assert_eq!(link.signal_relay_status().state, "unavailable");
    }
}

#[test]
fn relay_requires_configuration_live_transport_and_current_session() {
    for reject in 0..7 {
        let (_dir, link, receiver) = fixture();
        match reject {
            0 => link.0.cfg.lock().unwrap().enabled = false,
            1 => link.0.running.lock().unwrap().as_ref().unwrap().cancel(),
            2 => link.0.cfg.lock().unwrap().peers.clear(),
            3 => link.0.peers.lock().unwrap().get_mut("registered-pc").unwrap().generation += 1,
            4 => link.0.peers.lock().unwrap().get_mut("registered-pc").unwrap().tx = None,
            5 => link.0.peers.lock().unwrap().get_mut("registered-pc").unwrap().session_cancel.as_ref().unwrap().cancel(),
            6 => drop(receiver),
            _ => unreachable!(),
        }
        assert_eq!(link.signal_relay_status().state, "unavailable", "rejection {reject}");
    }
}

#[test]
fn several_ready_peers_are_reported_without_selecting_an_account() {
    let (_dir, link, _one) = fixture();
    let _two = add_peer(&link, "second-pc", Instant::now());
    let status = link.signal_relay_status();
    assert_eq!(status.state, "multiple");
    assert_eq!(status.candidate_count, 2);
    assert!(status.peer.is_none());
}

#[tokio::test]
async fn reconnect_clears_prior_signal_readiness_before_the_first_report() {
    let (_dir, link, _receiver) = fixture();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let remote = TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
    let (socket, _) = listener.accept().await.unwrap();
    let LinkConnection { stream, evidence } = LinkConnection::tcp(socket).unwrap();
    let route = evidence.remote_endpoint();
    let (read, write) = tokio::io::split(stream);
    let peer = link.0.cfg.lock().unwrap().peers[0].clone();
    let token = CancellationToken::new();
    let session_link = link.clone();
    let session_token = token.clone();
    let task = tokio::spawn(async move {
        session_link.session(peer, vec![7; 32], BufReader::new(read), write, route, evidence, None, session_token).await;
    });
    tokio::time::timeout(Duration::from_secs(1), async {
        while link.0.peers.lock().unwrap()["registered-pc"].generation == 3 {
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    assert_eq!(link.signal_relay_status().state, "unavailable");
    assert!(link.0.peers.lock().unwrap()["registered-pc"].signal_on, "status must not change the existing Signal election");
    token.cancel();
    tokio::time::timeout(Duration::from_secs(1), task).await.unwrap().unwrap();
    drop(remote);
}
