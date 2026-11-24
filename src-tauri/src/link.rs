//! Direct link between this computer and the user's other paired computers (Mac, Windows, Linux).
//!
//! * Pairing: the host shows a one-time 6-digit code; the joiner types it. SPAKE2 turns the code
//!   into a shared key, so an eavesdropper learns nothing and an active attacker gets one guess per
//!   attempt. A code expires after 5 minutes or 3 wrong tries.
//! * Transport: TCP on port 8470, one JSON object per line. After pairing every frame is sealed
//!   with ChaCha20-Poly1305 and every packet is an HMAC-authenticated `PeerPacket`; sessions start
//!   with a fresh challenge in both directions, so a recorded session can't be replayed.
//! * Delivery: incoming packets go through the crash-safe `ReplayLedger`, are appended (fsynced)
//!   to `inbox.jsonl` and only then acknowledged, so nothing is lost or taken in twice.
//!   Unacknowledged outgoing packets are kept in `outbox.json`, survive a restart, and are
//!   re-sent, re-stamped, on reconnect.
//! * A whole pairing handshake must finish within `PAIR_TIMEOUT`, so a stalled stranger can't hold
//!   the code. Learned routes are limited to private-LAN / Tailscale addresses on the link port.
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
use sha2::{Digest, Sha256};
use spake2::{Ed25519Group, Identity, Password, Spake2};
use std::collections::{HashMap, VecDeque};
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpSocket, TcpStream};
use tokio::sync::{mpsc, oneshot, Notify};
use tokio_util::sync::CancellationToken;

#[path = "link_discovery.rs"]
mod discovery;

pub const PORT: u16 = 8470;
const HEARTBEAT: Duration = Duration::from_millis(1500);
const DEAD_AFTER_MS: u64 = 4_000;
const SIGNAL_REPORT_TTL: Duration = Duration::from_millis(DEAD_AFTER_MS);
const SESSION_WRITE_IDLE: Duration = Duration::from_millis(DEAD_AFTER_MS);
const RETRY_MIN: Duration = Duration::from_millis(1000);
const RETRY_MAX: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_millis(1500);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const PAIR_TIMEOUT: Duration = Duration::from_secs(10);
const PAIR_TTL_MS: u64 = 5 * 60 * 1000;
const PAIR_ATTEMPTS: u32 = 3;
const LINE_LIMIT: usize = 3 * 1024 * 1024;
const OUTBOX_LIMIT: usize = 2000;
const TRIAL_CHUNK_BYTES: usize = 8 * 1024;
const TRIAL_CHUNKS: usize = 4;
const TRIAL_INTERVAL: Duration = Duration::from_millis(75);
const BLUETOOTH_CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

/// The authenticated protocol consumes bytes independently of the physical transport.
pub trait LinkIo: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> LinkIo for T {}
pub type LinkStream = Box<dyn LinkIo>;
type LinkReadHalf = tokio::io::ReadHalf<LinkStream>;
type LinkWriteHalf = tokio::io::WriteHalf<LinkStream>;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RouteEvidence {
    Tcp { local_addr: SocketAddr, remote_addr: SocketAddr, local_interface: Option<String> },
    BluetoothRfcomm { local_adapter: String, remote_device: String, service_uuid: String, channel: u8 },
}

pub struct LinkConnection {
    pub stream: LinkStream,
    pub evidence: RouteEvidence,
}

impl LinkConnection {
    fn tcp(stream: TcpStream) -> Result<Self, String> {
        let _ = stream.set_nodelay(true);
        let local_addr = stream.local_addr().map_err(|error| error.to_string())?;
        let remote_addr = stream.peer_addr().map_err(|error| error.to_string())?;
        let local_interface = if_addrs::get_if_addrs().ok().and_then(|interfaces| interface_name(&interfaces, local_addr.ip()));
        Ok(Self { stream: Box::new(stream), evidence: RouteEvidence::Tcp { local_addr, remote_addr, local_interface } })
    }
}

impl RouteEvidence {
    fn local_addr(&self) -> Option<SocketAddr> {
        match self { Self::Tcp { local_addr, .. } => Some(*local_addr), Self::BluetoothRfcomm { .. } => None }
    }

    fn remote_addr(&self) -> Option<SocketAddr> {
        match self { Self::Tcp { remote_addr, .. } => Some(*remote_addr), Self::BluetoothRfcomm { .. } => None }
    }

    fn local_interface(&self) -> Option<String> {
        match self {
            Self::Tcp { local_interface, .. } => local_interface.clone(),
            Self::BluetoothRfcomm { local_adapter, .. } => Some(format!("Bluetooth RFCOMM ({local_adapter})")),
        }
    }

    fn local_identity(&self) -> Option<String> {
        match self {
            Self::Tcp { local_interface, .. } => local_interface.as_ref().map(|name| format!("tcp:{name}")),
            Self::BluetoothRfcomm { local_adapter, .. } => Some(format!("rfcomm:{local_adapter}")),
        }
    }

    fn remote_endpoint(&self) -> String {
        match self {
            Self::Tcp { remote_addr, .. } => remote_addr.to_string(),
            Self::BluetoothRfcomm { remote_device, .. } => format!("rfcomm://{}", remote_device.to_ascii_uppercase()),
        }
    }
}

/// Fixed service, strict address, and no persisted RFCOMM channel numbers.
pub(crate) fn bluetooth_address(endpoint: &str) -> Option<String> {
    crate::bluetooth::device_address(endpoint)
}

#[derive(Deserialize)]
pub struct TransferPath {
    #[serde(default)]
    source: Option<IpAddr>,
    endpoint: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct TransferReceipt {
    sha256: String,
    bytes: usize,
    #[serde(default)]
    receiver_ip: Option<IpAddr>,
    #[serde(default)]
    receiver_interface: Option<String>,
    #[serde(default)]
    route_evidence: Option<RouteEvidence>,
    received_ms: u64,
    clock_id: String,
}

#[derive(Debug, Serialize)]
pub struct TransferResult {
    source: Option<ProbeSource>,
    endpoint: String,
    route_evidence: Option<RouteEvidence>,
    receipts: Vec<TransferReceipt>,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct TransferTrial {
    paths: Vec<TransferResult>,
    overlap_ms: Option<u64>,
    distinct_receiver_interfaces: bool,
}

fn distinct_receiver_interfaces(paths: &[TransferResult]) -> bool {
    let mut interfaces = std::collections::HashSet::new();
    paths.len() >= 2 && paths.iter().all(|path| {
        path.receipts.len() == TRIAL_CHUNKS
            && receipt_interface(&path.receipts[0]).is_some_and(|first| {
                path.receipts.iter().all(|receipt| receipt_interface(receipt).as_ref() == Some(&first))
                    && interfaces.insert(first)
            })
    })
}

fn transfer_overlap(paths: &[TransferResult]) -> Option<u64> {
    let clock_id = &paths.first()?.receipts.first()?.clock_id;
    let mut latest_start = 0;
    let mut earliest_end = u64::MAX;
    for path in paths {
        if path.receipts.len() != TRIAL_CHUNKS || path.receipts.iter().any(|receipt| receipt.clock_id != *clock_id) {
            return None;
        }
        latest_start = latest_start.max(path.receipts.first()?.received_ms);
        earliest_end = earliest_end.min(path.receipts.last()?.received_ms);
    }
    earliest_end.checked_sub(latest_start).filter(|ms| {
        *ms > 0 && paths.len() >= 2 && paths.iter().all(|path| {
            path.receipts.iter().any(|receipt| (latest_start..=earliest_end).contains(&receipt.received_ms))
        })
    })
}

fn receipt_interface(receipt: &TransferReceipt) -> Option<String> {
    receipt.route_evidence.as_ref().and_then(RouteEvidence::local_identity)
        .or_else(|| receipt.receiver_interface.as_ref().map(|name| format!("tcp:{name}")))
}

fn transfer_receipt(payload: &str, evidence: &RouteEvidence) -> Result<TransferReceipt, String> {
    if payload.len() != TRIAL_CHUNK_BYTES || !payload.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("invalid transfer chunk".into());
    }
    static CLOCK: OnceLock<(Instant, String)> = OnceLock::new();
    let (started, clock_id) = CLOCK.get_or_init(|| (Instant::now(), random_hex(16)));
    let received_ms = started.elapsed().as_millis() as u64;
    let receiver_ip = evidence.local_addr().map(|address| address.ip().to_canonical());
    let receiver_interface = evidence.local_interface();
    Ok(TransferReceipt {
        sha256: hex(&Sha256::digest(payload.as_bytes())),
        bytes: payload.len(),
        receiver_ip,
        receiver_interface,
        route_evidence: Some(evidence.clone()),
        received_ms,
        clock_id: clock_id.clone(),
    })
}

fn receipt_matches_path(receipt: &TransferReceipt, evidence: &RouteEvidence, source: Option<&ProbeSource>) -> bool {
    match evidence {
        RouteEvidence::Tcp { local_addr, remote_addr, .. } => {
            source.is_some_and(|source| source.address.to_canonical() == local_addr.ip().to_canonical())
                && receipt.receiver_ip == Some(remote_addr.ip().to_canonical())
                && receipt.route_evidence.as_ref().is_none_or(|receiver| match receiver {
                    RouteEvidence::Tcp { local_addr, .. } => local_addr.ip().to_canonical() == remote_addr.ip().to_canonical(),
                    _ => false,
                })
        }
        RouteEvidence::BluetoothRfcomm { local_adapter: sender_adapter, remote_device: receiver_device, service_uuid, channel } => {
            source.is_none() && receipt.receiver_ip.is_none()
                && service_uuid.eq_ignore_ascii_case(crate::bluetooth::SERVICE_UUID)
                && receipt.route_evidence.as_ref().is_some_and(|receiver| match receiver {
                    RouteEvidence::BluetoothRfcomm { local_adapter, remote_device, service_uuid: receiver_uuid, channel: receiver_channel } => {
                        local_adapter.eq_ignore_ascii_case(receiver_device) && remote_device.eq_ignore_ascii_case(sender_adapter)
                            && receiver_uuid.eq_ignore_ascii_case(service_uuid) && receiver_channel == channel
                    }
                    _ => false,
                })
        }
    }
}

fn selected_endpoints(peer: &Peer, endpoints: &[String]) -> Result<Vec<String>, String> {
    if endpoints.len() > 12 || endpoints.iter().any(|endpoint| !peer.addrs.contains(endpoint)) {
        return Err("Choose saved endpoints for this linked computer".into());
    }
    Ok(peer.addrs.iter().filter(|endpoint| endpoints.is_empty() || endpoints.contains(endpoint)).take(12).cloned().collect())
}

/// Whether this computer's Signal connection is running, shared with linked computers in each
/// heartbeat so they can agree which one answers a phone message.
static SIGNAL_ON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// When the human last sent a message from this computer's own window (ms since the epoch).
static LOCAL_ACTIVE_MS: AtomicU64 = AtomicU64::new(0);

/// What this computer's AIs are doing right now, shared in each heartbeat: (turn id, label such
/// as "Claude in Plans"). Entries are removed by [`WorkingGuard`] when the answer ends.
static WORKING: Mutex<Vec<(u64, String)>> = Mutex::new(Vec::new());
static WORKING_SEQ: AtomicU64 = AtomicU64::new(1);
/// The latest git summary of the project folder an AI here worked in (see `prompt::git_summary`).
static GIT_SUMMARY: Mutex<Option<String>> = Mutex::new(None);
const STATUS_ITEMS: usize = 8;
const STATUS_CHARS: usize = 120;

/// The AI handshake. For each linked computer (node id): which of its AIs the AIs here have read
/// and then answered, as their AI -> our AIs (see `Link::acknowledge`). Shared in heartbeats, so
/// each side can show a check once AIs on both computers have heard each other.
static READ: Mutex<std::collections::BTreeMap<String, std::collections::BTreeMap<String, std::collections::BTreeSet<String>>>> =
    Mutex::new(std::collections::BTreeMap::new());
/// This computer's council members that are ready to answer, for the roll call in each heartbeat.
static COUNCIL: Mutex<Vec<String>> = Mutex::new(Vec::new());
const MAX_ROLL: usize = 16;

/// Records which council members here are ready to answer (installed, not out of usage).
pub fn set_council(ids: Vec<String>) {
    *COUNCIL.lock().unwrap() = ids.into_iter().take(MAX_ROLL).collect();
}

/// What the AIs here have read of `node_id`'s AIs, for its heartbeat: {their AI: [our AIs]}.
fn read_for(node_id: &str) -> Value {
    READ.lock().unwrap().get(node_id).map(|m| json!(m)).unwrap_or_else(|| json!({}))
}

/// Known AI ids from another computer's list, so nothing odd reaches the screen.
fn known_ais<'a>(items: impl Iterator<Item = &'a Value>) -> Vec<String> {
    let known: Vec<&str> = crate::agents::all().into_iter().map(|a| a.id).collect();
    items.filter_map(Value::as_str).filter(|s| known.contains(s)).map(str::to_string).take(MAX_ROLL).collect()
}

/// One AI's side of the handshake with a linked computer.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AiHandshake {
    pub ai: String,
    /// "here" for this computer's AI, "there" for the linked computer's.
    pub side: String,
    /// How AIs on the other computer address it, e.g. "@claude-lukes-mbp".
    pub handle: String,
    /// It has read a message from an AI on the other computer and answered.
    pub reads: bool,
    /// An AI on the other computer has read one of its messages and answered.
    pub read: bool,
}

/// Marks one AI answer as in progress on this computer until the returned guard is dropped.
pub fn working(label: &str) -> WorkingGuard {
    let id = WORKING_SEQ.fetch_add(1, Ordering::SeqCst);
    WORKING.lock().unwrap().push((id, clean_status(label)));
    WorkingGuard(id)
}

pub struct WorkingGuard(u64);

impl Drop for WorkingGuard {
    fn drop(&mut self) {
        if let Ok(mut w) = WORKING.lock() {
            w.retain(|(id, _)| *id != self.0);
        }
    }
}

/// Records the project folder's git summary to show on linked computers.
pub fn set_git_summary(summary: Option<String>) {
    *GIT_SUMMARY.lock().unwrap() = summary.map(|s| clean_status(&s));
}

/// One short line from another computer, safe to show: no control characters, bounded length.
fn clean_status(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(STATUS_CHARS).collect::<String>().trim().to_string()
}

/// Reads a peer's heartbeat activity list, keeping at most a few short, clean lines.
fn read_activity(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(clean_status).filter(|s| !s.is_empty()).take(STATUS_ITEMS).collect())
        .unwrap_or_default()
}

/// Records whether Signal is running on this computer.
pub fn set_signal_on(on: bool) {
    SIGNAL_ON.store(on, Ordering::SeqCst);
}

/// Whether the Signal session on this computer is connected right now.
pub fn signal_on() -> bool {
    SIGNAL_ON.load(Ordering::SeqCst)
}

/// Records that the human just used this computer directly.
pub fn note_local_activity() {
    LOCAL_ACTIVE_MS.store(now_ms(), Ordering::SeqCst);
}

fn local_activity() -> Vec<String> {
    WORKING.lock().unwrap().iter().map(|(_, l)| l.clone()).take(STATUS_ITEMS).collect()
}

/// One computer that can answer Signal: (node ID, last time the human used it).
pub type SignalCandidate<'a> = (&'a str, u64);

/// A unique, stable nickname for paired computer `node_id` among `peers` (node id, name).
pub fn nickname(node_id: &str, peers: &[(String, String)]) -> String {
    let clean = |n: &str| -> String {
        let n: String = n.chars().filter(|c| !c.is_control() && !matches!(c, '[' | ']' | '(' | ')')).take(40).collect();
        n.trim().to_string()
    };
    let name = peers.iter().find(|(id, _)| id == node_id).map(|(_, n)| clean(n)).unwrap_or_default();
    let tag: String = node_id.chars().filter(|c| c.is_ascii_alphanumeric()).take(4).collect();
    if name.is_empty() {
        return format!("linked computer · {tag}");
    }
    let shared = peers.iter().any(|(id, n)| id != node_id && clean(n).eq_ignore_ascii_case(&name));
    if shared { format!("{name} · {tag}") } else { name }
}

/// Whether this computer should answer a phone message, given the other linked computers that
/// are connected right now and running Signal. Every linked computer receives each message, so
/// exactly one must answer: the one the human used most recently, and on a tie the lowest node
/// ID. Every computer applies the same rule to the same heartbeat facts, so they agree.
pub fn answers_signal(me: SignalCandidate, others: &[SignalCandidate]) -> bool {
    others.iter().all(|&(id, active)| {
        if id == me.0 {
            return true;
        }
        me.1 > active || (me.1 == active && me.0 < id)
    })
}
/// The higher node id dials too once its peer has been unreachable this long.
const BACKUP_DIAL_AFTER_MS: u64 = 2_500;
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
    pub supports_lan_notes: bool,
    pub route: Option<String>,
    pub local_interface: Option<String>,
    pub route_evidence: Option<RouteEvidence>,
    pub rtt_ms: Option<u64>,
    pub last_seen_ms: Option<u64>,
    pub queued: usize,
    /// What the AIs on that computer are answering right now, e.g. "Claude in Plans".
    pub activity: Vec<String>,
    /// Its project folder's git state, e.g. "consilium-ai · main · 2 changed".
    pub git: Option<String>,
    /// The Consilium build it runs, so updates can be kept in step (see `follow`).
    pub build: Option<crate::build_id::Build>,
    /// Every AI on both computers, and whether it has heard and been heard by the other side.
    pub handshake: Vec<AiHandshake>,
    /// All known candidate routes (wired Ethernet, Wi-Fi, direct cable) for failover.
    pub addrs: Vec<String>,
}

