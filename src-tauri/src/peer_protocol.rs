//! Authenticated, replay-safe frames shared by every future Mac/Windows peer transport.
//!
//! The transport (LAN, Tailscale, Bluetooth or an explicitly configured relay) is deliberately
//! separate. Every path must authenticate and record the same frame before dispatch, so switching
//! paths cannot duplicate a chat event or weaken its trust boundary.

use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

pub const PROTOCOL_VERSION: u16 = 1;
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;
pub const DEFAULT_MAX_AGE_MS: u64 = 5 * 60 * 1000;
pub const DEFAULT_MAX_FUTURE_SKEW_MS: u64 = 30 * 1000;
pub const DEFAULT_LEDGER_ENTRIES: usize = 50_000;

/// Only persisted, inspectable product state crosses devices. Provider-private reasoning is not a
/// packet kind and must never be represented as activity or chat text.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PacketKind {
    Chat,
    RequestDecision,
    ReadReceipt,
    Activity,
    ProjectState,
    Heartbeat,
    Acknowledgement,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PeerPacket {
    pub version: u16,
    pub id: String,
    pub node_id: String,
    pub timestamp_ms: u64,
    pub kind: PacketKind,
    pub seq: u64,
    pub payload: Value,
    pub payload_sha256: String,
    /// Lower-case hexadecimal HMAC-SHA256 over every preceding field in canonical struct order.
    pub mac: String,
}

#[derive(Serialize)]
struct AuthenticatedFields<'a> {
    version: u16,
    id: &'a str,
    node_id: &'a str,
    timestamp_ms: u64,
    kind: PacketKind,
    seq: u64,
    payload: &'a Value,
    payload_sha256: &'a str,
}

impl PeerPacket {
    pub fn signed(
        node_id: &str,
        timestamp_ms: u64,
        kind: PacketKind,
        seq: u64,
        payload: Value,
        key: &[u8],
    ) -> Result<Self, String> {
        validate_node_id(node_id)?;
        validate_key(key)?;
        let payload_sha256 = hash_payload(&payload)?;
        let mut packet = Self {
            version: PROTOCOL_VERSION,
            id: uuid::Uuid::new_v4().to_string(),
            node_id: node_id.to_string(),
            timestamp_ms,
            kind,
            seq,
            payload,
            payload_sha256,
            mac: String::new(),
        };
        packet.mac = packet.expected_mac(key)?;
        if serde_json::to_vec(&packet)
            .map_err(|e| e.to_string())?
            .len()
            > MAX_FRAME_BYTES
        {
            return Err(format!("peer frame exceeds {MAX_FRAME_BYTES} bytes"));
        }
        Ok(packet)
    }

    /// Re-stamps a queued packet for redelivery after an outage. The id and sequence stay the same,
    /// so the receiver's ledger still drops it if the first delivery had in fact arrived.
    pub fn resigned(&self, timestamp_ms: u64, key: &[u8]) -> Result<Self, String> {
        validate_key(key)?;
        let mut packet = self.clone();
        packet.timestamp_ms = timestamp_ms;
        packet.mac = packet.expected_mac(key)?;
        Ok(packet)
    }

