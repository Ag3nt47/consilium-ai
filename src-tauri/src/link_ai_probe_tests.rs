use super::*;
use super::tests::{memory_evidence, node, trust_memory_pair};

struct TestDir(PathBuf);
impl Drop for TestDir {
    fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
}

fn fixture() -> (TestDir, Link, Link, mpsc::UnboundedReceiver<String>) {
    let dir = TestDir(std::env::temp_dir().join(format!("link-ai-probe-{}", uuid::Uuid::new_v4())));
    let (host, _) = node(&dir.0, "host");
    let (client, _) = node(&dir.0, "client");
    trust_memory_pair(&host, &client);
    let (tx, receiver) = mpsc::unbounded_channel();
    client.0.peers.lock().unwrap().insert(host.me().0, PeerState {
        tx: Some(tx), session_cancel: Some(CancellationToken::new()), generation: 17,
        route: Some("192.0.2.2:8470".into()), council: vec!["gpt".into()], ai_reports: 23,
        ..Default::default()
    });
    (dir, host, client, receiver)
}

#[tokio::test]
async fn a_native_ai_query_uses_fresh_authenticated_status_without_publishing_a_session() {
    for fault in 0..6 {
        let (_dir, host, client, _primary_receiver) = fixture();
        let peer = client.peer(&host.me().0).unwrap();
        let key = peer_key(&peer).unwrap();
        let response_key = if fault == 4 { vec![0x43; 32] } else { key.clone() };
        let (server, stream) = tokio::io::duplex(17);
        let responder = host.clone();
        let responding = tokio::spawn(async move {
            let stream: LinkStream = Box::new(server);
            let (read, mut write) = tokio::io::split(stream);
            let mut read = BufReader::new(read);
            let nonce = random_hex(16);
            write_wire(&mut write, &Wire::Challenge { nonce: nonce.clone() }).await.unwrap();
            let Wire::Sealed { from, n, c } = read_wire(&mut read).await.unwrap() else { panic!("Expected sealed AI query") };
            let request = open(&from, &key, &n, &c).unwrap();
            request.validate(&from, &key, now_ms()).unwrap();
            let probe = &request.payload["route_probe"];
            assert_eq!(probe["challenge"].as_str(), Some(nonce.as_str()));
            assert_eq!(probe["ai_status"].as_bool(), Some(true));
            assert!(probe.get("transfer_chunk").is_none());
            let response_nonce = if fault == 3 { random_hex(16) } else { probe["nonce"].as_str().unwrap().into() };
            let mut payload = json!({"route_probe_ack": response_nonce,
                "ai_status": {"council": ["claude"], "read": {"gpt": ["claude"]}}});
            if fault == 1 { payload.as_object_mut().unwrap().remove("ai_status"); }
            if fault == 2 { payload["ai_status"]["council"] = json!("not a list"); }
            if fault == 5 { payload["padding"] = json!("x".repeat(AI_STATUS_FRAME_LIMIT as usize)); }
            let reply = responder.route_probe_frame(&response_key, payload).unwrap();
            // The bounded reader deliberately drops an oversized response.
            let _ = write.write_all(reply.as_bytes()).await;
            let _ = write.flush().await;
        });
        let result = tokio::time::timeout(Duration::from_secs(5), client.probe_ai_handshake(&peer,
            &peer_key(&peer).unwrap(), LinkConnection { stream: Box::new(stream),
                evidence: memory_evidence("020000000002", "020000000001") }, "rfcomm://020000000001")).await.unwrap();
        if fault == 0 {
            let report = result.unwrap();
            let remote: Vec<_> = report.handshake.iter().filter(|ai| ai.side == "there").collect();
            assert_eq!(remote.len(), 1);
            assert_eq!(remote[0].ai, "claude", "The result must use the fresh probe rather than cached GPT status");
            assert!(remote[0].reads);
            assert_eq!(report.route, "rfcomm://020000000001");
            assert!(matches!(report.route_evidence, Some(RouteEvidence::BluetoothRfcomm { .. })));
            assert!(report.restored.is_none());
        } else {
            let error = result.unwrap_err();
            if fault == 1 { assert!(error.contains("Update both computers"), "{error}"); }
            if fault == 2 { assert!(error.contains("invalid AI status"), "{error}"); }
            if fault == 3 { assert!(error.contains("stale route probe"), "{error}"); }
        }
        tokio::time::timeout(Duration::from_secs(5), responding).await.unwrap().unwrap();
        let states = client.0.peers.lock().unwrap();
        let state = &states[&peer.node_id];
        assert_eq!(state.generation, 17);
        assert_eq!(state.route.as_deref(), Some("192.0.2.2:8470"));
        assert_eq!(state.ai_reports, 23);
        assert_eq!(state.council, ["gpt"]);
        assert!(!state.session_cancel.as_ref().unwrap().is_cancelled());
        assert!(host.inbox().is_empty());
    }
}

#[tokio::test]
async fn the_production_probe_receiver_answers_ai_status_without_creating_a_session_or_receipt() {
    let (_dir, host, client, _primary_receiver) = fixture();
    let peer = client.peer(&host.me().0).unwrap();
    let key = peer_key(&peer).unwrap();
    let (server, stream) = tokio::io::duplex(17);
    let receiver = host.clone();
    let receiving = tokio::spawn(async move {
        receiver.accept_connection(LinkConnection { stream: Box::new(server),
            evidence: memory_evidence("020000000001", "020000000002") }, CancellationToken::new()).await
    });
    let report = tokio::time::timeout(Duration::from_secs(5), client.probe_ai_handshake(&peer, &key,
        LinkConnection { stream: Box::new(stream), evidence: memory_evidence("020000000002", "020000000001") },
        "rfcomm://020000000001")).await.unwrap().unwrap();
    tokio::time::timeout(Duration::from_secs(5), receiving).await.unwrap().unwrap().unwrap();
    assert!(report.restored.is_none());
    assert!(host.0.peers.lock().unwrap().get(&client.me().0).is_none_or(|state| state.generation == 0 && state.tx.is_none()));
    assert_eq!(client.0.peers.lock().unwrap()[&peer.node_id].generation, 17);
    assert!(host.inbox().is_empty());
    assert!(client.inbox().is_empty());
}

#[tokio::test]
async fn native_ai_queries_reject_an_ip_source_before_any_transport_call() {
    let (_dir, host, client, _primary_receiver) = fixture();
    let endpoint = "rfcomm://020000000001";
    client.0.cfg.lock().unwrap().peers[0].addrs = vec![endpoint.into()];
    let error = client.test_ai_handshake_selected(&host.me().0, Some(endpoint), Some("127.0.0.1".parse().unwrap())).await.unwrap_err();
    assert_eq!(error, "Native Bluetooth has no IP source address");
}
