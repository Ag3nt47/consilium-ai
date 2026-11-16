//! Direct link between this computer and the user's other paired computers (Mac, Windows, Linux).
//!
//! * Pairing: the host shows a one-time 6-digit code; the joiner types it. SPAKE2 turns the code
//!   into a shared key, so an eavesdropper learns nothing and an active attacker gets one guess per
//!   attempt. A code expires after 5 minutes or 3 wrong tries.
//! * Transport: TCP on port 8470, one JSON object per line. After pairing every frame is sealed
//!   with ChaCha20-Poly1305 and every packet is an HMAC-authenticated `PeerPacket`; sessions start
//!   with a fresh challenge in both directions, so a recorded session can't be replayed.
//! * Delivery: incoming packets go through the crash-safe `ReplayLedger` before dispatch and are
//!   acknowledged only after dispatch, so nothing is taken in twice. Unacknowledged outgoing
//!   packets stay queued and are re-sent, re-stamped, on reconnect.
//! * Health: a heartbeat every 3 s; a silent link is dropped after 10 s and redialled from 2 s,
//!   backing off to 30 s.
//! * Routes: each side advertises every local address (cable, Wi-Fi, Tailscale). The dialler tries
//!   them all at once and keeps the one that answers first, so the fastest working route wins and
//!   a failed route falls back to the next on the following attempt.

use crate::peer_protocol::{PacketKind, PeerPacket, ReplayDisposition, ReplayLedger};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::Sha256;
use spake2::{Ed25519Group, Identity, Password, Spake2};
use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub const PORT: u16 = 8470;
const HEARTBEAT: Duration = Duration::from_secs(3);
const DEAD_AFTER_MS: u64 = 10_000;
const RETRY_MIN: Duration = Duration::from_secs(2);
const RETRY_MAX: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_millis(1500);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const PAIR_TTL_MS: u64 = 5 * 60 * 1000;
const PAIR_ATTEMPTS: u32 = 3;
const LINE_LIMIT: usize = 3 * 1024 * 1024;
const OUTBOX_LIMIT: usize = 2000;
/// The higher node id dials too once its peer has been unreachable this long.
const BACKUP_DIAL_AFTER_MS: u64 = 20_000;
const PAIR_ID: &[u8] = b"consilium-link-pair-v1";