    /// Validates a frame before its id is written to the replay ledger. `expected_peer` comes from
    /// the paired-device record, never from discovery data supplied by the network.
    pub fn validate(&self, expected_peer: &str, key: &[u8], now_ms: u64) -> Result<(), String> {
        validate_key(key)?;
        if self.version != PROTOCOL_VERSION {
            return Err(format!(
                "unsupported peer protocol version {}",
                self.version
            ));
        }
        uuid::Uuid::parse_str(&self.id).map_err(|_| "invalid peer packet id".to_string())?;
        validate_node_id(&self.node_id)?;
        if self.node_id != expected_peer {
            return Err("peer packet node does not match the paired device".into());
        }
        if self.seq == 0 {
            return Err("peer packet sequence must be positive".into());
        }
        if now_ms.saturating_sub(self.timestamp_ms) > DEFAULT_MAX_AGE_MS {
            return Err("peer packet is outside the freshness window".into());
        }
        if self.timestamp_ms.saturating_sub(now_ms) > DEFAULT_MAX_FUTURE_SKEW_MS {
            return Err("peer packet timestamp is too far in the future".into());
        }
        let encoded = serde_json::to_vec(self).map_err(|e| e.to_string())?;
        if encoded.len() > MAX_FRAME_BYTES {
            return Err(format!("peer frame exceeds {MAX_FRAME_BYTES} bytes"));
        }
        let actual_payload_hash = hash_payload(&self.payload)?;
        if actual_payload_hash != self.payload_sha256 {
            return Err("peer packet payload hash does not match".into());
        }
        let signature =
            decode_hex(&self.mac).ok_or_else(|| "invalid peer packet MAC encoding".to_string())?;
        let bytes = self.authenticated_bytes()?;
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key)
            .map_err(|_| "invalid peer key".to_string())?;
        mac.update(&bytes);
        mac.verify_slice(&signature)
            .map_err(|_| "peer packet authentication failed".to_string())
    }

    fn authenticated_bytes(&self) -> Result<Vec<u8>, String> {
        serde_json::to_vec(&AuthenticatedFields {
            version: self.version,
            id: &self.id,
            node_id: &self.node_id,
            timestamp_ms: self.timestamp_ms,
            kind: self.kind,
            seq: self.seq,
            payload: &self.payload,
            payload_sha256: &self.payload_sha256,
        })
        .map_err(|e| e.to_string())
    }

    fn expected_mac(&self, key: &[u8]) -> Result<String, String> {
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key)
            .map_err(|_| "invalid peer key".to_string())?;
        mac.update(&self.authenticated_bytes()?);
        Ok(hex(mac.finalize().into_bytes().as_slice()))
    }
}

fn validate_key(key: &[u8]) -> Result<(), String> {
    if key.len() < 32 {
        Err("paired-device key must contain at least 32 bytes".into())
    } else {
        Ok(())
    }
}

fn validate_node_id(node_id: &str) -> Result<(), String> {
    if node_id.is_empty()
        || node_id.len() > 64
        || !node_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        Err("invalid peer node id".into())
    } else {
        Ok(())
    }
}

fn hash_payload(payload: &Value) -> Result<String, String> {
    let encoded = serde_json::to_vec(payload).map_err(|e| e.to_string())?;
    Ok(hex(&Sha256::digest(encoded)))
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}

fn decode_hex(value: &str) -> Option<Vec<u8>> {
    let (pairs, remainder) = value.as_bytes().as_chunks::<2>();
    if !remainder.is_empty() {
        return None;
    }
    pairs
        .iter()
        .map(|pair| {
            let high = (pair[0] as char).to_digit(16)?;
            let low = (pair[1] as char).to_digit(16)?;
            Some(((high << 4) | low) as u8)
        })
        .collect()
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum LedgerState {
    Received,
    Committed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LedgerEntry {
    id: String,
    node_id: String,
    seq: u64,
    timestamp_ms: u64,
    state: LedgerState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayDisposition {
    /// This packet has never been seen and is now durably reserved for dispatch.
    New,
    /// A crash happened after reservation. Dispatch again using the packet id as the destination
    /// event id, so the conversation store can make that retry idempotent.
    Resume,
    /// Dispatch already committed; acknowledge without dispatching it again.
    Duplicate,
}

/// A bounded two-phase append ledger. `begin` durably reserves a validated packet before dispatch;
/// `commit` runs only after the destination has persisted that same packet id. A crash between them
/// returns `Resume`, allowing an idempotent destination write rather than losing the message.
pub struct ReplayLedger {
    path: PathBuf,
    max_entries: usize,
}

impl ReplayLedger {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            max_entries: DEFAULT_LEDGER_ENTRIES,
        }
    }

    pub fn with_capacity(path: PathBuf, max_entries: usize) -> Self {
        Self {
            path,
            max_entries: max_entries.max(32),
        }
    }

    pub fn begin(&self, packet: &PeerPacket) -> Result<ReplayDisposition, String> {
        uuid::Uuid::parse_str(&packet.id).map_err(|_| "invalid peer packet id".to_string())?;
        self.with_lock(|entries| {
            match entries
                .iter()
                .rev()
                .find(|entry| entry.id == packet.id)
                .map(|entry| entry.state)
            {
                Some(LedgerState::Committed) => Ok(ReplayDisposition::Duplicate),
                Some(LedgerState::Received) => Ok(ReplayDisposition::Resume),
                None => {
                    self.persist(
                        entries,
                        LedgerEntry {
                            id: packet.id.clone(),
                            node_id: packet.node_id.clone(),
                            seq: packet.seq,
                            timestamp_ms: packet.timestamp_ms,
                            state: LedgerState::Received,
                        },
                    )?;
                    Ok(ReplayDisposition::New)
                }
            }
        })
    }

    /// Marks an idempotently persisted destination event complete. The transport may acknowledge
    /// the packet only after this returns successfully.
    pub fn commit(&self, packet: &PeerPacket) -> Result<bool, String> {
        uuid::Uuid::parse_str(&packet.id).map_err(|_| "invalid peer packet id".to_string())?;
        self.with_lock(|entries| {
            match entries
                .iter()
                .rev()
                .find(|entry| entry.id == packet.id)
                .map(|entry| entry.state)
            {
                Some(LedgerState::Committed) => Ok(false),
                Some(LedgerState::Received) => {
                    self.persist(
                        entries,
                        LedgerEntry {
                            id: packet.id.clone(),
                            node_id: packet.node_id.clone(),
                            seq: packet.seq,
                            timestamp_ms: packet.timestamp_ms,
                            state: LedgerState::Committed,
                        },
                    )?;
                    Ok(true)
                }
                None => Err("peer packet must be reserved before commit".into()),
            }
        })
    }

    fn with_lock<T>(
        &self,
        action: impl FnOnce(&mut Vec<LedgerEntry>) -> Result<T, String>,
    ) -> Result<T, String> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.path.with_extension("lock"))
            .map_err(|e| e.to_string())?;
        lock.lock().map_err(|e| e.to_string())?;
        let mut entries = read_ledger(&self.path)?;
        action(&mut entries)
    }

    fn persist(&self, entries: &mut Vec<LedgerEntry>, entry: LedgerEntry) -> Result<(), String> {
        entries.push(entry);
        if entries.len() >= self.max_entries {
            let compacted = latest_entries(entries, self.max_entries);
            write_ledger(&self.path, &compacted)
        } else {
            append_ledger(&self.path, entries.last().expect("just appended"))
        }
    }
}

