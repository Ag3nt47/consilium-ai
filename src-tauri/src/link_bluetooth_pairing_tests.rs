use super::*;

struct TestDir(PathBuf);
impl Drop for TestDir {
    fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
}

fn fixture() -> (TestDir, Link, mpsc::UnboundedReceiver<String>, Instant) {
    let dir = TestDir(std::env::temp_dir().join(format!("link-bluetooth-pairing-{}", uuid::Uuid::new_v4())));
    let link = Link::open(&dir.0, 0, Arc::new(|_| Ok(()))).unwrap();
    {
        let mut cfg = link.0.cfg.lock().unwrap();
        cfg.enabled = true;
        cfg.peers.push(Peer { node_id: "saved-mac".into(), name: "Saved Mac".into(), key: hex(&[7; 32]),
            // A persisted candidate alone is deliberately insufficient.
            addrs: vec!["rfcomm://020000000001".into()],
        });
    }
    *link.0.running.lock().unwrap() = Some(CancellationToken::new());
    let (tx, receiver) = mpsc::unbounded_channel();
    link.0.peers.lock().unwrap().insert("saved-mac".into(), PeerState {
        tx: Some(tx), session_cancel: Some(CancellationToken::new()), generation: 3,
        ..Default::default()
    });
    (dir, link, receiver, Instant::now())
}

fn report(link: &Link, payload: Value, now: Instant) {
    link.0.peers.lock().unwrap().get_mut("saved-mac").unwrap()
        .record_bluetooth_advertisement(3, &payload, now);
}

fn announcement() -> Value {
    json!({"ping": 1, "addrs": ["192.0.2.2:8470", "rfcomm://020000000001"]})
}

#[test]
fn saved_routes_cannot_authorize_pairing_but_an_authenticated_live_announcement_can() {
    let (_dir, link, _receiver, now) = fixture();
    assert!(link.bluetooth_pairing_binding_at("saved-mac", now).is_err());
    let tx = link.0.peers.lock().unwrap()["saved-mac"].tx.clone().unwrap();
    let packet = PeerPacket::signed("saved-mac", now_ms(), PacketKind::Heartbeat, 1, announcement(), &[7; 32]).unwrap();
    link.receive("local", &[7; 32], "saved-mac", 3, packet, &tx);
    let (binding, name) = link.bluetooth_pairing_binding_at("saved-mac", Instant::now()).unwrap();
    assert_eq!(name, "Saved Mac");
    assert_eq!(binding.address, "020000000001");
    assert!(link.bluetooth_pairing_is_current_at(&binding, Instant::now()));
    assert!(link.bluetooth_pairing_binding_at("not-a-saved-peer", Instant::now()).is_err());
}

#[test]
fn pairing_announcement_expires_independently_of_other_packet_activity() {
    let (_dir, link, _receiver, now) = fixture();
    report(&link, announcement(), now);
    let (binding, _) = link.bluetooth_pairing_binding_at("saved-mac", now).unwrap();
    assert!(link.0.peers.lock().unwrap().get_mut("saved-mac").unwrap().record_packet(3, now_ms() + 60_000));
    assert!(link.bluetooth_pairing_is_current_at(&binding, now + BLUETOOTH_ADVERTISEMENT_TTL - Duration::from_nanos(1)));
    assert!(!link.bluetooth_pairing_is_current_at(&binding, now + BLUETOOTH_ADVERTISEMENT_TTL));
    assert!(!link.bluetooth_pairing_is_current_at(&binding, now - Duration::from_nanos(1)));
    report(&link, announcement(), now + Duration::from_secs(2));
    assert!(link.bluetooth_pairing_is_current_at(&binding, now + Duration::from_secs(5)));
}

#[test]
fn missing_disabled_ambiguous_or_malformed_radio_announcements_revoke_prior_authority() {
    for payload in [json!({"ping": 2}), json!({"ping": 2, "addrs": []}),
        json!({"ping": 2, "addrs": ["192.0.2.2:8470"]}),
        json!({"ping": "2", "addrs": ["rfcomm://020000000001"]}),
        json!({"ping": 2, "addrs": ["rfcomm://020000000001", "rfcomm://020000000002"]}),
        json!({"ping": 2, "addrs": ["rfcomm://020000000001", false]}),
        json!({"ping": 2, "addrs": ["rfcomm://020000000001", "rfcomm://000000000000"]})] {
        let (_dir, link, _receiver, now) = fixture();
        report(&link, announcement(), now);
        let (binding, _) = link.bluetooth_pairing_binding_at("saved-mac", now).unwrap();
        report(&link, payload, now);
        assert!(!link.bluetooth_pairing_is_current_at(&binding, now));
    }
}

#[test]
fn attempts_cannot_migrate_between_peer_keys_radios_or_connection_generations() {
    for change in 0..9 {
        let (_dir, link, receiver, now) = fixture();
        report(&link, announcement(), now);
        let (binding, _) = link.bluetooth_pairing_binding_at("saved-mac", now).unwrap();
        match change {
            0 => link.0.cfg.lock().unwrap().peers[0].key = hex(&[8; 32]),
            1 => link.0.cfg.lock().unwrap().peers.clear(),
            2 => link.0.cfg.lock().unwrap().enabled = false,
            3 => link.0.running.lock().unwrap().as_ref().unwrap().cancel(),
            4 => link.0.peers.lock().unwrap().get_mut("saved-mac").unwrap().generation += 1,
            5 => link.0.peers.lock().unwrap().get_mut("saved-mac").unwrap().session_cancel.as_ref().unwrap().cancel(),
            6 => drop(receiver),
            7 => report(&link, json!({"ping": 2, "addrs": ["rfcomm://020000000002"]}), now),
            8 => link.0.cfg.lock().unwrap().peers[0].key = "damaged".into(),
            _ => unreachable!(),
        }
        assert!(!link.bluetooth_pairing_is_current_at(&binding, now), "change {change}");
    }
}

#[test]
fn an_old_socket_cannot_replace_the_current_sessions_pairing_announcement() {
    let (_dir, link, _receiver, now) = fixture();
    report(&link, announcement(), now);
    let (binding, _) = link.bluetooth_pairing_binding_at("saved-mac", now).unwrap();
    link.0.peers.lock().unwrap().get_mut("saved-mac").unwrap()
        .record_bluetooth_advertisement(2, &json!({"ping": 2, "addrs": ["rfcomm://020000000002"]}), now);
    assert!(link.bluetooth_pairing_is_current_at(&binding, now));
    let mut states = link.0.peers.lock().unwrap();
    let state = states.get_mut("saved-mac").unwrap();
    state.generation += 1;
    state.record_bluetooth_advertisement(3, &announcement(), now);
    drop(states);
    assert!(link.bluetooth_pairing_binding_at("saved-mac", now).is_err());
}