pub type Handler = Arc<dyn Fn(&PeerPacket) -> Result<(), String> + Send + Sync>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Peer {
    pub node_id: String,
    pub name: String,
    /// Hex-encoded 32-byte link key. Never leaves this computer's config file.
    key: String,
    pub addrs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LinkConfig {
    pub enabled: bool,
    pub node_id: String,
    pub name: String,
    pub peers: Vec<Peer>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PeerView {
    pub node_id: String,
    pub name: String,
    pub connected: bool,
    pub route: Option<String>,
    pub rtt_ms: Option<u64>,
    pub last_seen_ms: Option<u64>,
    pub queued: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct LinkStatus {
    pub enabled: bool,
    pub node_id: String,
    pub name: String,
    pub port: u16,
    pub addrs: Vec<String>,
    pub pairing_code_expires_ms: Option<u64>,
    pub peers: Vec<PeerView>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PairingCode {
    pub code: String,
    pub expires_ms: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum Wire {
    Challenge { nonce: String },
    PairStart { node_id: String, name: String, port: u16, spake: String },
    PairConfirm { mac: String },
    Sealed { from: String, n: String, c: String },
}

struct Pending {
    code: String,
    expires_ms: u64,
    failures: u32,
    busy: bool,
}

#[derive(Default)]
struct PeerState {
    tx: Option<mpsc::UnboundedSender<String>>,
    generation: u64,
    route: Option<String>,
    rtt_ms: Option<u64>,
    last_seen_ms: u64,
    down_since_ms: u64,
    outbox: VecDeque<PeerPacket>,
}

struct Inner {
    cfg_path: PathBuf,
    cfg: Mutex<LinkConfig>,
    ledger: ReplayLedger,
    pending: Mutex<Option<Pending>>,
    peers: Mutex<HashMap<String, PeerState>>,
    handler: Handler,
    port: u16,
    seq: AtomicU64,
    running: Mutex<Option<CancellationToken>>,
    error: Mutex<Option<String>>,
}

#[derive(Clone)]
pub struct Link(Arc<Inner>);

impl Link {
    /// Opens (creating if needed) the link state in `dir`. Nothing listens until `start`.
    pub fn open(dir: &Path, port: u16, handler: Handler) -> Result<Self, String> {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let cfg_path = dir.join("link.json");
        let mut cfg: LinkConfig = match std::fs::read(&cfg_path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| format!("link.json: {e}"))?,
            Err(_) => LinkConfig::default(),
        };
        if cfg.node_id.is_empty() {
            cfg.node_id = format!("node-{}", &uuid::Uuid::new_v4().simple().to_string()[..16]);
        }
        if cfg.name.is_empty() {
            cfg.name = computer_name();
        }
        write_private(&cfg_path, &serde_json::to_vec_pretty(&cfg).map_err(|e| e.to_string())?)?;
        Ok(Link(Arc::new(Inner {
            ledger: ReplayLedger::new(dir.join("sync_ledger.jsonl")),
            cfg_path,
            cfg: Mutex::new(cfg),
            pending: Mutex::new(None),
            peers: Mutex::new(HashMap::new()),
            handler,
            port,
            seq: AtomicU64::new(now_ms()),
            running: Mutex::new(None),
            error: Mutex::new(None),
        })))
    }

    pub fn enabled(&self) -> bool {
        self.0.cfg.lock().unwrap().enabled
    }

    /// Turns the link on or off and remembers the choice.
    pub fn set_enabled(&self, on: bool) -> Result<(), String> {
        {
            let mut cfg = self.0.cfg.lock().unwrap();
            cfg.enabled = on;
            self.save(&cfg)?;
        }
        if on {
            self.start();
        } else {
            self.stop();
        }
        Ok(())
    }

    /// Starts listening and dialling paired computers. Idempotent.
    pub fn start(&self) {
        let token = {
            let mut running = self.0.running.lock().unwrap();
            if running.is_some() {
                return;
            }
            let token = CancellationToken::new();
            *running = Some(token.clone());
            token
        };
        // Bind now, so a pairing code is only shown once the other computer can actually reach us.
        let bound = std::net::TcpListener::bind(("0.0.0.0", self.0.port))
            .and_then(|l| l.set_nonblocking(true).map(|_| l))
            .and_then(TcpListener::from_std);
        match bound {
            Ok(listener) => {
                *self.0.error.lock().unwrap() = None;
                let me = self.clone();
                let t = token.clone();
                tokio::spawn(async move { me.listen(listener, t).await });
            }
            Err(e) => {
                *self.0.error.lock().unwrap() = Some(format!("couldn't open port {}: {e}", self.0.port));
            }
        }
        let me = self.clone();
        tokio::spawn(async move { me.supervise(token).await });
    }

    pub fn stop(&self) {
        if let Some(token) = self.0.running.lock().unwrap().take() {
            token.cancel();
        }
        for state in self.0.peers.lock().unwrap().values_mut() {
            state.tx = None;
            state.generation += 1;
        }
    }

    pub fn status(&self) -> LinkStatus {
        let cfg = self.0.cfg.lock().unwrap().clone();
        let peers = self.0.peers.lock().unwrap();
        let pending = self.0.pending.lock().unwrap();
        LinkStatus {
            enabled: cfg.enabled,
            node_id: cfg.node_id.clone(),
            name: cfg.name.clone(),
            port: self.0.port,
            addrs: local_addrs(self.0.port),
            pairing_code_expires_ms: pending
                .as_ref()
                .filter(|p| p.expires_ms > now_ms())
                .map(|p| p.expires_ms),
            peers: cfg
                .peers
                .iter()
                .map(|p| {
                    let s = peers.get(&p.node_id);
                    PeerView {
                        node_id: p.node_id.clone(),
                        name: p.name.clone(),
                        connected: s.is_some_and(|s| s.tx.is_some()),
                        route: s.and_then(|s| s.tx.as_ref().and(s.route.clone())),
                        rtt_ms: s.and_then(|s| s.rtt_ms),
                        last_seen_ms: s.map(|s| s.last_seen_ms).filter(|&t| t > 0),
                        queued: s.map_or(0, |s| s.outbox.len()),
                    }
                })
                .collect(),
            error: self.0.error.lock().unwrap().clone(),
        }
    }

    /// Shows a fresh one-time pairing code and turns the link on so the other computer can reach
    /// this one. Any earlier code stops working.
    pub fn begin_pairing(&self) -> Result<PairingCode, String> {
        let code = format!("{:06}", uuid::Uuid::new_v4().as_u128() % 1_000_000);
        let expires_ms = now_ms() + PAIR_TTL_MS;
        *self.0.pending.lock().unwrap() =
            Some(Pending { code: code.clone(), expires_ms, failures: 0, busy: false });
        self.set_enabled(true)?;
        Ok(PairingCode { code, expires_ms })
    }

    pub fn cancel_pairing(&self) {
        *self.0.pending.lock().unwrap() = None;
    }

    /// Pairs with the computer showing `code` at `address` (host or host:port).
    pub async fn join(&self, address: &str, code: &str) -> Result<PeerView, String> {
        let code = code.trim().replace([' ', '-'], "");
        if code.len() != 6 || !code.chars().all(|c| c.is_ascii_digit()) {
            return Err("the pairing code is the 6 digits shown on the other computer".into());
        }
        let addr = with_port(address.trim(), PORT);
        let stream = tokio::time::timeout(HANDSHAKE_TIMEOUT, TcpStream::connect(&addr))
            .await
            .map_err(|_| format!("couldn't reach {addr}: no answer"))?
            .map_err(|e| format!("couldn't reach {addr}: {e}"))?;
        let peer = tokio::time::timeout(HANDSHAKE_TIMEOUT, self.pair_as_joiner(stream, &code))
            .await
            .map_err(|_| "pairing timed out".to_string())??;
        let mut peer = peer;
        if !peer.addrs.contains(&addr) {
            peer.addrs.insert(0, addr);
        }
        self.remember(peer.clone())?;
        self.set_enabled(true)?;
        Ok(self.status().peers.into_iter().find(|p| p.node_id == peer.node_id).expect("just paired"))
    }

    pub fn unpair(&self, node_id: &str) -> Result<(), String> {
        let mut cfg = self.0.cfg.lock().unwrap();
        cfg.peers.retain(|p| p.node_id != node_id);
        self.save(&cfg)?;
        if let Some(mut s) = self.0.peers.lock().unwrap().remove(node_id) {
            s.tx = None;
        }
        Ok(())
    }

    /// Queues a packet for every paired computer; it is delivered now if connected, otherwise
    /// as soon as a route comes back. Returns the packet id.
    pub fn send(&self, kind: PacketKind, payload: Value) -> Result<String, String> {
        let (me, peers) = {
            let cfg = self.0.cfg.lock().unwrap();
            (cfg.node_id.clone(), cfg.peers.clone())
        };
        if peers.is_empty() {
            return Err("no paired computers".into());
        }
        let seq = self.0.seq.fetch_add(1, Ordering::SeqCst);
        let mut first_id = String::new();
        for peer in &peers {
            let key = peer_key(peer)?;
            let packet = PeerPacket::signed(&me, now_ms(), kind, seq, payload.clone(), &key)?;
            if first_id.is_empty() {
                first_id = packet.id.clone();
            }
            let line = seal(&me, &key, &packet)?;
            let mut states = self.0.peers.lock().unwrap();
            let state = states.entry(peer.node_id.clone()).or_default();
            if state.outbox.len() >= OUTBOX_LIMIT {
                state.outbox.pop_front();
            }
            state.outbox.push_back(packet);
            if let Some(tx) = &state.tx {
                let _ = tx.send(line);
            }
        }
        Ok(first_id)
    }

    // ---- internals ----------------------------------------------------------------------------

    fn save(&self, cfg: &LinkConfig) -> Result<(), String> {
        write_private(&self.0.cfg_path, &serde_json::to_vec_pretty(cfg).map_err(|e| e.to_string())?)
    }

    fn remember(&self, peer: Peer) -> Result<(), String> {
        let mut cfg = self.0.cfg.lock().unwrap();
        cfg.peers.retain(|p| p.node_id != peer.node_id);
        cfg.peers.push(peer);
        self.save(&cfg)
    }

    fn me(&self) -> (String, String) {
        let cfg = self.0.cfg.lock().unwrap();
        (cfg.node_id.clone(), cfg.name.clone())
    }

    fn peer(&self, node_id: &str) -> Option<Peer> {
        self.0.cfg.lock().unwrap().peers.iter().find(|p| p.node_id == node_id).cloned()
    }

    async fn listen(self, listener: TcpListener, token: CancellationToken) {
        loop {
            tokio::select! {
                _ = token.cancelled() => return,
                accepted = listener.accept() => {
                    let Ok((stream, from)) = accepted else { continue };
                    let me = self.clone();
                    let t = token.clone();
                    tokio::spawn(async move {
                        let _ = me.accept(stream, from, t).await;
                    });
                }
            }
        }
    }

    async fn accept(self, stream: TcpStream, from: SocketAddr, token: CancellationToken) -> Result<(), String> {
        let _ = stream.set_nodelay(true);
        let (r, mut w) = stream.into_split();
        let mut r = BufReader::new(r);
        let nonce = random_hex(16);
        write_wire(&mut w, &Wire::Challenge { nonce: nonce.clone() }).await?;
        let first = tokio::time::timeout(HANDSHAKE_TIMEOUT, read_wire(&mut r))
            .await
            .map_err(|_| "handshake timed out".to_string())??;
        match first {
            Wire::PairStart { node_id, name, port, spake } => {
                self.pair_as_host(r, w, from, node_id, name, port, spake).await
            }
            Wire::Sealed { from: node, n, c } => {
                let peer = self.peer(&node).ok_or("unknown computer")?;
                let key = peer_key(&peer)?;
                let hello = open(&node, &key, &n, &c)?;
                hello.validate(&peer.node_id, &key, now_ms())?;
                let h = hello.payload.get("hello").ok_or("expected hello")?;
                if h.get("challenge").and_then(Value::as_str) != Some(nonce.as_str()) {
                    return Err("stale hello".into());
                }
                let their_nonce = h.get("nonce").and_then(Value::as_str).unwrap_or_default().to_string();
                self.learn_addrs(&peer.node_id, h, Some(from));
                let (me, _) = self.me();
                let reply = self.hello(&me, &key, &their_nonce, "")?;
                let line = seal(&me, &key, &reply)?;
                w.write_all(format!("{line}\n").as_bytes()).await.map_err(|e| e.to_string())?;
                self.session(peer, key, r, w, from.to_string(), None, token).await;
                Ok(())
            }
            _ => Err("unexpected first frame".into()),
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn pair_as_host(
        &self,
        mut r: BufReader<tokio::net::tcp::OwnedReadHalf>,
        mut w: tokio::net::tcp::OwnedWriteHalf,
        from: SocketAddr,
        their_id: String,
        their_name: String,
        their_port: u16,
        their_spake: String,
    ) -> Result<(), String> {
        validate_node_id(&their_id)?;
        let code = {
            let mut pending = self.0.pending.lock().unwrap();
            let p = pending.as_mut().ok_or("this computer isn't pairing right now")?;
            if p.expires_ms <= now_ms() {
                *pending = None;
                return Err("pairing code expired".into());
            }
            if p.busy {
                return Err("another pairing attempt is in progress".into());
            }
            p.busy = true;
            p.code.clone()
        };
        let result = async {
            let (my_id, my_name) = self.me();
            let (state, mine) = Spake2::<Ed25519Group>::start_symmetric(
                &Password::new(code.as_bytes()),
                &Identity::new(PAIR_ID),
            );
            let theirs = decode_hex(&their_spake).ok_or("bad pairing message")?;
            write_wire(
                &mut w,
                &Wire::PairStart { node_id: my_id.clone(), name: my_name, port: self.0.port, spake: hex(&mine) },
            )
            .await?;
            let shared = state.finish(&theirs).map_err(|_| "bad pairing message".to_string())?;
            let Wire::PairConfirm { mac } = read_wire(&mut r).await? else {
                return Err("expected confirmation".into());
            };
            let expected = confirm(&shared, &their_id, &my_id, &theirs, &mine);
            if !ct_eq(&decode_hex(&mac).unwrap_or_default(), &expected) {
                return Err("wrong pairing code".into());
            }
            write_wire(&mut w, &Wire::PairConfirm { mac: hex(&confirm(&shared, &my_id, &their_id, &mine, &theirs)) })
                .await?;
            Ok(Peer {
                key: hex(&link_key(&shared, &my_id, &their_id)),
                node_id: their_id.clone(),
                name: clean_name(&their_name),
                addrs: vec![SocketAddr::new(from.ip(), their_port).to_string()],
            })
        }
        .await;
        let mut pending = self.0.pending.lock().unwrap();
        match result {
            Ok(peer) => {
                *pending = None;
                drop(pending);
                self.remember(peer)
            }
            Err(e) => {
                if let Some(p) = pending.as_mut() {
                    p.busy = false;
                    if e == "wrong pairing code" {
                        p.failures += 1;
                        if p.failures >= PAIR_ATTEMPTS {
                            *pending = None;
                        }
                    }
                }
                Err(e)
            }
        }
    }

    async fn pair_as_joiner(&self, stream: TcpStream, code: &str) -> Result<Peer, String> {
        let (r, mut w) = stream.into_split();
        let mut r = BufReader::new(r);
        let Wire::Challenge { .. } = read_wire(&mut r).await? else {
            return Err("that address isn't a Consilium link".into());
        };
        let (my_id, my_name) = self.me();
        let (state, mine) = Spake2::<Ed25519Group>::start_symmetric(
            &Password::new(code.as_bytes()),
            &Identity::new(PAIR_ID),
        );
        write_wire(
            &mut w,
            &Wire::PairStart { node_id: my_id.clone(), name: my_name, port: self.0.port, spake: hex(&mine) },
        )
        .await?;
        let Wire::PairStart { node_id: their_id, name, spake, .. } = read_wire(&mut r)
            .await
            .map_err(|_| "the other computer isn't pairing right now, or its code expired".to_string())?
        else {
            return Err("unexpected pairing reply".into());
        };
        validate_node_id(&their_id)?;
        let theirs = decode_hex(&spake).ok_or("bad pairing message")?;
        let shared = state.finish(&theirs).map_err(|_| "bad pairing message".to_string())?;
        write_wire(&mut w, &Wire::PairConfirm { mac: hex(&confirm(&shared, &my_id, &their_id, &mine, &theirs)) })
            .await?;
        let Ok(Wire::PairConfirm { mac }) = read_wire(&mut r).await else {
            return Err("wrong pairing code".into());
        };
        if !ct_eq(&decode_hex(&mac).unwrap_or_default(), &confirm(&shared, &their_id, &my_id, &theirs, &mine)) {
            return Err("wrong pairing code".into());
        }
        Ok(Peer { key: hex(&link_key(&shared, &my_id, &their_id)), node_id: their_id, name: clean_name(&name), addrs: vec![] })
    }

    fn hello(&self, me: &str, key: &[u8], challenge: &str, nonce: &str) -> Result<PeerPacket, String> {
        let (_, name) = self.me();
        PeerPacket::signed(
            me,
            now_ms(),
            PacketKind::Heartbeat,
            self.0.seq.fetch_add(1, Ordering::SeqCst),
            json!({ "hello": { "challenge": challenge, "nonce": nonce, "name": name, "addrs": local_addrs(self.0.port) } }),
            key,
        )
    }

    fn learn_addrs(&self, node_id: &str, hello: &Value, observed: Option<SocketAddr>) {
        let mut addrs: Vec<String> = hello
            .get("addrs")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).filter_map(|s| s.parse::<SocketAddr>().ok()).map(|s| s.to_string()).take(8).collect())
            .unwrap_or_default();
        if let (Some(obs), Some(first)) = (observed, addrs.first().and_then(|a| a.parse::<SocketAddr>().ok())) {
            let seen = SocketAddr::new(obs.ip(), first.port()).to_string();
            if !addrs.contains(&seen) {
                addrs.insert(0, seen);
            }
        }
        if addrs.is_empty() {
            return;
        }
        let mut cfg = self.0.cfg.lock().unwrap();
        if let Some(p) = cfg.peers.iter_mut().find(|p| p.node_id == node_id) {
            for old in p.addrs.clone() {
                if !addrs.contains(&old) && addrs.len() < 12 {
                    addrs.push(old);
                }
            }
            if p.addrs != addrs {
                p.addrs = addrs;
                let snapshot = cfg.clone();
                let _ = self.save(&snapshot);
            }
        }
    }

    async fn supervise(self, token: CancellationToken) {
        let mut backoff: HashMap<String, (Duration, u64)> = HashMap::new();
        loop {
            tokio::select! {
                _ = token.cancelled() => return,
                _ = tokio::time::sleep(Duration::from_millis(500)) => {}
            }
            let (me, peers) = {
                let cfg = self.0.cfg.lock().unwrap();
                (cfg.node_id.clone(), cfg.peers.clone())
            };
            for peer in peers {
                let now = now_ms();
                let (connected, down_since) = {
                    let mut states = self.0.peers.lock().unwrap();
                    let s = states.entry(peer.node_id.clone()).or_default();
                    if s.tx.is_none() && s.down_since_ms == 0 {
                        s.down_since_ms = now;
                    }
                    (s.tx.is_some(), s.down_since_ms)
                };
                if connected || peer.addrs.is_empty() {
                    continue;
                }
                // The lower id dials; the higher one also dials once the link has been down a while,
                // in case only one direction gets through a firewall.
                if me > peer.node_id && now.saturating_sub(down_since) < BACKUP_DIAL_AFTER_MS {
                    continue;
                }
                let (delay, next_at) = backoff.get(&peer.node_id).copied().unwrap_or((RETRY_MIN, 0));
                if now < next_at {
                    continue;
                }
                backoff.insert(peer.node_id.clone(), ((delay * 2).min(RETRY_MAX), now + delay.as_millis() as u64));
                let me2 = self.clone();
                let t = token.clone();
                tokio::spawn(async move {
                    let _ = me2.dial(peer, t).await;
                });
            }
            // A working session resets that computer's back-off.
            let states = self.0.peers.lock().unwrap();
            backoff.retain(|id, _| !states.get(id).is_some_and(|s| s.tx.is_some()));
        }
    }

    async fn dial(self, peer: Peer, token: CancellationToken) -> Result<(), String> {
        let (stream, route, rtt) = fastest_route(&peer.addrs).await.ok_or("no route answered")?;
        let _ = stream.set_nodelay(true);
        let key = peer_key(&peer)?;
        let (r, mut w) = stream.into_split();
        let mut r = BufReader::new(r);
        let (me, _) = self.me();
        let reply = tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
            let Wire::Challenge { nonce } = read_wire(&mut r).await? else {
                return Err("expected challenge".to_string());
            };
            let mine = random_hex(16);
            let hello = self.hello(&me, &key, &nonce, &mine)?;
            w.write_all(format!("{}\n", seal(&me, &key, &hello)?).as_bytes()).await.map_err(|e| e.to_string())?;
            let Wire::Sealed { from, n, c } = read_wire(&mut r).await? else {
                return Err("expected hello".to_string());
            };
            if from != peer.node_id {
                return Err("a different computer answered".to_string());
            }
            let reply = open(&from, &key, &n, &c)?;
            reply.validate(&peer.node_id, &key, now_ms())?;
            let h = reply.payload.get("hello").cloned().ok_or("expected hello")?;
            if h.get("challenge").and_then(Value::as_str) != Some(mine.as_str()) {
                return Err("stale hello".to_string());
            }
            Ok(h)
        })
        .await
        .map_err(|_| "handshake timed out".to_string())??;
        self.learn_addrs(&peer.node_id, &reply, route.parse().ok());
        self.session(peer, key, r, w, route, Some(rtt), token).await;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn session(
        &self,
        peer: Peer,
        key: Vec<u8>,
        mut r: BufReader<tokio::net::tcp::OwnedReadHalf>,
        mut w: tokio::net::tcp::OwnedWriteHalf,
        route: String,
        rtt_ms: Option<u64>,
        token: CancellationToken,
    ) {
        let (me, _) = self.me();
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let generation = {
            let mut states = self.0.peers.lock().unwrap();
            let s = states.entry(peer.node_id.clone()).or_default();
            s.generation += 1;
            s.tx = Some(tx.clone());
            s.route = Some(route);
            s.rtt_ms = rtt_ms;
            s.last_seen_ms = now_ms();
            s.down_since_ms = 0;
            // Re-send everything not yet acknowledged, re-stamped so it is still fresh.
            for p in &s.outbox {
                if let Ok(line) = p.resigned(now_ms(), &key).and_then(|p| seal(&me, &key, &p)) {
                    let _ = tx.send(line);
                }
            }
            s.generation
        };
        let current = |link: &Link| {
            link.0.peers.lock().unwrap().get(&peer.node_id).is_some_and(|s| s.generation == generation && s.tx.is_some())
        };
        let (in_tx, mut in_rx) = mpsc::channel::<Result<Wire, String>>(64);
        let reader = tokio::spawn(async move {
            loop {
                let frame = read_wire(&mut r).await;
                let stop = frame.is_err();
                if in_tx.send(frame).await.is_err() || stop {
                    return;
                }
            }
        });
        let mut beat = tokio::time::interval(HEARTBEAT);
        loop {
            tokio::select! {
                _ = token.cancelled() => break,
                line = rx.recv() => {
                    let Some(line) = line else { break };
                    if w.write_all(format!("{line}\n").as_bytes()).await.is_err() { break; }
                }
                frame = in_rx.recv() => {
                    let Some(Ok(Wire::Sealed { from, n, c })) = frame else { break };
                    if from != peer.node_id { break; }
                    let Ok(packet) = open(&from, &key, &n, &c) else { break };
                    if packet.validate(&peer.node_id, &key, now_ms()).is_err() { continue; }
                    if let Some(s) = self.0.peers.lock().unwrap().get_mut(&peer.node_id) {
                        s.last_seen_ms = now_ms();
                    }
                    self.receive(&me, &key, &peer.node_id, packet, &tx);
                }
                _ = beat.tick() => {
                    if !current(self) { break; }
                    let silent = self.0.peers.lock().unwrap().get(&peer.node_id).map_or(0, |s| now_ms().saturating_sub(s.last_seen_ms));
                    if silent > DEAD_AFTER_MS { break; }
                    let ping = PeerPacket::signed(&me, now_ms(), PacketKind::Heartbeat, self.0.seq.fetch_add(1, Ordering::SeqCst), json!({ "ping": now_ms() }), &key);
                    if let Ok(line) = ping.and_then(|p| seal(&me, &key, &p)) {
                        if w.write_all(format!("{line}\n").as_bytes()).await.is_err() { break; }
                    }
                }
            }
        }
        reader.abort();
        let mut states = self.0.peers.lock().unwrap();
        if let Some(s) = states.get_mut(&peer.node_id) {
            if s.generation == generation {
                s.tx = None;
                s.down_since_ms = now_ms();
            }
        }
    }

    fn receive(&self, me: &str, key: &[u8], from: &str, packet: PeerPacket, tx: &mpsc::UnboundedSender<String>) {
        match packet.kind {
            PacketKind::Heartbeat => {}
            PacketKind::Acknowledgement => {
                if let Some(id) = packet.payload.get("ack").and_then(Value::as_str) {
                    if let Some(s) = self.0.peers.lock().unwrap().get_mut(from) {
                        s.outbox.retain(|p| p.id != id);
                    }
                }
            }
            _ => {
                let deliver = match self.0.ledger.begin(&packet) {
                    Ok(ReplayDisposition::Duplicate) => false,
                    Ok(_) => true,
                    Err(_) => return,
                };
                // Not acknowledged on failure: the sender keeps it queued and retries.
                if deliver && ((self.0.handler)(&packet).is_err() || self.0.ledger.commit(&packet).is_err()) {
                    return;
                }
                let ack = PeerPacket::signed(me, now_ms(), PacketKind::Acknowledgement, self.0.seq.fetch_add(1, Ordering::SeqCst), json!({ "ack": packet.id }), key);
                if let Ok(line) = ack.and_then(|p| seal(me, key, &p)) {
                    let _ = tx.send(line);
                }
            }
        }
    }
}

/// Tries every known address at once and keeps whichever connects first.
async fn fastest_route(addrs: &[String]) -> Option<(TcpStream, String, u64)> {
    let (tx, mut rx) = mpsc::channel(addrs.len().max(1));
    for addr in addrs.iter().take(12).cloned() {
        let tx = tx.clone();
        tokio::spawn(async move {
            let started = std::time::Instant::now();
            if let Ok(Ok(s)) = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(&addr)).await {
                let _ = tx.send((s, addr, started.elapsed().as_millis() as u64)).await;
            }
        });
    }
    drop(tx);
    rx.recv().await
}

// ---- framing, crypto and helpers --------------------------------------------------------------

async fn read_line<R: AsyncBufRead + Unpin>(r: &mut R) -> Result<String, String> {
    let mut buf = Vec::new();
    loop {
        let avail = r.fill_buf().await.map_err(|e| e.to_string())?;
        if avail.is_empty() {
            return Err("connection closed".into());
        }
        if let Some(i) = avail.iter().position(|&b| b == b'\n') {
            buf.extend_from_slice(&avail[..i]);
            r.consume(i + 1);
            break;
        }
        let n = avail.len();
        buf.extend_from_slice(avail);
        r.consume(n);
        if buf.len() > LINE_LIMIT {
            return Err("frame too large".into());
        }
    }
    if buf.len() > LINE_LIMIT {
        return Err("frame too large".into());
    }
    String::from_utf8(buf).map_err(|_| "frame isn't text".into())
}

async fn read_wire<R: AsyncBufRead + Unpin>(r: &mut R) -> Result<Wire, String> {
    serde_json::from_str(&read_line(r).await?).map_err(|_| "malformed frame".into())
}

async fn write_wire<W: AsyncWriteExt + Unpin>(w: &mut W, wire: &Wire) -> Result<(), String> {
    let mut line = serde_json::to_string(wire).map_err(|e| e.to_string())?;
    line.push('\n');
    w.write_all(line.as_bytes()).await.map_err(|e| e.to_string())
}

fn seal_key(key: &[u8]) -> Vec<u8> {
    hmac(key, b"consilium-link-seal-v1")
}

fn seal(from: &str, key: &[u8], packet: &PeerPacket) -> Result<String, String> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(&seal_key(key)));
    let nonce_bytes = &uuid::Uuid::new_v4().into_bytes()[..12];
    let plain = serde_json::to_vec(packet).map_err(|e| e.to_string())?;
    let c = cipher
        .encrypt(Nonce::from_slice(nonce_bytes), Payload { msg: &plain, aad: from.as_bytes() })
        .map_err(|_| "couldn't seal frame".to_string())?;
    serde_json::to_string(&Wire::Sealed { from: from.to_string(), n: hex(nonce_bytes), c: hex(&c) })
        .map_err(|e| e.to_string())
}