fn latest_entries(entries: &[LedgerEntry], capacity: usize) -> Vec<LedgerEntry> {
    let mut ids = HashSet::new();
    let mut latest: Vec<LedgerEntry> = entries
        .iter()
        .rev()
        .filter(|entry| ids.insert(entry.id.clone()))
        .take(capacity)
        .cloned()
        .collect();
    latest.reverse();
    latest
}

fn read_ledger(path: &Path) -> Result<Vec<LedgerEntry>, String> {
    let Ok(text) = fs::read_to_string(path) else {
        return Ok(Vec::new());
    };
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str(line).map_err(|e| format!("invalid peer replay ledger: {e}"))
        })
        .collect()
}

fn append_ledger(path: &Path, entry: &LedgerEntry) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    set_private_permissions(path);
    serde_json::to_writer(&mut file, entry).map_err(|e| e.to_string())?;
    file.write_all(b"\n").map_err(|e| e.to_string())?;
    file.sync_data().map_err(|e| e.to_string())
}

fn write_ledger(path: &Path, entries: &[LedgerEntry]) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&tmp)
        .map_err(|e| e.to_string())?;
    set_private_permissions(&tmp);
    for entry in entries {
        serde_json::to_writer(&mut file, entry).map_err(|e| e.to_string())?;
        file.write_all(b"\n").map_err(|e| e.to_string())?;
    }
    file.sync_all().map_err(|e| e.to_string())?;
    crate::store::replace_file(&tmp, path).map_err(|e| e.to_string())?;
    set_private_permissions(path);
    Ok(())
}