/// The handshake between this computer (named `me`) and the linked computer `node_id` (named
/// `name`), from what each side reports. Empty while it isn't connected.
fn handshake(node_id: &str, name: &str, me: &str, s: Option<&PeerState>) -> Vec<AiHandshake> {
    let Some(s) = s else { return vec![] };
    let read = READ.lock().unwrap();
    let ours_read: std::collections::BTreeMap<String, std::collections::BTreeSet<String>> = read.get(node_id).cloned().unwrap_or_default();
    let mut out = Vec::new();
    for ai in COUNCIL.lock().unwrap().iter() {
        out.push(AiHandshake {
            ai: ai.clone(),
            side: "here".into(),
            handle: crate::prompt::handle(ai, me),
            reads: ours_read.values().any(|by| by.contains(ai)),
            read: s.read_ours.get(ai).is_some_and(|by| !by.is_empty()),
        });
    }
    for ai in &s.council {
        out.push(AiHandshake {
            ai: ai.clone(),
            side: "there".into(),
            handle: crate::prompt::handle(ai, name),
            reads: s.read_ours.values().any(|by| by.contains(ai)),
            read: ours_read.get(ai).is_some_and(|by| !by.is_empty()),
        });
    }
    out
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
    /// The build this computer runs, and what it is doing to match the others (see `follow`).
    pub build: crate::build_id::Build,
    pub follow: crate::follow::Status,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteTest {
    pub addr: String,
    pub kind: String,
    pub reachable: bool,
    pub rtt_ms: Option<u64>,
    /// Elapsed time from dial to authenticated reply on this endpoint.
    pub authenticated_ms: Option<u64>,
    pub local_ip: Option<std::net::IpAddr>,
    pub local_interface: Option<String>,
    #[serde(default)]
    pub route_evidence: Option<RouteEvidence>,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeSource {
    pub address: IpAddr,
    pub interface: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerTestReport {
    pub node_id: String,
    pub name: String,
    pub connected: bool,
    pub active_route: Option<String>,
    pub handshake_ok: bool,
    pub rtt_ms: Option<u64>,
    pub routes: Vec<RouteTest>,
    pub source: Option<ProbeSource>,
    pub queued_outgoing: usize,
    pub summary: String,
}

#[derive(Debug, Serialize)]
pub struct AiHandshakeTestReport {
    pub handshake: Vec<AiHandshake>,
    pub route: String,
    pub route_evidence: Option<RouteEvidence>,
    pub restored: Option<RouteSwitch>,
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
    session_cancel: Option<CancellationToken>,
    generation: u64,
    route: Option<String>,
    local_interface: Option<String>,
    local_addr: Option<SocketAddr>,
    route_evidence: Option<RouteEvidence>,
    rtt_ms: Option<u64>,
    last_seen_ms: u64,
    down_since_ms: u64,
    outbox: VecDeque<PeerPacket>,
    supports_lan_notes: bool,
    /// From the peer's latest heartbeat: Signal is running there, and when it was last used.
    signal_on: bool,
    /// A separate clock: unrelated authenticated traffic cannot renew Signal readiness.
    signal_report: Option<SignalReport>,
    /// Only an automatic startup diagnostic may own this child. Native publication preempts it.
    startup_stress: Option<Arc<CancellationToken>>,
    active_ms: u64,
    activity: Vec<String>,
    git: Option<String>,
    /// The build it runs (see `build_id`), from its latest heartbeat.
    build: Option<crate::build_id::Build>,
    /// Its council members ready to answer, and which of ours they have read: our AI -> theirs.
    council: Vec<String>,
    read_ours: std::collections::BTreeMap<String, Vec<String>>,
    ai_reports: u64,
    /// Authenticated packets received on the current session, used to verify a route switch.
    packets_in: u64,
    route_updates: u64,
}

impl PeerState {
    fn reset_signal_report(&mut self) {
        self.signal_report = None;
    }

    fn record_signal_report(&mut self, generation: u64, payload: &Value, received: Instant) {
        if self.generation != generation || self.tx.is_none() {
            return;
        }
        self.signal_report = match (payload.get("ping").and_then(Value::as_u64), payload.get("signal").and_then(Value::as_bool)) {
            (Some(_), Some(healthy)) => Some(SignalReport { generation, received, healthy }),
            _ => None,
        };
    }

    /// Only the current socket can prove that its route is alive.
    fn record_packet(&mut self, generation: u64, received_ms: u64) -> bool {
        if self.generation != generation || self.tx.is_none() {
            return false;
        }
        self.last_seen_ms = received_ms;
        self.packets_in += 1;
        true
    }
}

struct SignalReport {
    generation: u64,
    received: Instant,
    healthy: bool,
}

#[derive(Debug, Serialize)]
pub struct SignalRelayPeer {
    node_id: String,
    name: String,
}

/// Readiness reported by a trusted peer, not a local daemon probe or a delivery receipt.
#[derive(Debug, Serialize)]
pub struct SignalRelayStatus {
    state: &'static str,
    peer: Option<SignalRelayPeer>,
    candidate_count: usize,
    valid_for_ms: u64,
    local_direct_active: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SignalRelayReadiness<'a> {
    Unavailable,
    Available(&'a str),
    Multiple,
}

impl SignalRelayStatus {
    /// Consume immediately: the remaining report lease is measured when the snapshot is queried.
    pub(crate) fn readiness(&self) -> SignalRelayReadiness<'_> {
        if self.valid_for_ms == 0 {
            return SignalRelayReadiness::Unavailable;
        }
        match (self.state, self.peer.as_ref()) {
            ("available", Some(peer)) => SignalRelayReadiness::Available(&peer.name),
            ("multiple", _) => SignalRelayReadiness::Multiple,
            _ => SignalRelayReadiness::Unavailable,
        }
    }
}

struct Inner {
    cfg_path: PathBuf,
    outbox_path: PathBuf,
    inbox_path: PathBuf,
    cfg: Mutex<LinkConfig>,
    ledger: ReplayLedger,
    inbox_write: Mutex<()>,
    pending: Mutex<Option<Pending>>,
    peers: Mutex<HashMap<String, PeerState>>,
    handler: Handler,
    port: u16,
    seq: AtomicU64,
    running: Mutex<Option<CancellationToken>>,
    error: Mutex<Option<String>>,
    wake_dial: Notify,
    stress_test_lock: Arc<tokio::sync::Mutex<()>>,
    discovery: Mutex<Option<Arc<discovery::Discovery>>>,
}

#[derive(Clone)]
pub struct Link(Arc<Inner>);

/// Manual controls may select every transport; unattended checks stay on IP.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum StressOrigin { Manual, StartupIpOnly }

impl StressOrigin {
    fn eligible_routes(self, routes: &[String]) -> Vec<String> {
        routes.iter().filter(|address| self == Self::Manual || bluetooth_address(address).is_none()).cloned().collect()
    }
}

struct StartupStressScope { link: Link, node_id: String, stop: Arc<CancellationToken> }

impl Drop for StartupStressScope {
    fn drop(&mut self) {
        let mut states = self.link.0.peers.lock().unwrap();
        let Some(state) = states.get_mut(&self.node_id) else { return };
        if state.startup_stress.as_ref().is_some_and(|current| Arc::ptr_eq(current, &self.stop)) {
            // The restored TCP session may still use this child. Dropping
            // the diagnostic registration must not cancel that session.
            state.startup_stress = None;
        }
    }
}

/// A dropped UI request must release the selected route and keep diagnostics
/// exclusive until the asynchronous restoration attempt has finished.
struct RestoreDefaultRoute {
    link: Link,
    peer: Peer,
    token: CancellationToken,
    run: Option<tokio::sync::OwnedMutexGuard<()>>,
}

impl Drop for RestoreDefaultRoute {
    fn drop(&mut self) {
        let Some(run) = self.run.take() else { return };
        if self.token.is_cancelled() { return; }
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let (link, peer, token) = (self.link.clone(), self.peer.clone(), self.token.clone());
            runtime.spawn(async move {
                let _run = run;
                let _ = link.switch_to(&peer, &peer.addrs, None, &token).await;
            });
        }
    }
}

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
        let outbox_path = dir.join("outbox.json");
        let saved: HashMap<String, Vec<PeerPacket>> = match std::fs::read(&outbox_path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| format!("outbox.json: {e}"))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
            Err(e) => return Err(format!("outbox.json: {e}")),
        };
        let peers = saved
            .into_iter()
            .filter(|(id, _)| cfg.peers.iter().any(|p| &p.node_id == id))
            .map(|(id, q)| (id, PeerState { outbox: q.into_iter().collect(), ..Default::default() }))
            .collect();
        Ok(Link(Arc::new(Inner {
            outbox_path,
            inbox_path: dir.join("inbox.jsonl"),
            ledger: ReplayLedger::new(dir.join("sync_ledger.jsonl")),
            inbox_write: Mutex::new(()),
            cfg_path,
            cfg: Mutex::new(cfg),
            pending: Mutex::new(None),
            peers: Mutex::new(peers),
            handler,
            port,
            seq: AtomicU64::new(now_ms()),
            running: Mutex::new(None),
            error: Mutex::new(None),
            wake_dial: Notify::new(),
            stress_test_lock: Arc::new(tokio::sync::Mutex::new(())),
            discovery: Mutex::new(None),
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
        // Tests listen on loopback only: every rebuilt test binary that listened on all interfaces
        // made the macOS firewall (and Windows Defender) ask the user to allow incoming connections.
        let host = if cfg!(test) { "127.0.0.1" } else { "0.0.0.0" };
        let bound = std::net::TcpListener::bind((host, self.0.port))
            .and_then(|l| l.set_nonblocking(true).map(|_| l))
            .and_then(TcpListener::from_std);
        match bound {
            Ok(listener) => {
                *self.0.error.lock().unwrap() = None;
                let me = self.clone();
                let t = token.clone();
                tokio::spawn(async move { me.listen(listener, t).await });
                if !cfg!(test) {
                    let mut discovery = self.0.discovery.lock().unwrap();
                    if !token.is_cancelled() {
                        *discovery = discovery::Discovery::publish(self.0.port).ok().map(Arc::new);
                    }
                }
            }
            Err(e) => {
                *self.0.error.lock().unwrap() = Some(format!("couldn't open port {}: {e}", self.0.port));
            }
        }
        if !cfg!(test) {
            let (accepted, mut incoming) = mpsc::channel(8);
            let listener_token = token.clone();
            tokio::spawn(async move { let _ = crate::bluetooth::listen(accepted, listener_token).await; });
            let me = self.clone();
            let listener_token = token.clone();
            tokio::spawn(async move {
                loop {
                    let connection = tokio::select! {
                        _ = listener_token.cancelled() => break,
                        connection = incoming.recv() => match connection { Some(connection) => connection, None => break },
                    };
                    let (me, token) = (me.clone(), listener_token.clone());
                    tokio::spawn(async move { let _ = me.accept_connection(connection, token).await; });
                }
            });
        }
        let me = self.clone();
        tokio::spawn(async move { me.supervise(token).await });
    }

    pub fn stop(&self) {
        if let Some(token) = self.0.running.lock().unwrap().take() {
            token.cancel();
        }
        if let Some(discovery) = self.0.discovery.lock().unwrap().take() {
            discovery.stop();
        }
        for state in self.0.peers.lock().unwrap().values_mut() {
            state.tx = None;
            state.session_cancel = None;
            state.generation += 1;
            state.reset_signal_report();
        }
    }

    /// Automatic alerts use service discovery. A manual scan also checks for older peers.
    pub async fn discover(&self, include_legacy: bool) -> Result<Vec<String>, String> {
        let discovery = self.0.discovery.lock().unwrap().clone();
        let announced = async {
            match discovery {
                Some(discovery) => discovery.browse(self.0.port).await,
                None => Vec::new(),
            }
        };
        if !include_legacy {
            return Ok(announced.await);
        }
        let (mut addresses, legacy) = tokio::join!(announced, discover_lan(self.0.port));
        if !addresses.is_empty() {
            addresses.extend(legacy.unwrap_or_default());
        } else {
            addresses = legacy?;
        }
        addresses.sort_unstable();
        addresses.dedup();
        Ok(addresses)
    }

    /// Whether this computer should answer the phone message that just arrived. True when the
    /// link is off or no connected linked computer is running Signal.
    pub fn answers_signal(&self) -> bool {
        let cfg = self.0.cfg.lock().unwrap().clone();
        if !cfg.enabled {
            return true;
        }
        let peers = self.0.peers.lock().unwrap();
        let fresh = now_ms().saturating_sub(DEAD_AFTER_MS);
        let others: Vec<SignalCandidate> = peers
            .iter()
            .filter(|(_, s)| s.tx.is_some() && s.signal_on && s.last_seen_ms >= fresh)
            .map(|(id, s)| (id.as_str(), s.active_ms))
            .collect();
        answers_signal((cfg.node_id.as_str(), LOCAL_ACTIVE_MS.load(Ordering::SeqCst)), &others)
    }

    pub fn signal_relay_status(&self) -> SignalRelayStatus {
        self.signal_relay_status_at(Instant::now())
    }

    fn signal_relay_status_at(&self, now: Instant) -> SignalRelayStatus {
        let cfg = self.0.cfg.lock().unwrap().clone();
        let running = self.0.running.lock().unwrap().clone();
        let mut result = SignalRelayStatus {
            state: "unavailable",
            peer: None,
            candidate_count: 0,
            valid_for_ms: 0,
            local_direct_active: signal_on(),
        };
        if !cfg.enabled || running.as_ref().is_none_or(CancellationToken::is_cancelled) {
            return result;
        }
        let states = self.0.peers.lock().unwrap();
        let candidates: Vec<_> = cfg.peers.iter().filter_map(|peer| {
            let state = states.get(&peer.node_id)?;
            if state.tx.as_ref().is_none_or(mpsc::UnboundedSender::is_closed)
                || state.session_cancel.as_ref().is_none_or(CancellationToken::is_cancelled)
            {
                return None;
            }
            let report = state.signal_report.as_ref()?;
            let age = now.checked_duration_since(report.received)?;
            (report.healthy && report.generation == state.generation && age <= SIGNAL_REPORT_TTL)
                .then_some((peer, SIGNAL_REPORT_TTL.saturating_sub(age)))
        }).collect();
        result.candidate_count = candidates.len();
        if let [(peer, remaining)] = candidates.as_slice() {
            result.state = "available";
            result.peer = Some(SignalRelayPeer { node_id: peer.node_id.clone(), name: peer.name.clone() });
            result.valid_for_ms = remaining.as_millis() as u64;
        } else if candidates.len() > 1 {
            result.state = "multiple";
            result.valid_for_ms = candidates.iter().map(|(_, remaining)| remaining.as_millis() as u64).min().unwrap_or(0);
        }
        result
    }

    pub(crate) fn connected_peer_ids(&self) -> Vec<String> {
        let cfg = self.0.cfg.lock().unwrap();
        let states = self.0.peers.lock().unwrap();
        cfg.peers
            .iter()
            .filter(|peer| states.get(&peer.node_id).is_some_and(|state| state.tx.is_some()))
            .map(|peer| peer.node_id.clone())
            .collect()
    }

    pub fn status(&self) -> LinkStatus {
        let cfg = self.0.cfg.lock().unwrap().clone();
        let peers = self.0.peers.lock().unwrap();
        let pending = self.0.pending.lock().unwrap();
        LinkStatus {
            build: crate::build_id::Build::this(),
            follow: crate::follow::status(),
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
                        supports_lan_notes: s.is_some_and(|s| s.tx.is_some() && s.supports_lan_notes),
                        route: s.and_then(|s| s.tx.as_ref().and(s.route.clone())),
                        local_interface: s.filter(|s| s.tx.is_some()).and_then(|s| s.local_interface.clone()),
                        route_evidence: s.filter(|s| s.tx.is_some()).and_then(|s| s.route_evidence.clone()),
                        rtt_ms: s.filter(|s| s.tx.is_some()).and_then(|s| s.rtt_ms),
                        last_seen_ms: s.map(|s| s.last_seen_ms).filter(|&t| t > 0),
                        queued: s.map_or(0, |s| s.outbox.len()),
                        // Only while connected: an old heartbeat's activity would be stale.
                        activity: s.filter(|s| s.tx.is_some()).map(|s| s.activity.clone()).unwrap_or_default(),
                        git: s.and_then(|s| s.git.clone()),
                        build: s.and_then(|s| s.build.clone()),
                        handshake: handshake(&p.node_id, &p.name, &cfg.name, s.filter(|s| s.tx.is_some())),
                        addrs: p.addrs.clone(),
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
        let mut states = self.0.peers.lock().unwrap();
        if let Some(mut s) = states.remove(node_id) {
            s.tx = None;
        }
        self.persist_outbox(&states)
    }

    /// Registers an IP endpoint or the fixed native RFCOMM service for a paired computer.
    pub fn add_peer_route(&self, node_id: &str, address: &str) -> Result<Vec<String>, String> {
        let address = address.trim();
        let addr = if let Some(address) = bluetooth_address(address) {
            format!("rfcomm://{address}")
        } else {
            let addr = with_port(address, PORT);
            let parsed = addr.parse::<SocketAddr>().map_err(|e| format!("invalid address: {e}"))?;
            if !route_ip_allowed(parsed.ip()) {
                return Err("only local LAN, direct cable/Bluetooth, or Tailscale addresses are allowed".into());
            }
            addr
        };
        let mut cfg = self.0.cfg.lock().unwrap();
        let peer = cfg
            .peers
            .iter_mut()
            .find(|p| p.node_id == node_id)
            .ok_or_else(|| format!("computer {node_id} not found"))?;
        let should_save = if !peer.addrs.contains(&addr) {
            peer.addrs.push(addr);
            true
        } else {
            false
        };
        let addrs = peer.addrs.clone();
        if should_save {
            let snapshot = cfg.clone();
            self.save(&snapshot)?;
        }
        drop(cfg);
        self.0.wake_dial.notify_waiters();
        Ok(addrs)
    }

    /// Re-evaluates local interfaces, broadcasts current reachable endpoints in a heartbeat, and wakes the dialer.
    pub fn refresh_routes(&self, node_id: &str) -> Result<Vec<String>, String> {
        let (me, _) = self.me();
        let key = {
            let cfg = self.0.cfg.lock().unwrap();
            let peer = cfg
                .peers
                .iter()
                .find(|p| p.node_id == node_id)
                .ok_or_else(|| format!("computer {node_id} not found"))?;
            peer_key(peer)?
        };
        let states = self.0.peers.lock().unwrap();
        if let Some(s) = states.get(node_id) {
            if let Some(tx) = &s.tx {
                let ping = PeerPacket::signed(
                    &me,
                    now_ms(),
                    PacketKind::Heartbeat,
                    self.0.seq.fetch_add(1, Ordering::SeqCst),
                    json!({
                        "ping": now_ms(),
                        "signal": SIGNAL_ON.load(Ordering::SeqCst),
                        "active": LOCAL_ACTIVE_MS.load(Ordering::SeqCst),
                        "working": local_activity(),
                        "git": GIT_SUMMARY.lock().unwrap().clone(),
                        "build": crate::build_id::Build::this(),
                        "council": COUNCIL.lock().unwrap().clone(),
                        "read": read_for(node_id),
                        "addrs": local_addrs(self.0.port),
                    }),
                    &key,
                );
                if let Ok(line) = ping.and_then(|p| seal(&me, &key, &p)) {
                    let _ = tx.send(line);
                }
            }
        }
        drop(states);
        self.0.wake_dial.notify_waiters();
        let cfg = self.0.cfg.lock().unwrap();
        let addrs = cfg.peers.iter().find(|p| p.node_id == node_id).map(|p| p.addrs.clone()).unwrap_or_default();
        Ok(addrs)
    }

    /// The communication stress test: for each connected computer, moves the live link onto each
    /// of its routes in turn (the others are left out, as if they had failed), proves each with a
    /// packet received over it, and finally returns the link to the fastest route. A route that
    /// doesn't connect, or carries nothing, fails and the test moves on.
    pub async fn stress_test(&self, node_id: Option<&str>, sources: &[IpAddr]) -> Result<Vec<StressReport>, String> {
        self.stress_test_selected(node_id, sources, &[]).await
    }

    pub async fn stress_test_selected(&self, node_id: Option<&str>, sources: &[IpAddr], endpoints: &[String]) -> Result<Vec<StressReport>, String> {
        self.stress_test_with_origin(node_id, sources, endpoints, StressOrigin::Manual).await
    }

    pub(crate) async fn startup_stress_test(&self) -> Result<Vec<StressReport>, String> {
        self.stress_test_with_origin(None, &[], &[], StressOrigin::StartupIpOnly).await
    }

    fn startup_stress_scope(&self, node_id: &str, parent: &CancellationToken) -> Option<StartupStressScope> {
        let mut states = self.0.peers.lock().unwrap();
        let state = states.get_mut(node_id)?;
        if parent.is_cancelled() || state.tx.as_ref().is_none_or(mpsc::UnboundedSender::is_closed)
            || matches!(state.route_evidence.as_ref(), Some(RouteEvidence::BluetoothRfcomm { .. })) {
            return None;
        }
        let stop = Arc::new(parent.child_token());
        state.startup_stress = Some(stop.clone());
        Some(StartupStressScope { link: self.clone(), node_id: node_id.into(), stop })
    }

    async fn stress_test_with_origin(&self, node_id: Option<&str>, sources: &[IpAddr], endpoints: &[String], origin: StressOrigin) -> Result<Vec<StressReport>, String> {
        let _run = self.0.stress_test_lock.try_lock().map_err(|_| "A communication stress test is already running")?;
        let token = self.0.running.lock().unwrap().clone().ok_or("linking is off")?;
        if !sources.is_empty() {
            let interfaces = if_addrs::get_if_addrs().map_err(|error| error.to_string())?;
            for &source in sources {
                validate_probe_source(&interfaces, source)?;
            }
        }
        let peers = match node_id {
            Some(id) => {
                let peer = self.peer(id).ok_or("Linked computer not found")?;
                if peer.addrs.is_empty() {
                    return Err("The selected computer has no configured endpoints".into());
                }
                vec![peer]
            }
            None => self.0.cfg.lock().unwrap().peers.clone(),
        };
        let mut out = Vec::new();
        'peer: for peer in peers.into_iter().filter(|p| !p.addrs.is_empty()) {
            let connected = self.0.peers.lock().unwrap().get(&peer.node_id).is_some_and(|s| s.tx.is_some());
            if !connected {
                let computer = self.peer_name(&peer.node_id);
                out.push(StressReport { node_id: peer.node_id, computer, routes: vec![], restored: None });
                continue;
            }
            let scope = if origin == StressOrigin::StartupIpOnly {
                let Some(scope) = self.startup_stress_scope(&peer.node_id, &token) else { continue };
                Some(scope)
            } else { None };
            let operation = scope.as_ref().map_or_else(|| token.clone(), |scope| scope.stop.as_ref().clone());
            let updates = self.0.peers.lock().unwrap().get(&peer.node_id).map_or(0, |s| s.route_updates);
            // Wait briefly for the peer's next authenticated interface advertisement.
            let deadline = tokio::time::Instant::now() + HEARTBEAT + Duration::from_millis(500);
            while tokio::time::Instant::now() < deadline {
                if self.0.peers.lock().unwrap().get(&peer.node_id)
                    .is_none_or(|s| s.tx.is_none() || s.route_updates != updates)
                {
                    break;
                }
                tokio::select! {
                    _ = operation.cancelled() => {
                        if origin == StressOrigin::StartupIpOnly { continue 'peer; }
                        return Err("The direct link was turned off".into());
                    },
                    _ = tokio::time::sleep(Duration::from_millis(100)) => {}
                }
            }
            let Some(peer) = self.peer(&peer.node_id) else { continue };
            let selected = origin.eligible_routes(&selected_endpoints(&peer, endpoints)?);
            let restore_routes = origin.eligible_routes(&peer.addrs);
            if origin == StressOrigin::StartupIpOnly && (selected.is_empty() || restore_routes.is_empty()) { continue; }
            let mut routes = Vec::new();
            if sources.is_empty() {
                for addr in &selected {
                    if origin == StressOrigin::StartupIpOnly && operation.is_cancelled() { continue 'peer; }
                    routes.push(self.switch_to(&peer, std::slice::from_ref(addr), None, &operation).await);
                }
            } else {
                for &source in sources {
                    routes.push(self.switch_to(&peer, &selected, Some(source), &operation).await);
                }
            }
            // A native session that arrived meanwhile owns the live route;
            // neither the pending IP attempt nor restoration may replace it.
            if origin == StressOrigin::StartupIpOnly && operation.is_cancelled() { continue; }
            let restored = self.switch_to(&peer, &restore_routes, None, &operation).await;
            if origin == StressOrigin::StartupIpOnly && operation.is_cancelled() { continue; }
            out.push(StressReport {
                computer: self.peer_name(&peer.node_id),
                node_id: peer.node_id,
                routes,
                restored: Some(restored),
            });
        }
        Ok(out)
    }

    /// Moves the live link with `peer` onto the fastest of `addrs`, and waits for a packet over it.
    async fn switch_to(&self, peer: &Peer, addrs: &[String], source: Option<IpAddr>, token: &CancellationToken) -> RouteSwitch {
        let label = addrs.first().cloned().unwrap_or_default();
        let generation = self.0.peers.lock().unwrap().get(&peer.node_id).map_or(0, |s| s.generation);
        let started = std::time::Instant::now();
        let switch_timeout = if addrs.iter().any(|address| bluetooth_address(address).is_some()) {
            BLUETOOTH_CONNECT_TIMEOUT + HANDSHAKE_TIMEOUT + HEARTBEAT + Duration::from_secs(2)
        } else { STRESS_SWITCH_TIMEOUT };
        let attempt = token.child_token();
        let cleanup = attempt.clone().drop_guard();
        let spawn_dial = || {
            let (me, p, a, t) = (self.clone(), peer.clone(), addrs.to_vec(), attempt.clone());
            tokio::spawn(async move { me.dial_on(p, &a, source, t, None).await })
        };
        let mut dial = spawn_dial();
        let mut switched_at: Option<(String, u64, u64, u64)> = None;
        let fail = |addr: String, detail: String| RouteSwitch { kind: route_kind(&addr).into(), addr, ok: false, ms: None, requested_source: source, local_interface: None, local_addr: None, route_evidence: None, detail };
        while started.elapsed() < switch_timeout {
            tokio::select! {
                _ = attempt.cancelled() => return fail(label, "the direct link was turned off".into()),
                _ = tokio::time::sleep(Duration::from_millis(100)) => {}
            }
            let (route, packets, session_generation, interface, local_addr, route_evidence) = {
                let states = self.0.peers.lock().unwrap();
                let Some(s) = states.get(&peer.node_id) else { continue };
                // Only a session over one of the routes under test counts. The other computer may
                // dial in meanwhile, and that session's route is its own outgoing port: crediting
                // it passed a dead route, or reported the working one under the wrong address.
                if s.generation <= generation || s.tx.is_none() ||
                    source.is_some_and(|ip| s.local_addr.is_none_or(|addr| addr.ip().to_canonical() != ip.to_canonical())) {
                    (None, 0, s.generation, None, None, None)
                } else {
                    (s.route.clone().filter(|r| addrs.contains(r)), s.packets_in, s.generation, s.local_interface.clone(), s.local_addr, s.route_evidence.clone())
                }
            };
            if let Some(route) = route {
                match &switched_at {
                    Some((r, before, ms, observed_generation)) if *observed_generation == session_generation && packets > *before => {
                        let _ = cleanup.disarm();
                        return RouteSwitch {
                            kind: route_kind(r).into(),
                            addr: r.clone(),
                            ok: true,
                            ms: Some(*ms),
                            requested_source: source,
                            local_interface: interface,
                            local_addr,
                            route_evidence,
                            detail: format!("switched in {ms} ms, traffic confirmed after {} ms", started.elapsed().as_millis()),
                        };
                    }
                    Some((_, _, _, observed_generation)) if *observed_generation == session_generation => {}
                    _ => switched_at = Some((route, packets, started.elapsed().as_millis() as u64, session_generation)),
                }
            } else {
                switched_at = None;
            }
            if dial.is_finished() && switched_at.is_none() {
                match (&mut dial).await {
                    // The session came up and ended before it was seen: the other computer dialed
                    // in at the same moment and replaced it. The route answered, so dial it again.
                    Ok(Ok(())) => dial = spawn_dial(),
                    Ok(Err(e)) => return fail(label, e),
                    Err(_) => return fail(label, "the connection closed".into()),
                }
            }
        }
        // A late test dial must not replace the route selected by the next attempt.
        attempt.cancel();
        match switched_at {
            Some((r, _, _, _)) => fail(r, "connected, but nothing came back over it".into()),
            None => fail(label, "timed out".into()),
        }
    }

    /// Rechecks the live peer's council and read receipts without sending an AI prompt.
    pub async fn test_ai_handshake(&self, node_id: &str) -> Result<Vec<AiHandshake>, String> {
        let peer = self.peer(node_id).ok_or("Linked computer not found")?;
        let (_, mine) = self.me();
        let (generation, reports) = {
            let states = self.0.peers.lock().unwrap();
            let state = states.get(node_id).filter(|state| state.tx.is_some()).ok_or("The linked computer is offline")?;
            (state.generation, state.ai_reports)
        };
        let deadline = tokio::time::Instant::now() + HEARTBEAT * 2 + Duration::from_millis(500);
        loop {
            {
                let states = self.0.peers.lock().unwrap();
                let state = states.get(node_id)
                    .filter(|state| state.tx.is_some() && state.generation == generation)
                    .ok_or("The connection changed during the AI handshake test. Try again.")?;
                if state.ai_reports != reports {
                    return Ok(handshake(node_id, &peer.name, &mine, Some(state)));
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return Err("No fresh AI handshake report arrived. Check that the linked computer supports AI status.".into());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    pub async fn test_ai_handshake_selected(&self, node_id: &str, endpoint: Option<&str>) -> Result<AiHandshakeTestReport, String> {
        let run = self.0.stress_test_lock.clone().try_lock_owned().map_err(|_| "A communication test is already running")?;
        let peer = self.peer(node_id).ok_or("Linked computer not found")?;
        let token = self.0.running.lock().unwrap().clone().ok_or("linking is off")?;
        let endpoints = endpoint.map(|endpoint| selected_endpoints(&peer, &[endpoint.to_string()])).transpose()?;
        let mut run = Some(run);
        let mut restore = endpoint.map(|_| RestoreDefaultRoute { link: self.clone(), peer: peer.clone(), token: token.clone(), run: run.take() });
        let selected = if let Some(endpoint) = endpoint {
            let switched = self.switch_to(&peer, endpoints.as_ref().expect("selected endpoint validated"), None, &token).await;
            if !switched.ok {
                return Err(format!("The selected route did not confirm traffic: {}", switched.detail));
            }
            Some(endpoint)
        } else { None };
        let (generation, route, route_evidence) = {
            let states = self.0.peers.lock().unwrap();
            let state = states.get(node_id).filter(|state| state.tx.is_some()).ok_or("The linked computer is offline")?;
            let route = state.route.clone().ok_or("The active route is unavailable")?;
            if selected.is_some_and(|selected| selected != route) {
                return Err("The selected route changed before the AI handshake test".into());
            }
            (state.generation, route, state.route_evidence.clone())
        };
        let mut outcome = self.test_ai_handshake(node_id).await;
        if outcome.is_ok() && self.0.peers.lock().unwrap().get(node_id)
            .is_none_or(|state| state.tx.is_none() || state.generation != generation || state.route.as_deref() != Some(route.as_str())) {
            outcome = Err("The connection changed during the AI handshake test. Try again.".into());
        }
        let restored = if let Some(restore) = restore.as_mut() {
            let restored = self.switch_to(&peer, &peer.addrs, None, &token).await;
            // The explicit attempt finished; Drop must not start a second dial.
            let _ = restore.run.take();
            Some(restored)
        } else { None };
        Ok(AiHandshakeTestReport { handshake: outcome?, route, route_evidence, restored })
    }

    /// Sends bounded authenticated data over separately selected local and remote addresses.
    pub async fn transfer_trial(&self, node_id: &str, paths: Vec<TransferPath>) -> Result<TransferTrial, String> {
        let _run = self.0.stress_test_lock.try_lock().map_err(|_| "A communication test is already running")?;
        let token = self.0.running.lock().unwrap().clone().ok_or("linking is off")?;
        let peer = self.peer(node_id).ok_or("Linked computer not found")?;
        if !(2..=4).contains(&paths.len()) {
            return Err("Choose two to four paths to the same linked computer".into());
        }
        let interfaces = if_addrs::get_if_addrs().map_err(|error| error.to_string())?;
        let mut planned: Vec<(Option<ProbeSource>, String)> = Vec::with_capacity(paths.len());
        let mut destinations = std::collections::HashSet::new();
        for path in paths {
            let source = path.source.map(|address| validate_probe_source(&interfaces, address)).transpose()?;
            let bluetooth = bluetooth_address(&path.endpoint);
            let destination = if let Some(address) = bluetooth {
                if source.is_some() { return Err("A native Bluetooth path has no IP source address".into()); }
                format!("rfcomm:{address}")
            } else {
                if source.is_none() { return Err("Choose a local IP source for each TCP path".into()); }
                let endpoint: SocketAddr = path.endpoint.parse().map_err(|_| "Choose a saved IP or native Bluetooth endpoint")?;
                format!("tcp:{}", endpoint.ip().to_canonical())
            };
            if !peer.addrs.contains(&path.endpoint) {
                return Err("Choose a saved endpoint for this linked computer".into());
            }
            if !destinations.insert(destination)
                || planned.iter().any(|(existing, _)| match (existing, &source) {
                    (Some(existing), Some(source)) => existing.interface == source.interface,
                    (None, None) => true,
                    _ => false,
                })
            {
                return Err("Each path needs a different local adapter and peer endpoint".into());
            }
            planned.push((source, path.endpoint));
        }

        let key = peer_key(&peer)?;
        let mut running = tokio::task::JoinSet::new();
        for (index, (source, endpoint)) in planned.into_iter().enumerate() {
            let (link, peer, key) = (self.clone(), peer.clone(), key.clone());
            running.spawn(async move { (index, link.transfer_path(&peer, &key, source, endpoint).await) });
        }
        let mut results = Vec::new();
        loop {
            tokio::select! {
                _ = token.cancelled() => return Err("The direct link was turned off".into()),
                result = running.join_next() => match result {
                    Some(result) => results.push(result.map_err(|error| error.to_string())?),
                    None => break,
                }
            }
        }
        results.sort_by_key(|(index, _)| *index);
        let paths: Vec<_> = results.into_iter().map(|(_, result)| result).collect();
        Ok(TransferTrial {
            overlap_ms: transfer_overlap(&paths),
            distinct_receiver_interfaces: distinct_receiver_interfaces(&paths),
            paths,
        })
    }

    async fn transfer_path(&self, peer: &Peer, key: &[u8], source: Option<ProbeSource>, endpoint: String) -> TransferResult {
        let mut receipts: Vec<TransferReceipt> = Vec::with_capacity(TRIAL_CHUNKS);
        let mut route_evidence = None;
        let result: Result<(), String> = async {
            for index in 0..TRIAL_CHUNKS {
                if index > 0 {
                    tokio::time::sleep(TRIAL_INTERVAL).await;
                }
                let budget = if bluetooth_address(&endpoint).is_some() { BLUETOOTH_CONNECT_TIMEOUT + HANDSHAKE_TIMEOUT } else { HANDSHAKE_TIMEOUT };
                let receipt = tokio::time::timeout(budget, async {
                    let connection = connect_endpoint(&endpoint, source.as_ref().map(|source| source.address)).await
                        .map_err(|error| format!("Could not connect to the peer endpoint: {error}"))?;
                    let evidence = connection.evidence.clone();
                    route_evidence = Some(evidence.clone());
                    let chunk = random_hex(TRIAL_CHUNK_BYTES / 2);
                    let reply = self.exchange_probe(peer, key, connection, Some(&chunk)).await?;
                    let receipt: TransferReceipt = serde_json::from_value(reply["transfer_receipt"].clone())
                        .map_err(|_| "The linked computer did not return a payload receipt. Update both computers.")?;
                    if !receipt_matches_path(&receipt, &evidence, source.as_ref())
                        || receipt.bytes != chunk.len() || receipt.sha256 != hex(&Sha256::digest(chunk.as_bytes()))
                    {
                        return Err("The payload receipt did not match the requested path or checksum".into());
                    }
                    Ok::<_, String>(receipt)
                }).await
                    .unwrap_or_else(|_| Err("Payload probe timed out".into()))
                    .map_err(|error| format!("Sample {} of {TRIAL_CHUNKS}: {error}", index + 1))?;
                if receipts.last().is_some_and(|prev| prev.clock_id != receipt.clock_id || prev.received_ms > receipt.received_ms) {
                    return Err("The receiver clock changed during the test".into());
                }
                receipts.push(receipt);
            }
            Ok(())
        }.await;
        TransferResult { source, endpoint, route_evidence, receipts, error: result.err() }
    }

    /// Tests connectivity, route latency, and sync alignment for a paired computer.
    pub async fn test_peer(&self, node_id: &str, source: Option<IpAddr>) -> Result<PeerTestReport, String> {
        self.test_peer_selected(node_id, source, &[]).await
    }

    pub async fn test_peer_selected(&self, node_id: &str, source: Option<IpAddr>, endpoints: &[String]) -> Result<PeerTestReport, String> {
        let peer = self.peer(node_id).ok_or_else(|| format!("computer {node_id} not found"))?;
        let key = peer_key(&peer)?;
        let interfaces = if_addrs::get_if_addrs().unwrap_or_default();
        let selected = source.map(|address| validate_probe_source(&interfaces, address)).transpose()?;
        let mut probes = tokio::task::JoinSet::new();
        for (index, addr) in selected_endpoints(&peer, endpoints)?.into_iter()
            .filter(|address| source.is_none() || bluetooth_address(address).is_none()).enumerate() {
            let link = self.clone();
            let peer = peer.clone();
            let key = key.clone();
            probes.spawn(async move {
                let kind = route_kind(&addr).to_string();
                let started = std::time::Instant::now();
                let timeout = if bluetooth_address(&addr).is_some() { BLUETOOTH_CONNECT_TIMEOUT } else { Duration::from_secs(1) };
                let (rtt_ms, authenticated_ms, local_ip, route_evidence, status) = match tokio::time::timeout(timeout, connect_endpoint(&addr, source)).await {
                    Ok(Ok(connection)) => {
                        let elapsed = started.elapsed().as_millis() as u64;
                        let evidence = connection.evidence.clone();
                        let local_ip = evidence.local_addr().map(|address| address.ip());
                        let result = tokio::time::timeout(timeout, link.probe_route(&peer, &key, connection)).await;
                        let authenticated_ms = matches!(result, Ok(Ok(())))
                            .then(|| started.elapsed().as_millis() as u64);
                        let status = match authenticated_ms {
                            Some(total) => format!("Authenticated probe passed in {total} ms"),
                            None => format!("{} connected in {elapsed} ms · peer authentication unconfirmed", if local_ip.is_some() { "TCP" } else { "RFCOMM" }),
                        };
                        (Some(elapsed), authenticated_ms, local_ip, Some(evidence), status)
                    }
                    Ok(Err(_)) => (None, None, None, None, "Refused or unreachable".to_string()),
                    Err(_) => (None, None, None, None, "Timed out".to_string()),
                };
                (index, RouteTest { addr, kind, reachable: rtt_ms.is_some(), rtt_ms, authenticated_ms, local_ip, local_interface: route_evidence.as_ref().and_then(RouteEvidence::local_interface), route_evidence, status })
            });
        }
        let mut completed = Vec::new();
        while let Some(result) = probes.join_next().await {
            completed.push(result.map_err(|e| format!("Route check failed: {e}"))?);
        }
        completed.sort_by_key(|(index, _)| *index);
        let routes: Vec<_> = completed.into_iter().map(|(_, mut route)| {
            if route.local_interface.is_none() { route.local_interface = route.local_ip.and_then(|ip| interface_name(&interfaces, ip)); }
            route
        }).collect();

        // Use the current session after the probes, since failover might have occurred.
        let (connected, active_route, rtt_ms, queued_outgoing) = {
            let states = self.0.peers.lock().unwrap();
            let state = states.get(node_id);
            let session = state.filter(|state| state.tx.is_some());
            (
                session.is_some(),
                session.and_then(|state| state.route.clone()),
                session.and_then(|state| state.rtt_ms),
                state.map_or(0, |state| state.outbox.len()),
            )
        };

        let handshake_ok = if endpoints.is_empty() { connected }
            else { !routes.is_empty() && routes.iter().all(|route| route.authenticated_ms.is_some()) };
        let summary = if !endpoints.is_empty() {
            if handshake_ok { "The selected routes returned authenticated replies. The live session was left in place.".into() }
            else { "The selected routes did not all authenticate. An existing session on another route does not verify them.".into() }
        } else if connected {
            let latency = rtt_ms.map(|ms| format!(" · {ms} ms round trip")).unwrap_or_default();
            format!(
                "Authenticated session via {}{latency} · {} outgoing queued",
                active_route.as_deref().unwrap_or("active link"),
                queued_outgoing
            )
        } else if routes.iter().any(|r| r.reachable) {
            "Routes reachable, but link handshake is not active".to_string()
        } else {
            "No network routes reachable to peer".to_string()
        };

        Ok(PeerTestReport {
            source: selected,
            node_id: peer.node_id,
            name: peer.name,
            connected,
            active_route,
            handshake_ok,
            rtt_ms,
            routes,
            queued_outgoing,
            summary,
        })
    }

    /// Queues a packet for every paired computer; it is delivered now if connected, otherwise
    /// as soon as a route comes back. Returns the packet id.
    pub fn send(&self, kind: PacketKind, payload: Value) -> Result<String, String> {
        self.send_packet(None, false, kind, payload)
    }

    /// Queues a packet for one paired computer using the same reliable delivery as a broadcast.
    pub fn send_to(&self, node_id: &str, kind: PacketKind, payload: Value) -> Result<String, String> {
        self.send_packet(Some(node_id), false, kind, payload)
    }

    /// Queues an on-demand request only for peers connected at admission.
    pub fn send_connected(&self, kind: PacketKind, payload: Value) -> Result<String, String> {
        self.send_packet(None, true, kind, payload)
    }

    pub(crate) fn send_lan_note(&self, node_id: &str, payload: Value) -> Result<String, String> {
        let supported = self.0.peers.lock().unwrap().get(node_id)
            .is_some_and(|state| state.tx.is_some() && state.supports_lan_notes);
        if !supported {
            return Err("Choose a connected computer that supports LAN notes.".into());
        }
        self.send_packet(Some(node_id), true, PacketKind::Chat, payload)
    }

    fn send_packet(&self, recipient: Option<&str>, connected_only: bool, kind: PacketKind, payload: Value) -> Result<String, String> {
        let (me, peers) = {
            let cfg = self.0.cfg.lock().unwrap();
            let peers = cfg.peers.iter()
                .filter(|peer| recipient.is_none_or(|id| peer.node_id == id))
                .cloned()
                .collect::<Vec<_>>();
            (cfg.node_id.clone(), peers)
        };
        if peers.is_empty() {
            return Err("no paired computers".into());
        }
        let seq = self.0.seq.fetch_add(1, Ordering::SeqCst);
        let mut prepared = Vec::with_capacity(peers.len());
        for peer in &peers {
            let key = peer_key(peer)?;
            let packet = PeerPacket::signed(&me, now_ms(), kind, seq, payload.clone(), &key)?;
            let line = seal(&me, &key, &packet)?;
            prepared.push((peer.node_id.clone(), packet, line));
        }
        let mut states = self.0.peers.lock().unwrap();
        if connected_only {
            prepared.retain(|(id, _, _)| states.get(id).is_some_and(|state| state.tx.is_some()));
        }
        let first_id = prepared.first().ok_or("No paired computers are connected.")?.1.id.clone();
        if let Some((id, _)) = prepared.iter().find_map(|(id, _, _)| {
            let queued = states.get(id).map_or(0, |state| state.outbox.len());
            (queued >= OUTBOX_LIMIT).then_some((id, queued))
        }) {
            return Err(format!("the reliable queue for {id} is full; reconnect or unpair it before sending more"));
        }
        for (id, packet, _) in &prepared {
            states.entry(id.clone()).or_default().outbox.push_back(packet.clone());
        }
        // Saved before it goes on the wire, so a restart can't lose a queued message. Roll the
        // in-memory admission back if the durable write fails; the caller can then retry safely.
        if let Err(e) = self.persist_outbox(&states) {
            for (id, packet, _) in &prepared {
                if let Some(state) = states.get_mut(id) {
                    state.outbox.retain(|queued| queued.id != packet.id);
                }
            }
            return Err(e);
        }
        for (id, _, line) in prepared {
            if let Some(tx) = states.get(&id).and_then(|s| s.tx.as_ref()) {
                let _ = tx.send(line);
            }
        }
        Ok(first_id)
    }

    /// An AI here (`our_ai`) was handed a message from `their_ai` on the computer shown as
    /// `computer`, and answered: one half of their handshake.
    pub fn acknowledge(&self, computer: &str, their_ai: &str, our_ai: &str) {
        let peers: Vec<(String, String)> =
            self.0.cfg.lock().unwrap().peers.iter().map(|p| (p.node_id.clone(), p.name.clone())).collect();
        if let Some((node, _)) = peers.iter().find(|(id, _)| nickname(id, &peers) == computer) {
            READ.lock().unwrap().entry(node.clone()).or_default().entry(their_ai.to_string()).or_default().insert(our_ai.to_string());
        }
    }

    /// This computer's id among the linked computers.
    pub fn node_id(&self) -> String {
        self.0.cfg.lock().unwrap().node_id.clone()
    }

    pub fn has_peers(&self) -> bool {
        !self.0.cfg.lock().unwrap().peers.is_empty()
    }

    /// The paired computer's nickname, for labelling what it sent. Unique among the paired
    /// computers, so two PCs both called "Desktop" show as "Desktop · 1a2b" and "Desktop · 9f8e".
    pub fn peer_name(&self, node_id: &str) -> String {
        let peers: Vec<(String, String)> =
            self.0.cfg.lock().unwrap().peers.iter().map(|p| (p.node_id.clone(), p.name.clone())).collect();
        nickname(node_id, &peers)
    }

    /// Packets received from paired computers, oldest first, each id once. Durable: written
    /// before the sender was told it arrived.
    pub fn inbox(&self) -> Vec<PeerPacket> {
        let text = std::fs::read_to_string(&self.0.inbox_path).unwrap_or_default();
        let mut seen = std::collections::HashSet::new();
        text.lines()
            .filter_map(|l| serde_json::from_str::<PeerPacket>(l).ok())
            .filter(|p| seen.insert(p.id.clone()))
            .collect()
    }

    /// Keeps only the inbox packets `keep` accepts, so filed ones aren't re-read at every start.
    /// Runs under the inbox writer's lock, so a packet stored meanwhile isn't lost.
    pub fn retain_inbox(&self, keep: impl Fn(&PeerPacket) -> bool) -> Result<usize, String> {
        let _writing = self.0.inbox_write.lock().unwrap();
        let mut kept = Vec::new();
        let mut n = 0;
        for p in self.inbox().into_iter().filter(|p| keep(p)) {
            kept.extend(serde_json::to_vec(&p).map_err(|e| e.to_string())?);
            kept.push(b'\n');
            n += 1;
        }
        write_private(&self.0.inbox_path, &kept)?;
        Ok(n)
    }
    fn persist_outbox(&self, states: &HashMap<String, PeerState>) -> Result<(), String> {
        let q: HashMap<&String, Vec<&PeerPacket>> =
            states.iter().filter(|(_, s)| !s.outbox.is_empty()).map(|(id, s)| (id, s.outbox.iter().collect())).collect();
        write_private(&self.0.outbox_path, &serde_json::to_vec(&q).map_err(|e| e.to_string())?)
    }

    fn append_inbox(&self, packet: &PeerPacket) -> Result<(), String> {
        let _writing = self.0.inbox_write.lock().unwrap();
        let mut line = serde_json::to_vec(packet).map_err(|e| e.to_string())?;
        line.push(b'\n');
        let mut options = std::fs::OpenOptions::new();
        // Use ordinary read/write access rather than append-only access: Windows denies
        // `set_len` on an append-only handle when repairing an interrupted final record.
        options.create(true).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut f = open_with_retry(&options, &self.0.inbox_path)?;
        repair_partial_line(&mut f)?;
        f.seek(SeekFrom::End(0)).map_err(|e| e.to_string())?;
        f.write_all(&line).map_err(|e| e.to_string())?;
        f.sync_all().map_err(|e| e.to_string())
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
        let mut connection = LinkConnection::tcp(stream)?;
        if let RouteEvidence::Tcp { remote_addr, .. } = &mut connection.evidence { *remote_addr = from; }
        self.accept_connection(connection, token).await
    }

    async fn accept_connection(self, connection: LinkConnection, token: CancellationToken) -> Result<(), String> {
        let LinkConnection { stream, evidence } = connection;
        let from = evidence.remote_addr();
        let route = evidence.remote_endpoint();
        let (r, mut w) = tokio::io::split(stream);
        let mut r = BufReader::new(r);
        let nonce = random_hex(16);
        write_wire(&mut w, &Wire::Challenge { nonce: nonce.clone() }).await?;
        let first = tokio::time::timeout(HANDSHAKE_TIMEOUT, read_wire(&mut r))
            .await
            .map_err(|_| "handshake timed out".to_string())??;
        match first {
            Wire::PairStart { node_id, name, port, spake } => {
                let from = from.ok_or("Pair this computer through its existing trusted IP connection before adding Bluetooth")?;
                self.pair_as_host(r, w, from, node_id, name, port, spake).await
            }
            Wire::Sealed { from: node, n, c } => {
                let peer = self.peer(&node).ok_or("unknown computer")?;
                let key = peer_key(&peer)?;
                let hello = open(&node, &key, &n, &c)?;
                hello.validate(&peer.node_id, &key, now_ms())?;
                if let Some(probe) = hello.payload.get("route_probe") {
                    if probe.get("challenge").and_then(Value::as_str) != Some(nonce.as_str()) {
                        return Err("stale route probe".into());
                    }
                    let probe_nonce = probe.get("nonce").and_then(Value::as_str)
                        .filter(|nonce| nonce.len() == 32 && nonce.bytes().all(|byte| byte.is_ascii_hexdigit()))
                        .ok_or("invalid route probe nonce")?;
                    let mut payload = json!({ "route_probe_ack": probe_nonce });
                    if let Some(chunk) = probe.get("transfer_chunk") {
                        let chunk = chunk.as_str().ok_or("invalid transfer chunk")?;
                        payload["transfer_receipt"] = json!(transfer_receipt(chunk, &evidence)?);
                    }
                    let reply = self.route_probe_frame(&key, payload)?;
                    tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
                        w.write_all(reply.as_bytes()).await?;
                        w.flush().await
                    }).await
                        .map_err(|_| "route probe reply timed out".to_string())?
                        .map_err(|err| err.to_string())?;
                    return Ok(());
                }
                let h = hello.payload.get("hello").ok_or("expected hello")?;
                if h.get("challenge").and_then(Value::as_str) != Some(nonce.as_str()) {
                    return Err("stale hello".into());
                }
                let their_nonce = h.get("nonce").and_then(Value::as_str).unwrap_or_default().to_string();
                self.learn_addrs(&peer.node_id, h, from);
                let (me, _) = self.me();
                let mine = random_hex(16);
                let reply = self.hello(&me, &key, &their_nonce, &mine)?;
                let line = seal(&me, &key, &reply)?;
                w.write_all(format!("{line}\n").as_bytes()).await.map_err(|e| e.to_string())?;
                self.session(peer, key, r, w, route, evidence, None, token).await;
                Ok(())
            }
            _ => Err("unexpected first frame".into()),
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn pair_as_host(
        &self,
        mut r: BufReader<LinkReadHalf>,
        mut w: LinkWriteHalf,
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
        let handshake = async {
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
        };
        // The whole exchange is bounded, so a connection that stalls halfway can't hold the code.
        let result = tokio::time::timeout(PAIR_TIMEOUT, handshake)
            .await
            .unwrap_or_else(|_| Err("pairing timed out".into()));
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
        let stream: LinkStream = Box::new(stream);
        let (r, mut w) = tokio::io::split(stream);
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
        // Only the link port and any port the user paired through, on private-LAN or Tailscale
        // addresses: a paired computer can't point this one at arbitrary hosts or services.
        let mut ports = vec![PORT, self.0.port];
        if let Some(p) = self.0.cfg.lock().unwrap().peers.iter().find(|p| p.node_id == node_id) {
            ports.extend(p.addrs.iter().filter_map(|a| a.parse::<SocketAddr>().ok()).map(|a| a.port()));
        }
        let ok = |a: &SocketAddr| ports.contains(&a.port()) && route_ip_allowed(a.ip());
        let mut addrs: Vec<String> = hello
            .get("addrs")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).filter_map(|s| {
                bluetooth_address(s).map(|address| format!("rfcomm://{address}"))
                    .or_else(|| s.parse::<SocketAddr>().ok().filter(&ok).map(|address| address.to_string()))
            }).take(8).collect())
            .unwrap_or_default();
        if let (Some(obs), Some(first)) = (observed, addrs.first().and_then(|a| a.parse::<SocketAddr>().ok())) {
            let seen = SocketAddr::new(obs.ip(), first.port());
            if ok(&seen) && !addrs.contains(&seen.to_string()) {
                addrs.insert(0, seen.to_string());
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
        let mut pending_dials: HashMap<String, oneshot::Receiver<()>> = HashMap::new();
        loop {
            tokio::select! {
                _ = token.cancelled() => return,
                _ = tokio::time::sleep(Duration::from_millis(500)) => {}
                _ = self.0.wake_dial.notified() => {}
            }
            let (me, peers) = {
                let cfg = self.0.cfg.lock().unwrap();
                (cfg.node_id.clone(), cfg.peers.clone())
            };
            pending_dials.retain(|_, dial| {
                matches!(dial.try_recv(), Err(oneshot::error::TryRecvError::Empty))
            });
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
                if connected || peer.addrs.is_empty() || pending_dials.contains_key(&peer.node_id) {
                    continue;
                }
                // Freshly dropped links immediately clear accumulated backoff for instant failover.
                if now.saturating_sub(down_since) <= RETRY_MIN.as_millis() as u64 {
                    backoff.remove(&peer.node_id);
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
                let (pending, done) = oneshot::channel();
                pending_dials.insert(peer.node_id.clone(), done);
                let me2 = self.clone();
                let t = token.clone();
                tokio::spawn(async move {
                    let addrs = peer.addrs.clone();
                    let _ = me2.dial_on(peer, &addrs, None, t, Some(pending)).await;
                });
            }
            // A working session resets that computer's back-off.
            let states = self.0.peers.lock().unwrap();
            backoff.retain(|id, _| !states.get(id).is_some_and(|s| s.tx.is_some()));
        }
    }

    /// Connects to the peer and holds the automatic dial slot through authentication.
    async fn dial_on(
        self,
        peer: Peer,
        addrs: &[String],
        source: Option<IpAddr>,
        token: CancellationToken,
        pending: Option<oneshot::Sender<()>>,
    ) -> Result<(), String> {
        let key = peer_key(&peer)?;
        let mut remaining: Vec<_> = addrs.iter().take(12).cloned().collect();
        let mut failure = "no route answered".to_string();
        loop {
            let result = tokio::select! {
                _ = token.cancelled() => return Ok(()),
                result = fastest_route(&remaining, source) => result,
            };
            let Some((connection, route, rtt)) = result else { return Err(failure) };
            let LinkConnection { stream, evidence } = connection;
            let (r, mut w) = tokio::io::split(stream);
            let mut r = BufReader::new(r);
            let reply = tokio::select! {
                _ = token.cancelled() => return Ok(()),
                reply = self.authenticate_route(&peer, &key, &mut r, &mut w) => reply,
            };
            match reply {
                Ok(reply) => {
                    drop(pending);
                    self.learn_addrs(&peer.node_id, &reply, route.parse().ok());
                    self.session(peer, key, r, w, route, evidence, Some(rtt), token).await;
                    return Ok(());
                }
                Err(error) => {
                    failure = error;
                    remaining.retain(|addr| addr != &route);
                }
            }
        }
    }

    /// A diagnostic handshake does not replace a live session. Older peers close the probe
    /// before registering it because this first authenticated frame has no session hello.
    async fn probe_route(&self, peer: &Peer, key: &[u8], connection: LinkConnection) -> Result<(), String> {
        self.exchange_probe(peer, key, connection, None).await.map(|_| ())
    }

    async fn exchange_probe(&self, peer: &Peer, key: &[u8], connection: LinkConnection, chunk: Option<&str>) -> Result<Value, String> {
        let (r, mut w) = tokio::io::split(connection.stream);
        let mut r = BufReader::new(r);
        let Wire::Challenge { nonce } = read_wire(&mut r).await
            .map_err(|error| format!("Could not read the peer challenge: {error}"))? else {
            return Err("expected challenge".into());
        };
        let mine = random_hex(16);
        let mut payload = json!({ "route_probe": { "challenge": nonce, "nonce": mine } });
        if let Some(chunk) = chunk {
            payload["route_probe"]["transfer_chunk"] = json!(chunk);
        }
        let request = self.route_probe_frame(key, payload)?;
        w.write_all(request.as_bytes()).await.map_err(|error| format!("Could not send the probe: {error}"))?;
        let Wire::Sealed { from, n, c } = read_wire(&mut r).await
            .map_err(|error| format!("Could not read the authenticated probe reply: {error}"))? else {
            return Err("expected route probe reply".into());
        };
        if from != peer.node_id {
            return Err("a different computer answered".into());
        }
        let reply = open(&from, key, &n, &c)?;
        reply.validate(&peer.node_id, key, now_ms())?;
        if reply.payload.get("route_probe_ack").and_then(Value::as_str) != Some(mine.as_str()) {
            return Err("stale route probe reply".into());
        }
        Ok(reply.payload)
    }

    fn route_probe_frame(&self, key: &[u8], payload: Value) -> Result<String, String> {
        let (me, _) = self.me();
        let packet = PeerPacket::signed(
            &me, now_ms(), PacketKind::Heartbeat, self.0.seq.fetch_add(1, Ordering::SeqCst), payload, key,
        )?;
        let mut line = seal(&me, key, &packet)?;
        line.push('\n');
        Ok(line)
    }

    async fn authenticate_route(
        &self,
        peer: &Peer,
        key: &[u8],
        r: &mut BufReader<LinkReadHalf>,
        w: &mut LinkWriteHalf,
    ) -> Result<Value, String> {
        let (me, _) = self.me();
        tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
            let Wire::Challenge { nonce } = read_wire(r).await? else {
                return Err("expected challenge".to_string());
            };
            let mine = random_hex(16);
            let hello = self.hello(&me, key, &nonce, &mine)?;
            w.write_all(format!("{}\n", seal(&me, key, &hello)?).as_bytes()).await.map_err(|e| e.to_string())?;
            let Wire::Sealed { from, n, c } = read_wire(r).await? else {
                return Err("expected hello".to_string());
            };
            if from != peer.node_id {
                return Err("a different computer answered".to_string());
            }
            let reply = open(&from, key, &n, &c)?;
            reply.validate(&peer.node_id, key, now_ms())?;
            let h = reply.payload.get("hello").cloned().ok_or("expected hello")?;
            if h.get("challenge").and_then(Value::as_str) != Some(mine.as_str()) {
                return Err("stale hello".to_string());
            }
            Ok(h)
        })
        .await
        .map_err(|_| "handshake timed out".to_string())?
    }

    #[allow(clippy::too_many_arguments)]
    async fn session(
        &self,
        peer: Peer,
        key: Vec<u8>,
        mut r: BufReader<LinkReadHalf>,
        mut w: LinkWriteHalf,
        route: String,
        evidence: RouteEvidence,
        rtt_ms: Option<u64>,
        token: CancellationToken,
    ) {
        let (me, _) = self.me();
        let local_addr = evidence.local_addr();
        let local_interface = evidence.local_interface();
        // Byte activity only keeps a native frame's idle watchdog alive. It
        // never updates authenticated packet timestamps or diagnostic counters.
        let read_progress = matches!(&evidence, RouteEvidence::BluetoothRfcomm { .. })
            .then(|| Arc::new(NativeReadProgress::new(Instant::now())));
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let token = token.child_token();
        let generation = {
            let mut states = self.0.peers.lock().unwrap();
            if token.is_cancelled() {
                return;
            }
            let s = states.entry(peer.node_id.clone()).or_default();
            if matches!(&evidence, RouteEvidence::BluetoothRfcomm { .. }) {
                // Under the publication lock: a late startup TCP session must
                // observe cancellation before it can replace this native one.
                if let Some(startup) = s.startup_stress.take() { startup.cancel(); }
            }
            if let Some(previous) = s.session_cancel.replace(token.clone()) {
                previous.cancel();
            }
            s.generation += 1;
            s.tx = Some(tx.clone());
            s.route = Some(route);
            s.local_interface = local_interface;
            s.local_addr = local_addr;
            s.route_evidence = Some(evidence);
            s.rtt_ms = rtt_ms;
            s.last_seen_ms = now_ms();
            s.down_since_ms = 0;
            s.packets_in = 0;
            s.supports_lan_notes = false;
            s.reset_signal_report();
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
        let reader_progress = read_progress.clone();
        let reader = tokio::spawn(async move {
            loop {
                let frame = if let Some(progress) = &reader_progress {
                    read_native_wire(&mut r, progress).await
                } else { read_wire(&mut r).await };
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
                    if !write_session_frame(&mut w, line, &token, SESSION_WRITE_IDLE).await { break; }
                }
                frame = in_rx.recv() => {
                    let Some(Ok(Wire::Sealed { from, n, c })) = frame else { break };
                    if from != peer.node_id { break; }
                    let Ok(packet) = open(&from, &key, &n, &c) else { break };
                    if packet.validate(&peer.node_id, &key, now_ms()).is_err() { continue; }
                    let active = self.0.peers.lock().unwrap().get_mut(&peer.node_id)
                        .is_some_and(|state| state.record_packet(generation, now_ms()));
                    if !active { break; }
                    self.receive(&me, &key, &peer.node_id, generation, packet, &tx);
                }
                _ = beat.tick() => {
                    if !current(self) { break; }
                    let silent = self.0.peers.lock().unwrap().get(&peer.node_id).map_or(0, |s| now_ms().saturating_sub(s.last_seen_ms));
                    // A bounded queue may already hold frames received while
                    // a large outgoing write was in progress. Validate them
                    // before declaring the native reader silent.
                    let native_active = read_progress.as_ref().is_some_and(|progress|
                        progress.recent(Instant::now()) || !in_rx.is_empty());
                    if silent > DEAD_AFTER_MS && !native_active { break; }
                    let ping = PeerPacket::signed(
                        &me,
                        now_ms(),
                        PacketKind::Heartbeat,
                        self.0.seq.fetch_add(1, Ordering::SeqCst),
                        json!({
                            "ping": now_ms(),
                            "signal": SIGNAL_ON.load(Ordering::SeqCst),
                            "active": LOCAL_ACTIVE_MS.load(Ordering::SeqCst),
                            "working": local_activity(),
                            "git": GIT_SUMMARY.lock().unwrap().clone(),
                            "build": crate::build_id::Build::this(),
                            "lan_notes": true,
                            "council": COUNCIL.lock().unwrap().clone(),
                            "read": read_for(&peer.node_id),
                            "addrs": local_addrs(self.0.port),
                        }),
                        &key,
                    );
                    if let Ok(line) = ping.and_then(|p| seal(&me, &key, &p)) {
                        if !write_session_frame(&mut w, line, &token, SESSION_WRITE_IDLE).await { break; }
                    }
                }
            }
        }
        token.cancel();
        reader.abort();
        let _ = reader.await;
        let mut states = self.0.peers.lock().unwrap();
        if let Some(s) = states.get_mut(&peer.node_id) {
            if s.generation == generation {
                s.tx = None;
                s.session_cancel = None;
                s.down_since_ms = now_ms();
            }
        }
        drop(states);
        self.0.wake_dial.notify_waiters();
    }

    fn receive(&self, me: &str, key: &[u8], from: &str, generation: u64, packet: PeerPacket, tx: &mpsc::UnboundedSender<String>) {
        match packet.kind {
            PacketKind::Heartbeat => {
                if let Some(state) = self.0.peers.lock().unwrap().get_mut(from) {
                    state.record_signal_report(generation, &packet.payload, Instant::now());
                }
                if packet.payload.get("ping").is_some() {
                    if let Some(s) = self.0.peers.lock().unwrap().get_mut(from).filter(|state| state.generation == generation) {
                        s.signal_on = packet.payload.get("signal").and_then(Value::as_bool).unwrap_or(false);
                        s.active_ms = packet.payload.get("active").and_then(Value::as_u64).unwrap_or(0);
                        s.activity = read_activity(packet.payload.get("working"));
                        s.git = packet.payload.get("git").and_then(Value::as_str).map(clean_status).filter(|g| !g.is_empty());
                        s.build = packet.payload.get("build").and_then(crate::build_id::Build::from_peer);
                        s.supports_lan_notes = packet.payload.get("lan_notes").and_then(Value::as_bool) == Some(true);
                        s.council = packet.payload.get("council").and_then(Value::as_array).map(|a| known_ais(a.iter())).unwrap_or_default();
                        s.read_ours = packet
                            .payload
                            .get("read")
                            .and_then(Value::as_object)
                            .map(|m| {
                                m.iter()
                                    .filter(|(ours, _)| crate::agents::all().iter().any(|a| a.id == ours.as_str()))
                                    .take(MAX_ROLL)
                                    .map(|(ours, theirs)| (ours.clone(), theirs.as_array().map(|a| known_ais(a.iter())).unwrap_or_default()))
                                    .collect()
                            })
                            .unwrap_or_default();
                        if packet.payload.get("council").is_some_and(Value::is_array)
                            && packet.payload.get("read").is_some_and(Value::is_object)
                        {
                            s.ai_reports = s.ai_reports.wrapping_add(1);
                        }
                    }
                    if packet.payload.get("addrs").is_some() {
                        self.learn_addrs(from, &packet.payload, None);
                        if let Some(state) = self.0.peers.lock().unwrap().get_mut(from) {
                            state.route_updates = state.route_updates.wrapping_add(1);
                        }
                    }
                }
            }
            PacketKind::Acknowledgement => {
                if let Some(id) = packet.payload.get("ack").and_then(Value::as_str) {
                    let mut states = self.0.peers.lock().unwrap();
                    if let Some(s) = states.get_mut(from) {
                        let before = s.outbox.len();
                        s.outbox.retain(|p| p.id != id);
                        if s.outbox.len() != before {
                            let _ = self.persist_outbox(&states);
                        }
                    }
                }
            }
            _ => {
                let deliver = match self.0.ledger.begin(&packet) {
                    Ok(ReplayDisposition::Duplicate) => false,
                    Ok(_) => true,
                    Err(_) => return,
                };
                // Stored durably before the ack; on failure the sender keeps it queued and retries.
                // A crash between storing and committing stores it again, which `inbox` dedupes.
                if deliver {
                    if self.append_inbox(&packet).is_err() {
                        return;
                    }
                    match self.0.ledger.commit(&packet) {
                        // Two active routes can both reserve the same packet before either commits.
                        // The first successful commit owns the notification.
                        Ok(true) => { let _ = (self.0.handler)(&packet); }
                        Ok(false) => {}
                        Err(_) => return,
                    }
                }
                let ack = PeerPacket::signed(me, now_ms(), PacketKind::Acknowledgement, self.0.seq.fetch_add(1, Ordering::SeqCst), json!({ "ack": packet.id }), key);
                if let Ok(line) = ack.and_then(|p| seal(me, key, &p)) {
                    let _ = tx.send(line);
                }
            }
        }
    }
}

async fn connect_with_source(endpoint: &str, source: Option<IpAddr>) -> std::io::Result<TcpStream> {
    let Some(source) = source else { return TcpStream::connect(endpoint).await };
    let target = tokio::net::lookup_host(endpoint).await?
        .find(|target| target.is_ipv4() == source.is_ipv4())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::AddrNotAvailable, "no endpoint in the selected address family"))?;
    let socket = if source.is_ipv4() { TcpSocket::new_v4()? } else { TcpSocket::new_v6()? };
    socket.bind(SocketAddr::new(source, 0))?;
    socket.connect(target).await
}

fn connect_timeout(endpoint: &str) -> Duration {
    if bluetooth_address(endpoint).is_some() { BLUETOOTH_CONNECT_TIMEOUT } else { CONNECT_TIMEOUT }
}

async fn connect_endpoint(endpoint: &str, source: Option<IpAddr>) -> Result<LinkConnection, String> {
    if let Some(address) = bluetooth_address(endpoint) {
        if source.is_some() { return Err("Native Bluetooth has no IP source address".into()); }
        let token = CancellationToken::new();
        // Aborting a losing route race must cancel native connection attempts too.
        let guard = token.clone().drop_guard();
        let result = crate::bluetooth::connect(&address, token).await;
        if result.is_ok() { let _ = guard.disarm(); }
        result
    } else {
        let stream = connect_with_source(endpoint, source).await.map_err(|error| error.to_string())?;
        LinkConnection::tcp(stream)
    }
}

fn validate_probe_source(interfaces: &[if_addrs::Interface], address: IpAddr) -> Result<ProbeSource, &'static str> {
    interface_name(interfaces, address)
        .filter(|_| route_ip_allowed(address))
        .map(|interface| ProbeSource { address, interface })
        .ok_or("The selected source address is not available on a unique local interface")
}

/// Usable source addresses with an unambiguous local interface, for the connection diagnostic.
pub fn probe_sources() -> Result<Vec<ProbeSource>, String> {
    let interfaces = if_addrs::get_if_addrs().map_err(|error| error.to_string())?;
    let mut sources: Vec<_> = interfaces.iter().map(|interface| interface.ip().to_canonical())
        .filter(|address| !address.is_loopback() && route_ip_allowed(*address))
        .filter_map(|address| interface_name(&interfaces, address).map(|interface| ProbeSource { address, interface }))
        .collect();
    sources.sort_by(|a, b| (&a.interface, a.address).cmp(&(&b.interface, b.address)));
    sources.dedup_by(|a, b| a.address == b.address);
    Ok(sources)
}

fn interface_name(interfaces: &[if_addrs::Interface], ip: std::net::IpAddr) -> Option<String> {
    let mut names = interfaces.iter()
        .filter(|interface| interface.ip().to_canonical() == ip.to_canonical())
        .map(|interface| &interface.name);
    let first = names.next()?;
    names.all(|name| name == first).then(|| first.clone())
}

fn route_kind(addr: &str) -> &'static str {
    if bluetooth_address(addr).is_some() { return "Bluetooth RFCOMM"; }
    match addr.parse::<SocketAddr>().map(|socket| socket.ip().to_canonical()) {
        Ok(ip) if ip.is_loopback() => "Loopback",
        Ok(std::net::IpAddr::V4(ip)) if ip.is_link_local() => "Link-local",
        Ok(std::net::IpAddr::V4(ip)) if ip.octets()[0] == 100 && ip.octets()[1] & 0xc0 == 64 => "Shared address space",
        Ok(_) => "IP route",
        Err(_) => "Unclassified endpoint",
    }
}

/// How long the stress test waits for the link to move onto a route and carry a packet.
const STRESS_SWITCH_TIMEOUT: Duration = Duration::from_secs(12);

/// One route in the communication stress test.
#[derive(Debug, Clone, Serialize)]
pub struct RouteSwitch {
    pub addr: String,
    pub kind: String,
    pub ok: bool,
    /// How long the switch took.
    pub ms: Option<u64>,
    pub requested_source: Option<IpAddr>,
    pub local_interface: Option<String>,
    pub local_addr: Option<SocketAddr>,
    pub route_evidence: Option<RouteEvidence>,
    pub detail: String,
}

/// The stress test with one linked computer: each route, and where the link ended up.
#[derive(Debug, Clone, Serialize)]
pub struct StressReport {
    pub node_id: String,
    pub computer: String,
    pub routes: Vec<RouteSwitch>,
    /// The final reconnect attempt, absent when no routes were tested.
    pub restored: Option<RouteSwitch>,
}

impl StressReport {
    /// The result as a status note for the conversation.
    pub fn summary(&self, when: &str) -> String {
        if self.routes.is_empty() {
            return format!("Communication stress test ({when}) with {}: not connected right now, so nothing was tested.", self.computer);
        }
        let ok = self.routes.iter().filter(|r| r.ok).count();
        let subject = if self.routes.iter().any(|r| r.requested_source.is_some()) { "local sources" } else { "configured endpoints" };
        let mut s = format!(
            "Communication stress test ({when}) with {}: {ok} of {} {subject} carried authenticated traffic during individual connection attempts.",
            self.computer,
            self.routes.len()
        );
        for r in &self.routes {
            s.push_str(&format!("\n{} {} {}: {}", if r.ok { "✓" } else { "✗" }, r.kind, r.addr, r.detail));
            if let Some(address) = r.requested_source {
                s.push_str(&format!(", requested source {address}"));
            }
            if let Some(interface) = &r.local_interface {
                s.push_str(&format!(", local interface {interface}"));
            }
        }
        match &self.restored {
            Some(r) if r.ok => {
                s.push_str(&format!("\nReconnected through {}: {}", r.addr, r.detail));
                if let Some(interface) = &r.local_interface {
                    s.push_str(&format!(", local interface {interface}"));
                }
            }
            Some(r) => s.push_str(&format!("\nReconnection unconfirmed: {}", r.detail)),
            None => s.push_str("\nThe test did not confirm traffic after attempting to reconnect."),
        }
        s
    }
}

async fn fastest_route(addrs: &[String], source: Option<IpAddr>) -> Option<(LinkConnection, String, u64)> {
    let mut probes = tokio::task::JoinSet::new();
    for addr in addrs.iter().take(12).cloned() {
        probes.spawn(async move {
            let started = std::time::Instant::now();
            let connection = tokio::time::timeout(connect_timeout(&addr), connect_endpoint(&addr, source)).await.ok()?.ok()?;
            Some((connection, addr, started.elapsed().as_millis() as u64))
        });
    }
    while let Some(result) = probes.join_next().await {
        if let Ok(Some(connected)) = result {
            // Dropping the set cancels the remaining probes before route authentication.
            return Some(connected);
        }
    }
    None
}

// ---- framing, crypto and helpers --------------------------------------------------------------

/// A stalled write must leave the session so pending messages can be resent after reconnect.
async fn write_session_frame<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    mut line: String,
    token: &CancellationToken,
    idle: Duration,
) -> bool {
    line.push('\n');
    let mut remaining = line.as_bytes();
    while !remaining.is_empty() {
        let written = tokio::select! {
            biased;
            _ = token.cancelled() => return false,
            result = tokio::time::timeout(idle, writer.write(remaining)) => result,
        };
        match written {
            Ok(Ok(count)) if count > 0 => remaining = &remaining[count..],
            _ => return false,
        }
    }
    true
}


#[derive(Clone, Copy)]
struct NativeFrameBudget { idle: Duration, absolute: Duration }
const NATIVE_FRAME_BUDGET: NativeFrameBudget = NativeFrameBudget {
    idle: Duration::from_millis(DEAD_AFTER_MS), absolute: Duration::from_secs(180),
};

struct NativeReadProgress { last_byte: Mutex<Instant> }
impl NativeReadProgress {
    fn new(now: Instant) -> Self { Self { last_byte: Mutex::new(now) } }
    fn record(&self, now: Instant) { *self.last_byte.lock().unwrap_or_else(|e| e.into_inner()) = now; }
    fn recent(&self, now: Instant) -> bool {
        now.saturating_duration_since(*self.last_byte.lock().unwrap_or_else(|e| e.into_inner())) < NATIVE_FRAME_BUDGET.idle
    }
}

struct NativeFrameDeadline { started: Instant, last_byte: Instant, budget: NativeFrameBudget }
impl NativeFrameDeadline {
    fn new(now: Instant, budget: NativeFrameBudget) -> Self { Self { started: now, last_byte: now, budget } }
    fn remaining(&self, now: Instant) -> Result<Duration, String> {
        let elapsed = now.saturating_duration_since(self.started);
        let idle = now.saturating_duration_since(self.last_byte);
        if elapsed >= self.budget.absolute { return Err("Bluetooth frame exceeded its absolute deadline".into()); }
        if idle >= self.budget.idle { return Err("Bluetooth frame stopped receiving data".into()); }
        Ok((self.budget.absolute - elapsed).min(self.budget.idle - idle))
    }
    fn record(&mut self, now: Instant) -> Result<(), String> {
        self.remaining(now)?;
        self.last_byte = now;
        Ok(())
    }
}

async fn read_line<R: AsyncBufRead + Unpin>(r: &mut R) -> Result<String, String> {
    read_line_clocked(r, None, Instant::now).await
}

async fn read_line_clocked<R: AsyncBufRead + Unpin>(
    r: &mut R, native: Option<(&NativeReadProgress, NativeFrameBudget)>, now: impl Fn() -> Instant,
) -> Result<String, String> {
    let mut watchdog = native.map(|(_, budget)| NativeFrameDeadline::new(now(), budget));
    let mut buf = Vec::new();
    loop {
        let avail = if let Some(watchdog) = &watchdog {
            let wait = watchdog.remaining(now())?;
            tokio::time::timeout(wait, r.fill_buf()).await
                .map_err(|_| watchdog.remaining(now()).err().unwrap_or_else(|| "Bluetooth frame read timed out".into()))?
                .map_err(|error| error.to_string())?
        } else { r.fill_buf().await.map_err(|error| error.to_string())? };
        if avail.is_empty() {
            return Err("connection closed".into());
        }
        if let Some(watchdog) = &mut watchdog {
            let now = now();
            watchdog.record(now)?;
            if let Some((progress, _)) = native { progress.record(now); }
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

async fn read_native_wire<R: AsyncBufRead + Unpin>(r: &mut R, progress: &NativeReadProgress) -> Result<Wire, String> {
    serde_json::from_str(&read_line_clocked(r, Some((progress, NATIVE_FRAME_BUDGET)), Instant::now).await?)
        .map_err(|_| "malformed frame".into())
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

pub fn load_peer_keys(data_dir: &std::path::Path) -> std::collections::HashMap<String, Vec<u8>> {
    let cfg_path = data_dir.join("link.json");
    let mut map = std::collections::HashMap::new();
    if let Ok(bytes) = std::fs::read(&cfg_path) {
        if let Ok(cfg) = serde_json::from_slice::<LinkConfig>(&bytes) {
            for peer in cfg.peers {
                if let Ok(k) = peer_key(&peer) {
                    map.insert(peer.node_id, k);
                }
            }
        }
    }
    map
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

/// Looks for the PeerLink greeting on directly attached private IPv4 networks.
async fn discover_lan(port: u16) -> Result<Vec<String>, String> {
    use std::net::{Ipv4Addr, SocketAddrV4};

    let interfaces = if_addrs::get_if_addrs().map_err(|error| error.to_string())?;
    let own: std::collections::HashSet<_> = interfaces.iter().map(|interface| interface.ip()).collect();
    let mut candidates = Vec::new();
    for interface in interfaces {
        let if_addrs::IfAddr::V4(address) = interface.addr else { continue };
        if !(address.ip.is_private() || address.ip.is_link_local()) {
            continue;
        }
        let ip = u32::from(address.ip);
        let mask = u32::from(address.netmask);
        // Bound each interface to the surrounding 256 addresses, even on a larger LAN.
        let search_mask = mask | 0xffffff00;
        for candidate in (ip & search_mask)..=(ip | !search_mask) {
            if candidate == ip & mask || candidate == ip | !mask {
                continue;
            }
            let candidate = Ipv4Addr::from(candidate);
            if (candidate.is_private() || candidate.is_link_local()) && route_ip_allowed(candidate.into()) && !own.contains(&candidate.into()) {
                candidates.push((address.ip, SocketAddrV4::new(candidate, port)));
            }
        }
    }
    Ok(verify_discovery_candidates(candidates).await)
}

async fn verify_discovery_candidates(mut candidates: Vec<(std::net::Ipv4Addr, std::net::SocketAddrV4)>) -> Vec<String> {
    candidates.sort_unstable();
    candidates.dedup();
    let mut found = std::collections::BTreeSet::new();
    let mut tasks = tokio::task::JoinSet::new();
    let mut candidates = candidates.into_iter();
    loop {
        while tasks.len() < 48 {
            let Some((source, target)) = candidates.next() else { break };
            tasks.spawn(async move {
                tokio::time::timeout(Duration::from_millis(350), async move {
                    let socket = TcpSocket::new_v4().ok()?;
                    socket.bind(SocketAddr::new(source.into(), 0)).ok()?;
                    let stream = socket.connect(target.into()).await.ok()?;
                    peerlink_greeting(stream).await.then_some(target)
                }).await.ok().flatten()
            });
        }
        let Some(result) = tasks.join_next().await else { break };
        if let Ok(Some(target)) = result {
            found.insert(target);
        }
    }
    found.into_iter().map(|address| address.to_string()).collect()
}

async fn peerlink_greeting(stream: TcpStream) -> bool {
    use tokio::io::AsyncReadExt;
    let mut reader = BufReader::new(stream.take(256));
    let mut line = String::new();
    if reader.read_line(&mut line).await.is_err() || !line.ends_with('\n') {
        return false;
    }
    matches!(serde_json::from_str::<Wire>(&line), Ok(Wire::Challenge { nonce })
        if nonce.len() == 32 && nonce.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

/// Reachable IPv4 addresses. Tests advertise only loopback, matching their listeners.
fn local_addrs(port: u16) -> Vec<String> {
    let mut out: Vec<String> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|i| i.is_loopback() == cfg!(test))
        .filter_map(|i| match i.ip() {
            ip @ std::net::IpAddr::V4(_) if route_ip_allowed(ip) => Some(SocketAddr::new(ip, port).to_string()),
            _ => None,
        })
        .collect();
    out.sort();
    out.dedup();
    if let Some(endpoint) = crate::bluetooth::local_endpoint().filter(|endpoint| bluetooth_address(endpoint).is_some()) {
        out.insert(0, endpoint);
    }
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

/// Opens a file readable only by this user from the moment it exists.
fn open_private(path: &Path, append: bool) -> Result<std::fs::File, String> {
    let mut o = std::fs::OpenOptions::new();
    o.create(true);
    if append {
        o.append(true);
    } else {
        o.write(true).truncate(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(path).map_err(|e| e.to_string())
}

/// Atomic, fsynced, 0600 write.
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    let mut f = open_private(&tmp, false)?;
    f.write_all(bytes).and_then(|_| f.sync_all()).map_err(|e| e.to_string())?;
    drop(f);
    crate::store::replace_file(&tmp, path).map_err(|e| e.to_string())
}

fn open_with_retry(options: &std::fs::OpenOptions, path: &Path) -> Result<std::fs::File, String> {
    let mut err = String::new();
    for i in 0..10 {
        match options.open(path) {
            Ok(f) => return Ok(f),
            Err(e) => {
                err = e.to_string();
                if i < 9 {
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        }
    }
    Err(err)
}

/// A power loss can leave the final JSONL record partially written. Remove only that incomplete
/// tail before appending the sender's retry; complete earlier records remain untouched.
fn repair_partial_line(file: &mut std::fs::File) -> Result<(), String> {
    let len = file.metadata().map_err(|e| e.to_string())?.len();
    if len == 0 {
        return Ok(());
    }
    file.seek(SeekFrom::End(-1)).map_err(|e| e.to_string())?;
    let mut last = [0u8; 1];
    file.read_exact(&mut last).map_err(|e| e.to_string())?;
    if last[0] == b'\n' {
        return Ok(());
    }

    let mut end = len;
    let mut chunk = [0u8; 8192];
    while end > 0 {
        let start = end.saturating_sub(chunk.len() as u64);
        let size = (end - start) as usize;
        file.seek(SeekFrom::Start(start)).map_err(|e| e.to_string())?;
        file.read_exact(&mut chunk[..size]).map_err(|e| e.to_string())?;
        if let Some(index) = chunk[..size].iter().rposition(|&byte| byte == b'\n') {
            let target = start + index as u64 + 1;
            let _ = file.seek(SeekFrom::Start(target));
            if file.set_len(target).is_err() {
                // If the OS refuses set_len on this handle, close the partial line with \n
                // so the corrupt fragment does not bleed into the next valid JSON line.
                let _ = file.seek(SeekFrom::End(0));
                let _ = file.write_all(b"\n");
            }
            return Ok(());
        }
        end = start;
    }
    let _ = file.seek(SeekFrom::Start(0));
    if file.set_len(0).is_err() {
        let _ = file.seek(SeekFrom::End(0));
        let _ = file.write_all(b"\n");
    }
    Ok(())
}

/// Private LAN (10/8, 172.16/12, 192.168/16), Tailscale/CGNAT (100.64/10), IPv6 unique-local,
/// or loopback. Public, multicast and unspecified addresses are refused.
fn route_ip_allowed(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            let o = v4.octets();
            // Link-local 169.254.x.x is what a direct cable, Thunderbolt bridge or Bluetooth
            // network gets with no router handing out addresses (not the cloud metadata address).
            v4.is_private() || v4.is_loopback() || (v4.is_link_local() && o[2..] != [169, 254]) || (o[0] == 100 && (o[1] & 0xc0) == 64)
        }
        std::net::IpAddr::V6(v6) => v6.is_loopback() || (v6.segments()[0] & 0xfe00) == 0xfc00,
    }
}

#[cfg(test)]
#[path = "link_signal_status_tests.rs"]
mod signal_status_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn discovery_distinguishes_peerlink_greetings_from_other_services() {
        for (greeting, expected) in [
            (serde_json::to_string(&Wire::Challenge { nonce: "a".repeat(32) }).unwrap(), true),
            (serde_json::to_string(&Wire::Challenge { nonce: "b".repeat(300) }).unwrap(), false),
            ("HTTP/1.1 200 OK".to_string(), false),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let client = TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
            let (mut server, _) = listener.accept().await.unwrap();
            server.write_all(format!("{greeting}\n").as_bytes()).await.unwrap();
            assert_eq!(peerlink_greeting(client).await, expected);
        }
    }

    #[test]
    fn computer_nicknames_are_unique_and_clean() {
        let peers = |v: &[(&str, &str)]| v.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect::<Vec<_>>();
        let p = peers(&[("aaaa1111", "Desktop"), ("bbbb2222", "desktop"), ("cccc3333", "Mac[Book] (2)\n")]);
        assert_eq!(nickname("aaaa1111", &p), "Desktop · aaaa");
        assert_eq!(nickname("bbbb2222", &p), "desktop · bbbb");
        assert_eq!(nickname("cccc3333", &p), "MacBook 2");
        assert_eq!(nickname("dddd4444", &p), "linked computer · dddd");
    }

    #[test]
    fn the_ai_handshake_checks_both_directions() {
        set_council(vec!["claude".into(), "gemini".into()]);
        // The Mac's Claude has read our Claude; our Gemini has read the Mac's Claude.
        READ.lock().unwrap().insert("node-hs".into(), [("claude".to_string(), ["gemini".to_string()].into())].into());
        let mac = PeerState {
            council: known_ais([json!("claude"), json!("gpt"), json!("intruder")].iter()),
            read_ours: [("claude".to_string(), vec!["claude".to_string()])].into(),
            ..Default::default()
        };
        assert_eq!(mac.council, vec!["claude", "gpt"], "only known AIs are listed");
        let hs = handshake("node-hs", "Lukes-MBP", "Windows PC", Some(&mac));
        let get = |side: &str, ai: &str| hs.iter().find(|h| h.side == side && h.ai == ai).cloned().unwrap();
        let ours = get("here", "claude");
        assert_eq!((ours.handle.as_str(), ours.reads, ours.read), ("@claude-windows-pc", false, true), "read by the Mac, hasn't read it yet");
        assert_eq!((get("here", "gemini").reads, get("here", "gemini").read), (true, false));
        let theirs = get("there", "claude");
        assert_eq!((theirs.handle.as_str(), theirs.reads, theirs.read), ("@claude-lukes-mbp", true, true), "both directions: a green check");
        assert_eq!((get("there", "gpt").reads, get("there", "gpt").read), (false, false));
        assert!(handshake("node-hs", "Lukes-MBP", "Windows PC", None).is_empty(), "nothing while disconnected");
        assert_eq!(read_for("node-hs"), json!({"claude": ["gemini"]}));
        READ.lock().unwrap().remove("node-hs");
    }

    #[test]
    fn heartbeat_activity_is_bounded_and_cleared_when_the_answer_ends() {
        let g = working("Claude in Plans");
        assert!(local_activity().iter().any(|l| l == "Claude in Plans"));
        drop(g);
        assert!(!local_activity().iter().any(|l| l == "Claude in Plans"));
        let long = "x".repeat(500);
        let mut many: Vec<Value> = (0..20).map(|i| json!(format!("AI {i}\u{1b}[31m"))).collect();
        many.push(json!(long));
        let read = read_activity(Some(&Value::Array(many)));
        assert_eq!(read.len(), STATUS_ITEMS);
        assert!(read.iter().all(|l| l.chars().count() <= STATUS_CHARS && !l.contains('\u{1b}')));
        assert!(read_activity(Some(&json!("not a list"))).is_empty());
    }

    #[test]
    fn exactly_one_linked_computer_answers_signal() {
        // Alone, or nobody else running Signal: answer.
        assert!(answers_signal(("mac", 0), &[]));
        // The computer used most recently answers; the other stays quiet.
        assert!(answers_signal(("pc", 200), &[("mac", 100)]));
        assert!(!answers_signal(("mac", 100), &[("pc", 200)]));
        // A tie goes to the lower node ID, so both sides agree.
        assert!(answers_signal(("a", 5), &[("b", 5)]));
        assert!(!answers_signal(("b", 5), &[("a", 5)]));
        // Three computers: only the newest answers.
        let all = [("a", 1u64), ("b", 9), ("c", 4)];
        let winners: Vec<&str> = all
            .iter()
            .filter(|me| answers_signal(**me, &all))
            .map(|me| me.0)
            .collect();
        assert_eq!(winners, vec!["b"]);
    }

    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
    }

    /// Fragment arrival advances a monotonic test clock without making CI wait
    /// for real seconds or depending on overloaded-worker timer precision.
    struct ClockedFragments {
        chunks: Vec<Vec<u8>>, index: usize, offset: usize, fresh: bool,
        clock: Arc<Mutex<Instant>>, step: Duration,
    }
    impl ClockedFragments {
        fn new(input: &[u8], chunk: usize, step: Duration, now: Instant) -> Self {
            Self { chunks: input.chunks(chunk).map(<[u8]>::to_vec).collect(), index: 0, offset: 0,
                fresh: true, clock: Arc::new(Mutex::new(now)), step }
        }
    }
    impl AsyncBufRead for ClockedFragments {
        fn poll_fill_buf(self: std::pin::Pin<&mut Self>, _: &mut std::task::Context<'_>) -> std::task::Poll<std::io::Result<&[u8]>> {
            let this = self.get_mut();
            if this.index == this.chunks.len() { return std::task::Poll::Ready(Ok(&[])); }
            if this.fresh { *this.clock.lock().unwrap() += this.step; this.fresh = false; }
            std::task::Poll::Ready(Ok(&this.chunks[this.index][this.offset..]))
        }
        fn consume(self: std::pin::Pin<&mut Self>, amount: usize) {
            let this = self.get_mut();
            this.offset += amount;
            if this.index < this.chunks.len() && this.offset == this.chunks[this.index].len() {
                this.index += 1; this.offset = 0; this.fresh = true;
            }
        }
    }
    impl AsyncRead for ClockedFragments {
        fn poll_read(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>, output: &mut tokio::io::ReadBuf<'_>) -> std::task::Poll<std::io::Result<()>> {
            let input = std::task::ready!(self.as_mut().poll_fill_buf(cx))?;
            let count = input.len().min(output.remaining());
            output.put_slice(&input[..count]);
            self.consume(count);
            std::task::Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn a_native_frame_can_arrive_slowly_while_each_fragment_makes_progress() {
        let now = Instant::now();
        let mut input = serde_json::to_string(&Wire::Challenge { nonce: "a".repeat(32) }).unwrap();
        input.push('\n');
        let mut reader = ClockedFragments::new(input.as_bytes(), 8, Duration::from_secs(3), now);
        let clock = reader.clock.clone();
        let progress = NativeReadProgress::new(now);
        let line = read_line_clocked(&mut reader, Some((&progress, NATIVE_FRAME_BUDGET)), || *clock.lock().unwrap()).await.unwrap();
        assert_eq!(line, input.trim_end());
        let finished = *clock.lock().unwrap();
        assert!(finished.duration_since(now) > Duration::from_millis(DEAD_AFTER_MS));
        assert!(progress.recent(finished));
    }

    #[tokio::test]
    async fn a_native_frame_with_no_incoming_bytes_hits_its_idle_deadline() {
        let (stream, _held_open) = tokio::io::duplex(17);
        let mut reader = BufReader::new(stream);
        let progress = NativeReadProgress::new(Instant::now());
        let budget = NativeFrameBudget { idle: Duration::from_millis(5), absolute: Duration::from_secs(180) };
        let error = read_line_clocked(&mut reader, Some((&progress, budget)), Instant::now).await.unwrap_err();
        assert!(error.contains("stopped receiving data"), "{error}");
    }

    #[tokio::test]
    async fn continuous_native_progress_cannot_extend_a_frames_absolute_deadline() {
        let now = Instant::now();
        let mut reader = ClockedFragments::new(b"{\"t\":\"challenge\",\"nonce\":\"still arriving\"}\n", 4, Duration::from_secs(3), now);
        let clock = reader.clock.clone();
        let progress = NativeReadProgress::new(now);
        let budget = NativeFrameBudget { idle: Duration::from_secs(4), absolute: Duration::from_secs(10) };
        let error = read_line_clocked(&mut reader, Some((&progress, budget)), || *clock.lock().unwrap()).await.unwrap_err();
        assert!(error.contains("absolute deadline"), "{error}");
        assert!(progress.recent(*clock.lock().unwrap()), "The absolute limit must win even though the last fragment is recent");
    }

    #[tokio::test]
    async fn a_native_frame_still_obeys_the_existing_three_megabyte_limit() {
        let now = Instant::now();
        let input = vec![b'a'; LINE_LIMIT + 1];
        let mut reader = ClockedFragments::new(&input, 64 * 1024, Duration::ZERO, now);
        let clock = reader.clock.clone();
        let progress = NativeReadProgress::new(now);
        let error = read_line_clocked(&mut reader, Some((&progress, NATIVE_FRAME_BUDGET)), || *clock.lock().unwrap()).await.unwrap_err();
        assert_eq!(error, "frame too large");
    }

    fn memory_evidence(local: &str, remote: &str) -> RouteEvidence {
        RouteEvidence::BluetoothRfcomm {
            local_adapter: local.into(), remote_device: remote.into(),
            service_uuid: crate::bluetooth::SERVICE_UUID.into(), channel: 7,
        }
    }

    fn trust_memory_pair(host: &Link, client: &Link) {
        for (here, there) in [(host, client), (client, host)] {
            let (node_id, name) = there.me();
            here.remember(Peer { node_id, name, key: "42".repeat(32), addrs: vec![] }).unwrap();
        }
    }

    /// Native writes can be queued before completion. Hold the final probe
    /// response until flush so dropping the stream cannot accidentally pass.
    struct FlushFinalReply {
        inner: tokio::io::DuplexStream,
        challenge_sent: bool,
        pending: Vec<u8>,
    }
    impl AsyncRead for FlushFinalReply {
        fn poll_read(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>, output: &mut tokio::io::ReadBuf<'_>) -> std::task::Poll<std::io::Result<()>> {
            std::pin::Pin::new(&mut self.inner).poll_read(cx, output)
        }
    }
    impl AsyncWrite for FlushFinalReply {
        fn poll_write(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>, input: &[u8]) -> std::task::Poll<std::io::Result<usize>> {
            if self.challenge_sent {
                if self.pending.len() + input.len() > 64 * 1024 { return std::task::Poll::Ready(Err(std::io::ErrorKind::OutOfMemory.into())); }
                self.pending.extend_from_slice(input);
                return std::task::Poll::Ready(Ok(input.len()));
            }
            let result = std::pin::Pin::new(&mut self.inner).poll_write(cx, input);
            if let std::task::Poll::Ready(Ok(count)) = result {
                self.challenge_sent |= input[..count].contains(&b'\n');
            }
            result
        }
        fn poll_flush(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<std::io::Result<()>> {
            let this = self.get_mut();
            while !this.pending.is_empty() {
                match std::pin::Pin::new(&mut this.inner).poll_write(cx, &this.pending) {
                    std::task::Poll::Ready(Ok(count)) if count > 0 => { this.pending.drain(..count); }
                    std::task::Poll::Ready(Ok(_)) => return std::task::Poll::Ready(Err(std::io::ErrorKind::WriteZero.into())),
                    std::task::Poll::Ready(Err(error)) => return std::task::Poll::Ready(Err(error)),
                    std::task::Poll::Pending => return std::task::Poll::Pending,
                }
            }
            std::pin::Pin::new(&mut this.inner).poll_flush(cx)
        }
        fn poll_shutdown(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<std::io::Result<()>> { self.poll_flush(cx) }
    }

    /// In-memory bytes exercise non-IP transport framing; this is not a hardware Bluetooth result.
    async fn memory_session(host: &Link, client: &Link, token: CancellationToken) ->
        (tokio::task::JoinHandle<Result<(), String>>, tokio::task::JoinHandle<()>) {
        let (server, stream) = tokio::io::duplex(17);
        let receiver = host.clone();
        let server_token = token.clone();
        let receiving = tokio::spawn(async move {
            receiver.accept_connection(LinkConnection {
                stream: Box::new(server), evidence: memory_evidence("020000000001", "020000000002"),
            }, server_token).await
        });
        let peer = client.peer(&host.me().0).unwrap();
        let key = peer_key(&peer).unwrap();
        let evidence = memory_evidence("020000000002", "020000000001");
        let stream: LinkStream = Box::new(stream);
        let (read, mut write) = tokio::io::split(stream);
        let mut read = BufReader::new(read);
        client.authenticate_route(&peer, &key, &mut read, &mut write).await.unwrap();
        let sender = client.clone();
        let sending = tokio::spawn(async move {
            sender.session(peer, key, read, write, evidence.remote_endpoint(), evidence, None, token).await;
        });
        (receiving, sending)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn non_ip_streams_authenticate_payload_receipts_without_creating_ip_evidence() {
        let dir = std::env::temp_dir().join(format!("link-native-probe-{}", uuid::Uuid::new_v4()));
        let (host, _) = node(&dir, "host");
        let (client, _) = node(&dir, "client");
        trust_memory_pair(&host, &client);
        let peer = client.peer(&host.me().0).unwrap();
        let key = peer_key(&peer).unwrap();
        let (server, stream) = tokio::io::duplex(17);
        let token = CancellationToken::new();
        let receiver = host.clone();
        let receiving = tokio::spawn(async move {
            receiver.accept_connection(LinkConnection {
                stream: Box::new(FlushFinalReply { inner: server, challenge_sent: false, pending: Vec::new() }),
                evidence: memory_evidence("020000000001", "020000000002"),
            }, token).await
        });
        let evidence = memory_evidence("020000000002", "020000000001");
        let chunk = random_hex(TRIAL_CHUNK_BYTES / 2);
        let reply = tokio::time::timeout(Duration::from_secs(5), client.exchange_probe(&peer, &key, LinkConnection {
            stream: Box::new(stream), evidence: evidence.clone(),
        }, Some(&chunk))).await.unwrap().unwrap();
        receiving.await.unwrap().unwrap();
        let mut receipt: TransferReceipt = serde_json::from_value(reply["transfer_receipt"].clone()).unwrap();
        assert_eq!(receipt.bytes, TRIAL_CHUNK_BYTES);
        assert_eq!(receipt.sha256, hex(&Sha256::digest(chunk.as_bytes())));
        assert!(receipt_matches_path(&receipt, &evidence, None));
        assert_eq!(receipt.receiver_ip, None);
        assert_eq!(evidence.local_addr(), None);
        receipt.route_evidence = None;
        assert!(!receipt_matches_path(&receipt, &evidence, None), "An IP-era receipt cannot prove native Bluetooth");
        assert!(!host.status().peers[0].connected, "A probe must not replace a live session");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_non_ip_stream_cannot_bypass_the_existing_peer_key() {
        let dir = std::env::temp_dir().join(format!("link-native-untrusted-{}", uuid::Uuid::new_v4()));
        let (host, _) = node(&dir, "host");
        let (client, _) = node(&dir, "client");
        trust_memory_pair(&host, &client);
        let mut peer = client.peer(&host.me().0).unwrap();
        peer.key = "43".repeat(32);
        let key = peer_key(&peer).unwrap();
        let (server, stream) = tokio::io::duplex(17);
        let receiver = host.clone();
        let receiving = tokio::spawn(async move {
            receiver.accept_connection(LinkConnection {
                stream: Box::new(server), evidence: memory_evidence("020000000001", "020000000002"),
            }, CancellationToken::new()).await
        });
        let result = client.exchange_probe(&peer, &key, LinkConnection {
            stream: Box::new(stream), evidence: memory_evidence("020000000002", "020000000001"),
        }, None).await;
        assert!(result.is_err());
        assert!(receiving.await.unwrap().is_err());
        assert!(!host.status().peers[0].connected);
        assert!(host.inbox().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn native_routes_require_saved_canonical_endpoints_and_can_be_selected_without_ip_routes() {
        let dir = std::env::temp_dir().join(format!("link-native-endpoint-{}", uuid::Uuid::new_v4()));
        let (host, _) = node(&dir, "host");
        let (client, _) = node(&dir, "client");
        trust_memory_pair(&host, &client);
        let client_id = client.me().0;
        host.add_peer_route(&client_id, "192.168.1.2:8470").unwrap();
        host.add_peer_route(&client_id, "rfcomm://a1b2c3d4e5f6").unwrap();
        for endpoint in ["rfcomm://A1B2C3D4E5F6/7", "rfcomm://000000000000", "rfcomm://FFFFFFFFFFFF"] {
            assert!(host.add_peer_route(&client_id, endpoint).is_err());
        }
        let peer = host.peer(&client_id).unwrap();
        let native = "rfcomm://A1B2C3D4E5F6".to_string();
        assert_eq!(selected_endpoints(&peer, std::slice::from_ref(&native)).unwrap(), vec![native.clone()]);
        assert_eq!(route_kind(&native), "Bluetooth RFCOMM");
        assert!(selected_endpoints(&peer, &["rfcomm://020000000099".into()]).is_err());
        assert!(peer.addrs.contains(&"192.168.1.2:8470".into()));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn native_receipts_bind_both_radios_and_do_not_accept_ip_evidence() {
        let evidence = memory_evidence("020000000002", "020000000001");
        let mut receipt = transfer_receipt(&"a".repeat(TRIAL_CHUNK_BYTES), &memory_evidence("020000000001", "020000000002")).unwrap();
        assert!(receipt_matches_path(&receipt, &evidence, None));
        receipt.route_evidence = Some(memory_evidence("020000000099", "020000000002"));
        assert!(!receipt_matches_path(&receipt, &evidence, None));
        receipt.route_evidence = Some(memory_evidence("020000000001", "020000000099"));
        assert!(!receipt_matches_path(&receipt, &evidence, None));
        receipt.route_evidence = Some(memory_evidence("020000000001", "020000000002"));
        receipt.receiver_ip = Some("127.0.0.1".parse().unwrap());
        assert!(!receipt_matches_path(&receipt, &evidence, None));
        let path: TransferPath = serde_json::from_value(json!({"source": null, "endpoint": "rfcomm://020000000001"})).unwrap();
        assert!(path.source.is_none());
        let path: TransferPath = serde_json::from_value(json!({"source": "192.168.1.2", "endpoint": "192.168.1.3:8470"})).unwrap();
        assert_eq!(path.source, Some("192.168.1.2".parse().unwrap()));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn non_ip_sessions_preserve_queued_delivery_acknowledgments_and_replay_suppression() {
        let dir = std::env::temp_dir().join(format!("link-native-session-{}", uuid::Uuid::new_v4()));
        let (host, received) = node(&dir, "host");
        let (client, _) = node(&dir, "client");
        trust_memory_pair(&host, &client);
        let id = client.send(PacketKind::Chat, json!({"text": "queued before a native connection"})).unwrap();
        let token = CancellationToken::new();
        let (receiving, sending) = memory_session(&host, &client, token.clone()).await;
        assert!(wait_for(|| received.lock().unwrap().len() == 1 && client.status().peers[0].queued == 0).await);
        assert_eq!(received.lock().unwrap()[0].id, id);
        assert!(matches!(client.status().peers[0].route_evidence, Some(RouteEvidence::BluetoothRfcomm { .. })));
        assert!(client.0.peers.lock().unwrap().values().all(|state| state.local_addr.is_none()));
        token.cancel();
        tokio::time::timeout(Duration::from_secs(5), receiving).await.unwrap().unwrap().unwrap();
        tokio::time::timeout(Duration::from_secs(5), sending).await.unwrap().unwrap();
        let duplicate = received.lock().unwrap()[0].clone();
        client.0.peers.lock().unwrap().get_mut(&host.me().0).unwrap().outbox.push_back(duplicate);
        let token = CancellationToken::new();
        let (receiving, sending) = memory_session(&host, &client, token.clone()).await;
        assert!(wait_for(|| client.status().peers[0].queued == 0).await);
        assert_eq!(received.lock().unwrap().len(), 1);
        assert_eq!(host.inbox().len(), 1);
        token.cancel();
        tokio::time::timeout(Duration::from_secs(5), receiving).await.unwrap().unwrap().unwrap();
        tokio::time::timeout(Duration::from_secs(5), sending).await.unwrap().unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retain_inbox_drops_filed_packets() {
        let dir = std::env::temp_dir().join(format!("link-retain-{}", uuid::Uuid::new_v4()));
        let (link, _) = node(&dir, "retain");
        let make = |id: &str| PeerPacket {
            version: 1,
            id: id.into(),
            node_id: "node-x".into(),
            timestamp_ms: 0,
            kind: PacketKind::Chat,
            seq: 0,
            payload: json!({}),
            payload_sha256: String::new(),
            mac: String::new(),
        };
        let (first, second) = (make(&uuid::Uuid::new_v4().to_string()), make(&uuid::Uuid::new_v4().to_string()));
        link.append_inbox(&first).unwrap();
        link.append_inbox(&second).unwrap();
        assert_eq!(link.retain_inbox(|p| p.id == second.id).unwrap(), 1);
        assert_eq!(link.inbox(), vec![second.clone()]);
        link.append_inbox(&first).unwrap();
        assert_eq!(link.inbox(), vec![second, first], "appending still works after a trim");
        let _ = std::fs::remove_dir_all(&dir);
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

    #[test]
    fn only_the_current_connected_session_can_prove_route_traffic() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut state = PeerState {
            tx: Some(tx),
            generation: 2,
            last_seen_ms: 100,
            ..PeerState::default()
        };
        assert!(!state.record_packet(1, 110));
        assert_eq!((state.packets_in, state.last_seen_ms), (0, 100));
        assert!(state.record_packet(2, 120));
        assert_eq!((state.packets_in, state.last_seen_ms), (1, 120));
        state.tx = None;
        assert!(!state.record_packet(2, 130));
        assert_eq!((state.packets_in, state.last_seen_ms), (1, 120));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_stress_test_switches_through_every_route_and_reports_each() {
        let dir = std::env::temp_dir().join(format!("link-stress-{}", uuid::Uuid::new_v4()));
        let (host, _) = node(&dir, "host");
        let (joiner, _) = node(&dir, "joiner");
        let code = host.begin_pairing().unwrap();
        let good = format!("127.0.0.1:{}", host.0.port);
        joiner.join(&good, &code.code).await.unwrap();
        assert!(wait_for(|| joiner.status().peers.first().is_some_and(|p| p.connected)).await, "session comes up");
        // One route that works and one that is down (nothing listens there).
        let dead = format!("127.0.0.1:{}", free_port());
        joiner.0.cfg.lock().unwrap().peers[0].addrs = vec![good.clone(), dead.clone()];
        // A startup test holding the guard blocks a second diagnostic on the same device.
        let running = joiner.0.stress_test_lock.lock().await;
        assert_eq!(
            joiner.stress_test(None, &[]).await.unwrap_err(),
            "A communication stress test is already running"
        );
        drop(running);
        let host_id = host.status().node_id;
        let previous_session = joiner.0.peers.lock().unwrap()[&host_id].session_cancel.clone().unwrap();
        let reports = joiner.stress_test(None, &[]).await.unwrap();
        assert!(previous_session.is_cancelled(), "the replaced session stops writing immediately");
        assert_eq!(reports.len(), 1);
        let r = &reports[0];
        assert_eq!(r.routes.len(), 2);
        assert!(r.routes[0].ok && r.routes[0].addr == good, "{:?}", r.routes[0]);
        assert!(r.routes[0].detail.contains("traffic confirmed"));
        assert!(r.routes[0].local_addr.unwrap().ip().is_loopback());
        assert_eq!(r.routes[1].local_addr, None);
        assert_eq!(r.routes[1].local_interface, None);
        assert!(!r.routes[1].ok && r.routes[1].addr == dead, "{:?}", r.routes[1]);
        let restored = r.restored.as_ref().expect("reconnect attempted");
        assert!(restored.ok, "{}", restored.detail);
        assert_eq!(restored.addr, good, "back on the working route");
        assert!(restored.ms.is_some(), "reconnect timing captured");
        assert!(wait_for(|| joiner.status().peers[0].connected).await);
        let note = r.summary("requested");
        assert!(note.contains("1 of 2 configured endpoints carried authenticated traffic") && note.contains("✓ Loopback") && note.contains("✗ Loopback"), "{note}");
        host.stop();
        joiner.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn startup_stress_stays_on_ip_and_cannot_replace_a_native_session() {
        let dir = std::env::temp_dir().join(format!("link-startup-stress-{}", uuid::Uuid::new_v4()));
        let (host, _) = node(&dir, "host");
        let (client, _) = node(&dir, "client");
        let code = host.begin_pairing().unwrap();
        let ip = format!("127.0.0.1:{}", host.0.port);
        client.join(&ip, &code.code).await.unwrap();
        let host_id = host.me().0;
        assert!(wait_for(|| client.status().peers[0].connected).await);
        let native = "rfcomm://020000000001".to_string();
        client.0.cfg.lock().unwrap().peers[0].addrs = vec![native.clone(), ip.clone()];
        let addresses = client.peer(&host_id).unwrap().addrs;
        assert_eq!(StressOrigin::StartupIpOnly.eligible_routes(&addresses), vec![ip.clone()]);
        assert!(StressOrigin::Manual.eligible_routes(&addresses).contains(&native));

        // The actual unattended path tests and restores TCP even though a
        // native endpoint is saved. No Bluetooth API is needed by this fixture.
        let reports = client.startup_stress_test().await.unwrap();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].routes.len(), 1);
        assert!(reports[0].routes[0].ok, "{reports:?}");
        assert_eq!(reports[0].routes[0].addr, ip);
        assert!(reports[0].restored.as_ref().is_some_and(|route| route.ok && route.addr == ip));

        let running = client.0.running.lock().unwrap().clone().unwrap();
        let startup = client.startup_stress_scope(&host_id, &running).unwrap();
        let startup_token = startup.stop.as_ref().clone();
        // Pause a TCP session just before publication, as if its startup
        // authentication completed while a native route was becoming active.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let held = TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        let (socket, _) = listener.accept().await.unwrap();
        let LinkConnection { stream, evidence } = LinkConnection::tcp(socket).unwrap();
        let route = evidence.remote_endpoint();
        let (read, write) = tokio::io::split(stream);
        let peer = client.peer(&host_id).unwrap();
        let key = peer_key(&peer).unwrap();
        let owner = client.clone();
        let (release, paused) = oneshot::channel();
        let queued_tcp = tokio::spawn(async move {
            paused.await.unwrap();
            owner.session(peer, key, BufReader::new(read), write, route, evidence, None, startup_token).await;
        });
        let native_token = running.child_token();
        let (receiving, sending) = memory_session(&host, &client, native_token.clone()).await;
        assert!(wait_for(|| client.status().peers[0].route_evidence.as_ref()
            .is_some_and(|evidence| matches!(evidence, RouteEvidence::BluetoothRfcomm { .. }))).await);
        assert!(startup.stop.is_cancelled(), "Native publication preempts only the startup child");
        let (generation, native_session) = {
            let peers = client.0.peers.lock().unwrap();
            (peers[&host_id].generation, peers[&host_id].session_cancel.clone().unwrap())
        };
        assert!(client.startup_stress_test().await.unwrap().is_empty(), "A live native route is left alone");
        release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), queued_tcp).await.unwrap().unwrap();
        drop(startup);
        assert_eq!(client.0.peers.lock().unwrap()[&host_id].generation, generation);
        assert!(!native_session.is_cancelled(), "Neither late TCP publication nor startup restoration replaces native");
        native_token.cancel();
        tokio::time::timeout(Duration::from_secs(2), sending).await.unwrap().unwrap();
        tokio::time::timeout(Duration::from_secs(2), receiving).await.unwrap().unwrap().unwrap();
        drop(held);
        client.stop();
        host.stop();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_scoped_stress_test_leaves_other_peers_on_their_sessions() {
        let dir = std::env::temp_dir().join(format!("link-scoped-stress-{}", uuid::Uuid::new_v4()));
        let (host, _) = node(&dir, "host");
        let (selected, _) = node(&dir, "selected");
        let (other, _) = node(&dir, "other");
        let address = format!("127.0.0.1:{}", host.0.port);
        for peer in [&selected, &other] {
            let code = host.begin_pairing().unwrap();
            peer.join(&address, &code.code).await.unwrap();
        }
        let selected_id = selected.status().node_id;
        let other_id = other.status().node_id;
        assert!(wait_for(|| host.connected_peer_ids().len() == 2).await);
        let (selected_before, other_before) = {
            let peers = host.0.peers.lock().unwrap();
            (peers[&selected_id].generation, peers[&other_id].generation)
        };
        assert_eq!(host.stress_test(Some("missing"), &[]).await.unwrap_err(), "Linked computer not found");
        let source: IpAddr = "127.0.0.1".parse().unwrap();
        let reports = host.stress_test(Some(&selected_id), &[source]).await.unwrap();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].node_id, selected_id);
        assert_eq!(reports[0].routes.len(), 1);
        let route = &reports[0].routes[0];
        assert!(route.ok, "{route:?}");
        assert_eq!(route.requested_source, Some(source));
        assert_eq!(route.local_addr.unwrap().ip(), source);
        assert!(reports[0].restored.as_ref().is_some_and(|route| route.ok), "{reports:?}");
        assert!(reports[0].restored.as_ref().unwrap().requested_source.is_none());
        {
            let peers = host.0.peers.lock().unwrap();
            assert!(peers[&selected_id].generation > selected_before);
            assert_eq!(peers[&other_id].generation, other_before);
            assert!(peers[&other_id].tx.is_some());
        }
        host.stop();
        selected.stop();
        other.stop();
        let _ = std::fs::remove_dir_all(&dir);
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
        let names: std::collections::HashSet<_> = if_addrs::get_if_addrs()
            .unwrap_or_default()
            .into_iter()
            .filter(|interface| interface.ip().to_canonical() == std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST))
            .map(|interface| interface.name)
            .collect();
        let expected = (names.len() == 1).then(|| names.into_iter().next().unwrap());
        assert_eq!(host.status().peers[0].local_interface, expected);
        assert_eq!(joiner.status().peers[0].local_interface, expected);
        let id = joiner.send(PacketKind::Chat, json!({ "text": "hello from the PC" })).unwrap();
        assert!(wait_for(|| host_got.lock().unwrap().len() == 1).await);
        assert_eq!(host_got.lock().unwrap()[0].id, id);
        assert_eq!(host.inbox().len(), 1, "stored durably");
        assert!(wait_for(|| joiner.status().peers[0].queued == 0).await, "acknowledged");

        // Drop the session; a re-sent duplicate is acknowledged but not delivered twice.
        assert_eq!(host.connected_peer_ids(), vec![host.status().peers[0].node_id.clone()]);
        let dup = host_got.lock().unwrap()[0].clone();
        joiner.0.peers.lock().unwrap().get_mut(&view.node_id).unwrap().outbox.push_back(dup);
        joiner.stop();
        assert!(wait_for(|| !host.status().peers[0].connected).await, "host notices the drop");
        assert!(host.connected_peer_ids().is_empty());
        let disconnected = &host.status().peers[0];
        assert_eq!(disconnected.local_interface, None);
        assert_eq!(disconnected.rtt_ms, None);
        let report = host.test_peer(&disconnected.node_id, None).await.unwrap();
        assert!(!report.connected);
        assert_eq!(report.active_route, None);
        assert_eq!(report.rtt_ms, None);
        joiner.start();
        assert!(wait_for(|| joiner.status().peers[0].connected && joiner.status().peers[0].queued == 0).await, "reconnects and drains");
        assert_eq!(host_got.lock().unwrap().len(), 1);
        assert_eq!(host.inbox().len(), 1);
        host.stop();
        joiner.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn queued_chat_is_delivered_once_over_an_alternate_route() {
        let dir = std::env::temp_dir().join(format!("link-alternate-{}", uuid::Uuid::new_v4()));
        let (host, received) = node(&dir, "host");
        let (joiner, _) = node(&dir, "joiner");
        // Give the joiner the first dial so the host cannot bypass the test proxy.
        {
            let mut host_cfg = host.0.cfg.lock().unwrap();
            let mut joiner_cfg = joiner.0.cfg.lock().unwrap();
            if host_cfg.node_id < joiner_cfg.node_id {
                std::mem::swap(&mut host_cfg.node_id, &mut joiner_cfg.node_id);
            }
        }
        let direct = format!("127.0.0.1:{}", host.0.port);
        let code = host.begin_pairing().unwrap();
        joiner.join(&direct, &code.code).await.unwrap();
        assert!(wait_for(|| joiner.status().peers[0].connected).await);
        // Keep the listener available for the proxy without a competing reverse dial.
        host.0.cfg.lock().unwrap().peers[0].addrs.clear();
        joiner.stop();
        assert!(wait_for(|| !host.status().peers[0].connected).await);

        let id = joiner.send(PacketKind::Chat, json!({ "text": "queued during route change" })).unwrap();
        assert_eq!(joiner.status().peers[0].queued, 1);

        // A second loopback endpoint forwards the same authenticated peer session.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let alternate = listener.local_addr().unwrap().to_string();
        let proxy = tokio::spawn(async move {
            while let Ok((mut inbound, _)) = listener.accept().await {
                let Ok(mut outbound) = TcpStream::connect(&direct).await else { continue };
                let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
            }
        });
        let unavailable = tokio::net::TcpSocket::new_v4().unwrap();
        unavailable.bind("127.0.0.1:0".parse().unwrap()).unwrap();
        joiner.0.cfg.lock().unwrap().peers[0].addrs =
            vec![unavailable.local_addr().unwrap().to_string(), alternate.clone()];
        joiner.start();
        assert!(wait_for(|| {
            let status = joiner.status();
            let peer = &status.peers[0];
            peer.route.as_deref() == Some(alternate.as_str()) && peer.queued == 0
        }).await, "the alternate route drains the queue");
        assert_eq!(received.lock().unwrap().iter().map(|packet| packet.id.as_str()).collect::<Vec<_>>(), [id.as_str()]);
        assert_eq!(host.inbox().len(), 1);

        joiner.stop();
        proxy.abort();
        host.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn two_active_route_losses_redial_without_restarting_peerlink() {
        let dir = std::env::temp_dir().join(format!("link-live-failover-{}", uuid::Uuid::new_v4()));
        let (host, received) = node(&dir, "host");
        let (joiner, _) = node(&dir, "joiner");
        // Give the joiner the first dial so the host cannot bypass the test proxy.
        {
            let mut host_cfg = host.0.cfg.lock().unwrap();
            let mut joiner_cfg = joiner.0.cfg.lock().unwrap();
            if host_cfg.node_id < joiner_cfg.node_id {
                std::mem::swap(&mut host_cfg.node_id, &mut joiner_cfg.node_id);
            }
        }
        let direct = format!("127.0.0.1:{}", host.0.port);
        let code = host.begin_pairing().unwrap();

        // Three proxies give one peer independent TCP endpoints inside loopback.
        let mut routes = Vec::new();
        let mut proxies = Vec::new();
        for _ in 0..3 {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            routes.push(listener.local_addr().unwrap().to_string());
            let direct = direct.clone();
            proxies.push(tokio::spawn(async move {
                while let Ok((mut incoming, _)) = listener.accept().await {
                    let Ok(mut outgoing) = TcpStream::connect(&direct).await else { break };
                    let _ = tokio::io::copy_bidirectional(&mut incoming, &mut outgoing).await;
                }
            }));
        }

        joiner.join(&routes[0], &code.code).await.unwrap();
        assert!(wait_for(|| joiner.status().peers[0].route.as_deref() == Some(routes[0].as_str())).await);
        host.0.cfg.lock().unwrap().peers[0].addrs.clear();
        let mut expected = Vec::new();
        for (failed, backup) in routes.windows(2).enumerate() {
            proxies[failed].abort();
            assert!(wait_for(|| !joiner.status().peers[0].connected).await);

            let id = joiner.send(PacketKind::Chat, json!({ "text": format!("during route loss {failed}") })).unwrap();
            assert_eq!(joiner.status().peers[0].queued, 1);
            expected.push(id);

            // Introduce the next route with the test message already queued.
            joiner.0.cfg.lock().unwrap().peers[0].addrs = routes[..=failed + 1].to_vec();
            joiner.0.wake_dial.notify_one();
            assert!(wait_for(|| {
                let status = joiner.status();
                let peer = &status.peers[0];
                peer.route.as_deref() == Some(backup[1].as_str()) && peer.queued == 0
            }).await, "route {} carries traffic after loss {failed}", backup[1]);
            assert_eq!(
                received.lock().unwrap().iter().map(|packet| packet.id.clone()).collect::<Vec<_>>(),
                expected
            );
            assert_eq!(host.inbox().len(), expected.len());
        }
        joiner.stop();
        host.stop();
        proxies[2].abort();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn session_writes_finish_or_release_the_connection_when_stalled() {
        let (mut writer, _reader) = tokio::io::duplex(1);
        let token = CancellationToken::new();
        assert!(!write_session_frame(&mut writer, "frame".into(), &token, Duration::from_millis(40)).await);
        token.cancel();
        assert!(!write_session_frame(&mut writer, "frame".into(), &token, Duration::from_secs(1)).await);

        let (mut writer, reader) = tokio::io::duplex(1);
        let receive = tokio::spawn(async move { read_line(&mut BufReader::new(reader)).await });
        assert!(write_session_frame(&mut writer, "frame".into(), &CancellationToken::new(), Duration::from_secs(1)).await);
        assert_eq!(receive.await.unwrap().unwrap(), "frame");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn redial_waits_for_the_pending_peer_handshake() {
        let dir = std::env::temp_dir().join(format!("link-single-dial-{}", uuid::Uuid::new_v4()));
        let (host, _) = node(&dir, "host");
        let (joiner, _) = node(&dir, "joiner");
        let direct = format!("127.0.0.1:{}", host.0.port);
        let code = host.begin_pairing().unwrap();
        joiner.join(&direct, &code.code).await.unwrap();
        joiner.stop();
        host.stop();
        let stalled = TcpListener::bind("127.0.0.1:0").await.unwrap();
        joiner.0.cfg.lock().unwrap().peers[0].addrs = vec![stalled.local_addr().unwrap().to_string()];
        joiner.start();

        let (first, _) = tokio::time::timeout(Duration::from_secs(8), stalled.accept()).await.unwrap().unwrap();
        assert!(tokio::time::timeout(Duration::from_secs(2), stalled.accept()).await.is_err());
        drop(first);
        assert!(tokio::time::timeout(Duration::from_secs(6), stalled.accept()).await.unwrap().is_ok());

        joiner.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn dial_tries_another_endpoint_after_a_failed_handshake() {
        let dir = std::env::temp_dir().join(format!("link-auth-fallback-{}", uuid::Uuid::new_v4()));
        let (host, _) = node(&dir, "host");
        let (joiner, _) = node(&dir, "joiner");
        let direct = format!("127.0.0.1:{}", host.0.port);
        let code = host.begin_pairing().unwrap();
        joiner.join(&direct, &code.code).await.unwrap();
        joiner.stop();
        assert!(wait_for(|| !host.status().peers[0].connected).await);
        host.0.cfg.lock().unwrap().peers[0].addrs.clear();
        let peer = joiner.0.cfg.lock().unwrap().peers[0].clone();

        let broken = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let first = broken.local_addr().unwrap().to_string();
        let backup = tokio::net::TcpSocket::new_v4().unwrap();
        backup.bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let second = backup.local_addr().unwrap().to_string();
        let proxy = tokio::spawn(async move {
            let (untrusted, _) = broken.accept().await.unwrap();
            // Only expose the backup after the first TCP connection had time to win.
            tokio::time::sleep(Duration::from_millis(100)).await;
            let backup = backup.listen(4).unwrap();
            drop(untrusted);
            let (mut incoming, _) = backup.accept().await.unwrap();
            let mut outgoing = TcpStream::connect(direct).await.unwrap();
            let _ = tokio::io::copy_bidirectional(&mut incoming, &mut outgoing).await;
        });

        let token = CancellationToken::new();
        let dial = {
            let me = joiner.clone();
            let token = token.clone();
            let routes = [first, second.clone()];
            tokio::spawn(async move { me.dial_on(peer, &routes, None, token, None).await })
        };
        assert!(wait_for(|| joiner.status().peers[0].route.as_deref() == Some(second.as_str())).await);
        token.cancel();
        dial.await.unwrap().unwrap();
        proxy.abort();
        host.stop();
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

    #[test]
    fn routes_are_limited_to_private_networks() {
        for ok in ["192.168.1.5", "10.0.0.2", "172.20.1.1", "100.101.102.103", "127.0.0.1", "169.254.1.1", "fd7a:115c::1"] {
            assert!(route_ip_allowed(ok.parse().unwrap()), "{ok}");
        }
        for bad in ["8.8.8.8", "169.254.169.254", "100.128.0.1", "0.0.0.0", "224.0.0.1", "2001:db8::1"] {
            assert!(!route_ip_allowed(bad.parse().unwrap()), "{bad}");
        }
    }

    #[test]
    fn inbox_repairs_an_interrupted_final_record() {
        let dir = std::env::temp_dir().join(format!("link-test-{}", uuid::Uuid::new_v4()));
        let (link, _) = node(&dir, "inbox-repair");
        let key = [9u8; 32];
        let first = PeerPacket::signed("node-peer", now_ms(), PacketKind::Chat, 1, json!({"text": "one"}), &key).unwrap();
        let second = PeerPacket::signed("node-peer", now_ms(), PacketKind::Chat, 2, json!({"text": "two"}), &key).unwrap();
        link.append_inbox(&first).unwrap();
        std::fs::OpenOptions::new().append(true).open(&link.0.inbox_path).unwrap().write_all(b"{\"cut_off\"").unwrap();
        link.append_inbox(&second).unwrap();
        assert_eq!(link.inbox(), vec![first, second]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn targeted_packet_queues_only_for_its_recipient() {
        let dir = std::env::temp_dir().join(format!("link-target-{}", uuid::Uuid::new_v4()));
        let (host, _) = node(&dir, "host");
        let requester = "node-requester000000";
        let bystander = "node-bystander000000";
        for id in [requester, bystander] {
            host.remember(Peer {
                node_id: id.into(),
                name: id.into(),
                key: hex(&[42; 32]),
                addrs: vec![],
            }).unwrap();
        }
        host.send_to(requester, PacketKind::Chat, json!({ "kind": "resources" })).unwrap();
        let status = host.status();
        assert_eq!(status.peers.iter().find(|peer| peer.node_id == requester).unwrap().queued, 1);
        assert_eq!(status.peers.iter().find(|peer| peer.node_id == bystander).unwrap().queued, 0);
        let scan = json!({ "kind": "resources_request" });
        assert!(host.send_connected(PacketKind::Chat, scan.clone()).is_err());
        let (tx, mut receiver) = mpsc::unbounded_channel();
        host.0.peers.lock().unwrap().get_mut(requester).unwrap().tx = Some(tx);
        host.send_connected(PacketKind::Chat, scan).unwrap();
        assert!(receiver.try_recv().is_ok(), "connected peer received the request");
        let status = host.status();
        assert_eq!(status.peers.iter().find(|peer| peer.node_id == requester).unwrap().queued, 2);
        assert_eq!(status.peers.iter().find(|peer| peer.node_id == bystander).unwrap().queued, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stalled_pairing_releases_the_code_and_queue_survives_restart() {
        let dir = std::env::temp_dir().join(format!("link-test-{}", uuid::Uuid::new_v4()));
        let (host, _) = node(&dir, "host");
        let code = host.begin_pairing().unwrap();
        // A stranger starts pairing, then goes silent.
        let mut stall = TcpStream::connect(("127.0.0.1", host.0.port)).await.unwrap();
        let (_, mine) = Spake2::<Ed25519Group>::start_symmetric(&Password::new(b"000000"), &Identity::new(PAIR_ID));
        let start = serde_json::to_string(&Wire::PairStart { node_id: "node-stranger00000000".into(), name: "x".into(), port: 1, spake: hex(&mine) }).unwrap();
        stall.write_all(format!("{start}\n").as_bytes()).await.unwrap();
        assert!(wait_for(|| host.0.pending.lock().unwrap().as_ref().is_some_and(|p| p.busy)).await, "stranger holds the code");
        tokio::time::sleep(PAIR_TIMEOUT + Duration::from_millis(500)).await;
        assert!(host.0.pending.lock().unwrap().as_ref().is_some_and(|p| !p.busy), "timeout frees it");
        drop(stall);

        // The real computer then pairs, and a message queued while offline survives a restart.
        let (joiner, _) = node(&dir, "joiner");
        joiner.join(&format!("127.0.0.1:{}", host.0.port), &code.code).await.unwrap();
        host.stop();
        joiner.stop();
        let id = joiner.send(PacketKind::Chat, json!({ "text": "queued" })).unwrap();
        let port = joiner.0.port;
        drop(joiner);
        let reopened = Link::open(&dir.join("joiner"), port, Arc::new(|_: &PeerPacket| Ok(()))).unwrap();
        assert_eq!(reopened.status().peers[0].queued, 1);
        assert_eq!(reopened.0.peers.lock().unwrap().values().next().unwrap().outbox[0].id, id);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn peer_test_report_evaluates_routes_and_handshake() {
        let dir = std::env::temp_dir().join(format!("link-test-{}", uuid::Uuid::new_v4()));
        let (host, _) = node(&dir, "host-test");
        let code = host.begin_pairing().unwrap();
        let (joiner, _) = node(&dir, "joiner-test");
        joiner.join(&format!("127.0.0.1:{}", host.0.port), &code.code).await.unwrap();

        let joiner_id = joiner.0.cfg.lock().unwrap().node_id.clone();
        assert!(wait_for(|| host.status().peers[0].connected && joiner.status().peers[0].connected).await);
        let unavailable = tokio::net::TcpSocket::new_v4().unwrap();
        unavailable.bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let unreachable = unavailable.local_addr().unwrap().to_string();
        let reachable = format!("127.0.0.1:{}", joiner.0.port);
        let other_service = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let unauthenticated = other_service.local_addr().unwrap().to_string();
        let listener = tokio::spawn(async move { drop(other_service.accept().await); });
        host.0.cfg.lock().unwrap().peers[0].addrs = vec![unreachable.clone(), reachable.clone(), unauthenticated.clone()];
        let host_session = host.0.peers.lock().unwrap()[&joiner_id].generation;
        let host_id = host.status().node_id;
        let joiner_session = joiner.0.peers.lock().unwrap()[&host_id].generation;
        let loopback = "127.0.0.1".parse().unwrap();
        let report = host.test_peer(&joiner_id, Some(loopback)).await.unwrap();
        listener.abort();
        assert_eq!(report.name, joiner.0.cfg.lock().unwrap().name);
        assert!(report.handshake_ok);
        assert_eq!(report.routes.len(), 3);
        // An authenticated heartbeat may put the peer's working address first.
        let route = |addr: &str| report.routes.iter().find(|route| route.addr == addr).unwrap();
        let unreachable = route(&unreachable);
        let reachable = route(&reachable);
        let unauthenticated = route(&unauthenticated);
        assert!(!unreachable.reachable && reachable.reachable && unauthenticated.reachable);
        assert_eq!(unreachable.authenticated_ms, None);
        assert!(reachable.authenticated_ms.unwrap() >= reachable.rtt_ms.unwrap());
        assert_eq!(unauthenticated.authenticated_ms, None);
        assert!(unauthenticated.status.contains("peer authentication unconfirmed"));
        assert_eq!(host.0.peers.lock().unwrap()[&joiner_id].generation, host_session);
        assert_eq!(joiner.0.peers.lock().unwrap()[&host_id].generation, joiner_session);
        assert_eq!(unreachable.local_ip, None);
        assert_eq!(unreachable.local_interface, None);
        let source = report.source.as_ref().unwrap();
        assert_eq!(source.address, loopback);
        assert_eq!(reachable.local_ip, Some(loopback));
        assert_eq!(reachable.local_interface, interface_name(&if_addrs::get_if_addrs().unwrap_or_default(), loopback));
        assert_eq!(reachable.local_interface.as_deref(), Some(source.interface.as_str()));
        assert!(host.test_peer(&joiner_id, Some("198.51.100.1".parse().unwrap())).await.unwrap_err()
            .contains("selected source address"));

        host.stop();
        joiner.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn paired_computers_exchange_lan_notes_only_after_advertising_support() {
        let dir = std::env::temp_dir().join(format!("link-lan-notes-{}", uuid::Uuid::new_v4()));
        let (host, _) = node(&dir, "host");
        let (joiner, received) = node(&dir, "joiner");
        assert!(crate::peer_sync::send_lan_note(&host, "unknown", "note").is_err());
        let code = host.begin_pairing().unwrap();
        joiner.join(&format!("127.0.0.1:{}", host.0.port), &code.code).await.unwrap();
        let recipient = joiner.status().node_id;
        assert!(wait_for(|| host.status().peers.iter().any(|peer| peer.supports_lan_notes)).await);
        assert!(crate::peer_sync::send_lan_note(&host, &recipient, &"x".repeat(1001)).is_err());
        let id = crate::peer_sync::send_lan_note(&host, &recipient, "Coffee ready.").unwrap();
        assert!(wait_for(|| received.lock().unwrap().iter().any(|packet| {
            packet.id == id && packet.payload["kind"] == "lan_note" && packet.payload["text"] == "Coffee ready."
        })).await);
        host.stop();
        joiner.stop();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn authenticated_payload_paths_report_overlap_without_replacing_session() {
        let dir = std::env::temp_dir().join(format!("link-transfer-{}", uuid::Uuid::new_v4()));
        let (host, _) = node(&dir, "host");
        let (joiner, _) = node(&dir, "joiner");
        let code = host.begin_pairing().unwrap();
        joiner.join(&format!("127.0.0.1:{}", host.0.port), &code.code).await.unwrap();
        assert!(wait_for(|| host.status().peers[0].connected && joiner.status().peers[0].connected).await);
        let joiner_id = joiner.status().node_id;
        let generation = host.0.peers.lock().unwrap()[&joiner_id].generation;
        let peer = host.peer(&joiner_id).unwrap();
        let key = peer_key(&peer).unwrap();
        let dest = format!("127.0.0.1:{}", joiner.0.port);
        let address = "127.0.0.1".parse().unwrap();
        let source = validate_probe_source(&if_addrs::get_if_addrs().unwrap(), address).unwrap();
        // Run the wire protocol on loopback. The public trial requires distinct interfaces and IPs.
        let (a, b, c) = tokio::join!(
            host.transfer_path(&peer, &key, Some(source.clone()), dest.clone()),
            host.transfer_path(&peer, &key, Some(source.clone()), dest.clone()),
            host.transfer_path(&peer, &key, Some(source), dest.clone()),
        );
        let mut paths = [a, b, c];
        for path in &paths {
            assert!(path.error.is_none(), "{:?}", path.error);
            assert_eq!(path.receipts.iter().map(|receipt| receipt.bytes).sum::<usize>(), 32 * 1024);
            assert!(path.receipts.iter().all(|receipt| receipt.receiver_ip == Some(address) && receipt.sha256.len() == 64));
            assert!(path.receipts.iter().all(|receipt| receipt.receiver_interface.as_deref() == Some(path.source.as_ref().unwrap().interface.as_str())));
        }
        assert!(transfer_overlap(&paths).is_some());
        assert!(!distinct_receiver_interfaces(&paths), "loopback samples share the same receiving adapter");
        for (index, path) in paths.iter_mut().enumerate() {
            for receipt in &mut path.receipts {
                receipt.receiver_interface = Some(format!("adapter-{index}"));
                receipt.route_evidence = None; // Older peers supplied names without typed transport evidence.
            }
        }
        assert!(distinct_receiver_interfaces(&paths));
        paths[1].receipts[0].receiver_interface = None;
        assert!(!distinct_receiver_interfaces(&paths));
        paths[0].receipts.pop();
        assert_eq!(transfer_overlap(&paths), None);
        let duplicate = || TransferPath { source: Some(address), endpoint: dest.clone() };
        assert!(host.transfer_trial(&joiner_id, vec![duplicate(), duplicate()]).await.unwrap_err()
            .contains("different local adapter"));
        let stub = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let stub_endpoint = stub.local_addr().unwrap().to_string();
        let close = tokio::spawn(async move { drop(stub.accept().await.unwrap()); });
        let unavailable = host.transfer_path(&peer, &key, paths[0].source.clone(), stub_endpoint).await;
        close.await.unwrap();
        assert!(unavailable.receipts.is_empty());
        assert!(unavailable.error.as_deref().unwrap().starts_with("Sample 1 of 4: Could not read the peer challenge"));
        assert_eq!(host.0.peers.lock().unwrap()[&joiner_id].generation, generation);
        let evidence = paths[0].route_evidence.as_ref().unwrap();
        assert!(transfer_receipt("a", evidence).is_err());
        assert!(transfer_receipt(&"g".repeat(TRIAL_CHUNK_BYTES), evidence).is_err());
        host.stop();
        joiner.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn three_endpoint_probes_overlap_without_replacing_the_peer_session() {
        let dir = std::env::temp_dir().join(format!("link-probe-overlap-{}", uuid::Uuid::new_v4()));
        let (host, _) = node(&dir, "host");
        let (joiner, received) = node(&dir, "joiner");
        let host_addr = format!("127.0.0.1:{}", host.0.port);
        let code = host.begin_pairing().unwrap();
        joiner.join(&host_addr, &code.code).await.unwrap();
        assert!(wait_for(|| host.status().peers[0].connected && joiner.status().peers[0].connected).await);

        let joiner_addr = format!("127.0.0.1:{}", joiner.0.port);
        let ready = Arc::new(tokio::sync::Barrier::new(3));
        let mut routes = Vec::new();
        let mut proxies = tokio::task::JoinSet::new();
        for _ in 0..3 {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            routes.push(listener.local_addr().unwrap().to_string());
            let (ready, target) = (ready.clone(), joiner_addr.clone());
            proxies.spawn(async move {
                let (mut client, _) = listener.accept().await.unwrap();
                let mut peer = TcpStream::connect(target).await.unwrap();
                let mut from_peer = BufReader::new(&mut peer);
                write_wire(&mut client, &read_wire(&mut from_peer).await.unwrap()).await.unwrap();
                let request = read_wire(&mut BufReader::new(&mut client)).await.unwrap();
                // Hold all three encrypted requests before forwarding any to the recipient.
                ready.wait().await;
                write_wire(from_peer.get_mut(), &request).await.unwrap();
                let reply = read_wire(&mut from_peer).await.unwrap();
                write_wire(&mut client, &reply).await.unwrap();
            });
        }
        let joiner_id = joiner.status().node_id;
        let generation = host.0.peers.lock().unwrap()[&joiner_id].generation;
        let host_id = host.status().node_id;
        let remote_generation = joiner.0.peers.lock().unwrap()[&host_id].generation;
        host.0.cfg.lock().unwrap().peers[0].addrs = routes;
        let report = host.test_peer(&joiner_id, None).await.unwrap();
        assert_eq!(report.routes.len(), 3);
        assert!(report.routes.iter().all(|route| route.authenticated_ms.is_some()), "{report:?}");
        while let Some(result) = proxies.join_next().await {
            result.unwrap();
        }
        assert_eq!(host.0.peers.lock().unwrap()[&joiner_id].generation, generation);
        assert_eq!(joiner.0.peers.lock().unwrap()[&host_id].generation, remote_generation);
        let packet = host.send(PacketKind::Chat, json!({ "text": "after the probes" })).unwrap();
        assert!(wait_for(|| host.status().peers[0].queued == 0).await);
        assert_eq!(received.lock().unwrap().iter().map(|item| item.id.as_str()).collect::<Vec<_>>(), [packet.as_str()]);

        host.stop();
        joiner.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_ai_handshake_test_waits_for_a_new_report_on_the_existing_session() {
        let dir = std::env::temp_dir().join(format!("link-ai-check-{}", uuid::Uuid::new_v4()));
        let (host, _) = node(&dir, "host");
        let (joiner, _) = node(&dir, "joiner");
        let code = host.begin_pairing().unwrap();
        joiner.join(&format!("127.0.0.1:{}", host.0.port), &code.code).await.unwrap();
        let joiner_id = joiner.status().node_id;
        assert!(wait_for(|| host.status().peers[0].connected && joiner.status().peers[0].connected).await);
        let (generation, reports) = {
            let states = host.0.peers.lock().unwrap();
            (states[&joiner_id].generation, states[&joiner_id].ai_reports)
        };
        assert!(host.test_ai_handshake(&joiner_id).await.is_ok());
        {
            let states = host.0.peers.lock().unwrap();
            assert_eq!(states[&joiner_id].generation, generation);
            assert!(states[&joiner_id].ai_reports > reports);
        }
        let endpoint = format!("127.0.0.1:{}", joiner.0.port);
        let selected = host.test_ai_handshake_selected(&joiner_id, Some(&endpoint)).await.unwrap();
        assert_eq!(selected.route, endpoint);
        assert!(matches!(selected.route_evidence, Some(RouteEvidence::Tcp { .. })));
        assert!(selected.restored.as_ref().is_some_and(|route| route.ok), "{selected:?}");
        assert!(host.test_ai_handshake_selected(&joiner_id, Some("rfcomm://020000000099")).await.is_err());
        host.stop();
        assert_eq!(host.test_ai_handshake(&joiner_id).await.unwrap_err(), "The linked computer is offline");
        joiner.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn failover_routes_can_be_added_and_refreshed() {
        let dir = std::env::temp_dir().join(format!("link-test-{}", uuid::Uuid::new_v4()));
        let (host, _) = node(&dir, "host-failover");
        let code = host.begin_pairing().unwrap();
        let (joiner, _) = node(&dir, "joiner-failover");
        joiner.join(&format!("127.0.0.1:{}", host.0.port), &code.code).await.unwrap();

        let joiner_id = joiner.0.cfg.lock().unwrap().node_id.clone();
        let routes = host.add_peer_route(&joiner_id, "192.168.1.200:8470").unwrap();
        assert!(routes.contains(&"192.168.1.200:8470".to_string()));

        // Invalid or public IP refused
        assert!(host.add_peer_route(&joiner_id, "8.8.8.8:8470").is_err());

        // Refresh routes succeeds
        let refreshed = host.refresh_routes(&joiner_id).unwrap();
        assert!(refreshed.contains(&"192.168.1.200:8470".to_string()));

        host.stop();
        joiner.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