fn open(from: &str, key: &[u8], n: &str, c: &str) -> Result<PeerPacket, String> {
    let nonce = decode_hex(n).filter(|n| n.len() == 12).ok_or("bad frame nonce")?;
    let c = decode_hex(c).ok_or("bad frame")?;
    let cipher = ChaCha20Poly1305::new(Key::from_slice(&seal_key(key)));
    let plain = cipher
        .decrypt(Nonce::from_slice(&nonce), Payload { msg: &c, aad: from.as_bytes() })
        .map_err(|_| "frame failed authentication".to_string())?;
    serde_json::from_slice(&plain).map_err(|_| "malformed packet".into())
}

fn hmac(key: &[u8], msg: &[u8]) -> Vec<u8> {
    let mut m = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("hmac takes any key");
    m.update(msg);
    m.finalize().into_bytes().to_vec()
}

fn confirm(shared: &[u8], sender: &str, receiver: &str, sender_msg: &[u8], receiver_msg: &[u8]) -> Vec<u8> {
    let mut t = b"confirm|".to_vec();
    for part in [sender.as_bytes(), receiver.as_bytes(), sender_msg, receiver_msg] {
        t.extend_from_slice(&(part.len() as u32).to_be_bytes());
        t.extend_from_slice(part);
    }
    hmac(shared, &t)
}