#[cfg(unix)]
fn set_private_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn set_private_permissions(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const KEY: &[u8] = b"0123456789abcdef0123456789abcdef";
    const NOW: u64 = 1_800_000_000_000;

    fn packet() -> PeerPacket {
        PeerPacket::signed(
            "mac-host",
            NOW,
            PacketKind::Chat,
            1,
            json!({"text": "hello"}),
            KEY,
        )
        .unwrap()
    }

    #[test]
    fn signed_packet_authenticates_only_for_its_paired_peer() {
        let frame = packet();
        assert!(frame.validate("mac-host", KEY, NOW).is_ok());
        assert!(frame
            .validate("windows-host", KEY, NOW)
            .unwrap_err()
            .contains("paired device"));
        assert!(frame
            .validate("mac-host", b"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx", NOW)
            .unwrap_err()
            .contains("authentication"));
    }

    #[test]
    fn payload_tampering_and_stale_frames_are_rejected() {
        let mut frame = packet();
        frame.payload = json!({"text": "changed"});
        assert!(frame
            .validate("mac-host", KEY, NOW)
            .unwrap_err()
            .contains("payload hash"));

        let stale = PeerPacket::signed(
            "mac-host",
            NOW - DEFAULT_MAX_AGE_MS - 1,
            PacketKind::Chat,
            2,
            json!({}),
            KEY,
        )
        .unwrap();
        assert!(stale
            .validate("mac-host", KEY, NOW)
            .unwrap_err()
            .contains("freshness"));
    }

    #[test]
    fn hidden_reasoning_is_not_a_protocol_packet_kind() {
        let json = r#"{"version":1,"id":"00000000-0000-0000-0000-000000000000","node_id":"mac-host","timestamp_ms":1,"kind":"thought","seq":1,"payload":{},"payload_sha256":"x","mac":"x"}"#;
        assert!(serde_json::from_str::<PeerPacket>(json).is_err());
    }

    #[test]
    fn ledger_resumes_an_interrupted_dispatch_then_refuses_a_committed_duplicate() {
        let dir =
            std::env::temp_dir().join(format!("consilium-peer-ledger-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("seen.jsonl");
        let frame = packet();
        assert_eq!(
            ReplayLedger::with_capacity(path.clone(), 32)
                .begin(&frame)
                .unwrap(),
            ReplayDisposition::New
        );
        let reopened = ReplayLedger::with_capacity(path, 32);
        assert_eq!(reopened.begin(&frame).unwrap(), ReplayDisposition::Resume);
        assert!(reopened.commit(&frame).unwrap());
        assert_eq!(
            reopened.begin(&frame).unwrap(),
            ReplayDisposition::Duplicate
        );
        assert!(!reopened.commit(&frame).unwrap());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn ledger_compaction_remains_bounded_and_keeps_recent_ids() {
        let dir =
            std::env::temp_dir().join(format!("consilium-peer-ledger-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("seen.jsonl");
        let ledger = ReplayLedger::with_capacity(path.clone(), 32);
        let mut last = None;
        for seq in 1..=40 {
            let frame = PeerPacket::signed(
                "mac-host",
                NOW,
                PacketKind::Chat,
                seq,
                json!({"seq": seq}),
                KEY,
            )
            .unwrap();
            assert_eq!(ledger.begin(&frame).unwrap(), ReplayDisposition::New);
            assert!(ledger.commit(&frame).unwrap());
            last = Some(frame);
        }
        assert_eq!(read_ledger(&path).unwrap().len(), 32);
        assert_eq!(
            ledger.begin(&last.unwrap()).unwrap(),
            ReplayDisposition::Duplicate
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn malformed_mac_and_short_keys_are_rejected() {
        let mut frame = packet();
        frame.mac = "not-hex".into();
        assert!(frame
            .validate("mac-host", KEY, NOW)
            .unwrap_err()
            .contains("encoding"));
        assert!(frame
            .validate("mac-host", b"short", NOW)
            .unwrap_err()
            .contains("32 bytes"));
    }

    #[test]
    fn ledger_file_contains_no_payload_content() {
        let dir =
            std::env::temp_dir().join(format!("consilium-peer-ledger-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("seen.jsonl");
        let frame = PeerPacket::signed(
            "mac-host",
            NOW,
            PacketKind::Chat,
            1,
            json!({"secret": "never-store-this"}),
            KEY,
        )
        .unwrap();
        assert_eq!(
            ReplayLedger::new(path.clone()).begin(&frame).unwrap(),
            ReplayDisposition::New
        );
        let text = fs::read_to_string(path).unwrap();
        assert!(!text.contains("never-store-this"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn commit_without_a_reservation_is_refused() {
        let dir =
            std::env::temp_dir().join(format!("consilium-peer-ledger-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let ledger = ReplayLedger::new(dir.join("seen.jsonl"));
        assert!(ledger.commit(&packet()).unwrap_err().contains("reserved"));
        fs::remove_dir_all(dir).unwrap();
    }
}