fn link_key(shared: &[u8], a: &str, b: &str) -> Vec<u8> {
    let (lo, hi) = if a < b { (a, b) } else { (b, a) };
    hmac(shared, format!("consilium-link-key-v1|{lo}|{hi}").as_bytes())
}

fn peer_key(peer: &Peer) -> Result<Vec<u8>, String> {
    decode_hex(&peer.key).filter(|k| k.len() == 32).ok_or_else(|| format!("{} has a damaged key; pair it again", peer.name))
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn validate_node_id(id: &str) -> Result<(), String> {
    if !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')) {
        Ok(())
    } else {
        Err("invalid computer id".into())
    }
}

fn clean_name(name: &str) -> String {
    let n: String = name.chars().filter(|c| !c.is_control()).take(64).collect();
    if n.trim().is_empty() { "Other computer".into() } else { n.trim().to_string() }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn decode_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) || s.len() > 2 * LINE_LIMIT {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

fn random_hex(bytes: usize) -> String {
    let mut out = String::new();
    while out.len() < bytes * 2 {
        out.push_str(&uuid::Uuid::new_v4().simple().to_string());
    }
    out.truncate(bytes * 2);
    out
}

fn with_port(addr: &str, port: u16) -> String {
    if addr.parse::<SocketAddr>().is_ok() {
        addr.to_string()
    } else if addr.parse::<std::net::Ipv6Addr>().is_ok() {
        format!("[{addr}]:{port}")
    } else if addr.rsplit_once(':').is_some_and(|(_, p)| p.parse::<u16>().is_ok()) {
        addr.to_string()
    } else {
        format!("{addr}:{port}")
    }
}

/// This computer's reachable IPv4 addresses (cable, Wi-Fi, Tailscale), loopback excluded.
fn local_addrs(port: u16) -> Vec<String> {
    let mut out: Vec<String> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|i| !i.is_loopback())
        .filter_map(|i| match i.ip() {
            std::net::IpAddr::V4(v4) if !v4.is_link_local() => Some(format!("{v4}:{port}")),
            _ => None,
        })
        .collect();
    out.sort();
    out.dedup();
    out.truncate(8);
    out
}

fn computer_name() -> String {
    std::env::var("COMPUTERNAME")
        .ok()
        .or_else(|| {
            std::process::Command::new("hostname")
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
        })
        .map(|n| clean_name(n.trim().trim_end_matches(".local")))
        .unwrap_or_else(|| "This computer".into())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
    }

    fn node(dir: &Path, name: &str) -> (Link, Arc<Mutex<Vec<PeerPacket>>>) {
        let got = Arc::new(Mutex::new(Vec::new()));
        let sink = got.clone();
        let handler: Handler = Arc::new(move |p: &PeerPacket| {
            sink.lock().unwrap().push(p.clone());
            Ok(())
        });
        (Link::open(&dir.join(name), free_port(), handler).unwrap(), got)
    }

    async fn wait_for(mut f: impl FnMut() -> bool) -> bool {
        for _ in 0..200 {
            if f() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        false
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn pairs_with_code_then_delivers_once_and_acks() {
        let dir = std::env::temp_dir().join(format!("link-test-{}", uuid::Uuid::new_v4()));
        let (host, host_got) = node(&dir, "host");
        let (joiner, _) = node(&dir, "joiner");
        let code = host.begin_pairing().unwrap();
        let addr = format!("127.0.0.1:{}", host.0.port);
        let view = joiner.join(&addr, &code.code).await.unwrap();
        assert_eq!(view.node_id, host.status().node_id);
        assert!(wait_for(|| host.status().peers.len() == 1).await, "host remembers the joiner");
        // The code is single-use.
        assert!(host.status().pairing_code_expires_ms.is_none());
        // Both sides hold the same key.
        assert_eq!(host.0.cfg.lock().unwrap().peers[0].key, joiner.0.cfg.lock().unwrap().peers[0].key);

        assert!(wait_for(|| joiner.status().peers[0].connected && host.status().peers[0].connected).await, "session comes up");
        let id = joiner.send(PacketKind::Chat, json!({ "text": "hello from the PC" })).unwrap();
        assert!(wait_for(|| host_got.lock().unwrap().len() == 1).await);
        assert_eq!(host_got.lock().unwrap()[0].id, id);
        assert!(wait_for(|| joiner.status().peers[0].queued == 0).await, "acknowledged");

        // Drop the session; a re-sent duplicate is acknowledged but not delivered twice.
        let dup = host_got.lock().unwrap()[0].clone();
        joiner.0.peers.lock().unwrap().get_mut(&view.node_id).unwrap().outbox.push_back(dup);
        joiner.stop();
        assert!(wait_for(|| !host.status().peers[0].connected).await, "host notices the drop");
        joiner.start();
        assert!(wait_for(|| joiner.status().peers[0].connected && joiner.status().peers[0].queued == 0).await, "reconnects and drains");
        assert_eq!(host_got.lock().unwrap().len(), 1);
        host.stop();
        joiner.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn wrong_codes_fail_and_burn_the_code_after_three() {
        let dir = std::env::temp_dir().join(format!("link-test-{}", uuid::Uuid::new_v4()));
        let (host, _) = node(&dir, "host");
        let (joiner, _) = node(&dir, "joiner");
        let code = host.begin_pairing().unwrap();
        let wrong = if code.code == "000000" { "111111" } else { "000000" };
        let addr = format!("127.0.0.1:{}", host.0.port);
        for _ in 0..3 {
            assert!(joiner.join(&addr, wrong).await.is_err());
        }
        assert!(joiner.join(&addr, &code.code).await.is_err(), "code is gone after 3 misses");
        assert!(host.status().peers.is_empty() && joiner.status().peers.is_empty());
        host.stop();
        joiner.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sealed_frames_reject_tampering_and_wrong_keys() {
        let key = vec![7u8; 32];
        let p = PeerPacket::signed("node-a", now_ms(), PacketKind::Chat, 1, json!({"text": "hi"}), &key).unwrap();
        let Wire::Sealed { from, n, c } = serde_json::from_str(&seal("node-a", &key, &p).unwrap()).unwrap() else { panic!() };
        assert_eq!(open(&from, &key, &n, &c).unwrap(), p);
        assert!(open("node-b", &key, &n, &c).is_err());
        assert!(open(&from, &[8u8; 32], &n, &c).is_err());
        let mut bad = c.clone();
        bad.replace_range(0..2, if &c[0..2] == "00" { "01" } else { "00" });
        assert!(open(&from, &key, &n, &bad).is_err());
        assert_eq!(with_port("192.168.1.5", PORT), "192.168.1.5:8470");
        assert_eq!(with_port("pc.local:9000", PORT), "pc.local:9000");
    }
}
