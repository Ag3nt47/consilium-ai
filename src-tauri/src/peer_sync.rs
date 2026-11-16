//! Copies conversations between the user's linked computers (Document 3, first step).
//!
//! Each chat message shown on one computer is sent to every paired computer as a `Chat` packet.
//! The receiver files it in a mirror conversation of its own, titled "<title> (<computer>)",
//! so the two councils never answer the same message: the computer the message was typed on
//! owns the turn. Mirrors are never sent back, which keeps two computers from echoing forever.
//! Imported AI messages carry no REQUEST buttons yet; answering requests from another computer
//! is the next step.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::peer_protocol::{PacketKind, PeerPacket};
use crate::store::{Event, Store};

/// How many recently shared event ids are remembered, so an event shown twice is sent once.
const RECENT: usize = 4096;

#[derive(Default, Serialize, Deserialize)]
struct Mirrors {
    /// "<node id>/<their conversation id>" -> the local mirror conversation id.
    map: HashMap<String, String>,
    /// Packet ids already filed, so re-reading the inbox at startup files nothing twice.
    #[serde(default)]
    filed: HashSet<String>,
}

pub struct Sync {
    store: Arc<Store>,
    path: PathBuf,
    mirrors: Mutex<Mirrors>,
    recent: Mutex<(HashSet<String>, VecDeque<String>)>,
}

static SYNC: OnceLock<Sync> = OnceLock::new();

/// The wire form of one shared message.
#[derive(Serialize, Deserialize)]
struct Shared {
    conversation: String,
    title: String,
    id: String,
    sender: String,
    kind: String,
    #[serde(default)]
    to: Vec<String>,
    text: String,
    #[serde(default)]
    via: Option<String>,
}

impl Sync {
    pub fn new(store: Arc<Store>, link_dir: &Path) -> Self {
        let path = link_dir.join("mirrors.json");
        let mirrors = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Sync { store, path, mirrors: Mutex::new(mirrors), recent: Mutex::new((HashSet::new(), VecDeque::new())) }
    }

    fn is_mirror(&self, conversation: &str) -> bool {
        self.mirrors.lock().unwrap().map.values().any(|v| v == conversation)
    }

    /// The payload to send for `e`, or None if it shouldn't leave this computer.
    fn outgoing(&self, e: &Event) -> Option<Value> {
        if e.kind != "message" || e.text.trim().is_empty() || self.is_mirror(&e.conversation) {
            return None;
        }
        {
            let mut recent = self.recent.lock().unwrap();
            if !recent.0.insert(e.id.clone()) {
                return None;
            }
            recent.1.push_back(e.id.clone());
            if recent.1.len() > RECENT {
                if let Some(old) = recent.1.pop_front() {
                    recent.0.remove(&old);
                }
            }
        }
        let title = self.store.meta(&e.conversation).map(|m| m.title).unwrap_or_default();
        serde_json::to_value(Shared {
            conversation: e.conversation.clone(),
            title,
            id: e.id.clone(),
            sender: e.sender.clone(),
            kind: e.kind.clone(),
            to: e.to.clone(),
            text: e.text.clone(),
            via: e.via.clone(),
        })
        .ok()
    }

    /// Files a received chat packet in its mirror conversation. Returns the new event, if any.
    /// `computer` is the sending computer's name.
    pub fn file(&self, packet: &PeerPacket, computer: &str) -> Result<Option<Event>, String> {
        if packet.kind != PacketKind::Chat {
            return Ok(None);
        }
        let shared: Shared = serde_json::from_value(packet.payload.clone()).map_err(|e| e.to_string())?;
        if shared.kind != "message" || !crate::store::valid_id(&shared.conversation) {
            return Ok(None);
        }
        let mut m = self.mirrors.lock().unwrap();
        if m.filed.contains(&packet.id) {
            return Ok(None);
        }
        let key = format!("{}/{}", packet.node_id, shared.conversation);
        let local = match m.map.get(&key).filter(|id| self.store.meta(id).is_ok()) {
            Some(id) => id.clone(),
            None => {
                let title = if shared.title.trim().is_empty() { "Conversation".to_string() } else { shared.title.clone() };
                let meta = self.store.create(&format!("{title} ({computer})"))?;
                m.map.insert(key, meta.id.clone());
                meta.id
            }
        };
        // A human message shows as "You (<computer>)", or "You (Signal)" if it came by phone.
        let via = if shared.sender == "human" { Some(shared.via.clone().unwrap_or_else(|| computer.to_string())) } else { None };
        let event = self.store.append_imported(&local, &shared.sender, &shared.kind, shared.to, &shared.text, via)?;
        m.filed.insert(packet.id.clone());
        self.save(&m)?;
        Ok(Some(event))
    }

    fn save(&self, m: &Mirrors) -> Result<(), String> {
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(m).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        crate::store::replace_file(&tmp, &self.path).map_err(|e| e.to_string())
    }
}

/// Starts syncing with `store`. Call once, after the link has opened.
pub fn init(store: Arc<Store>, link_dir: &Path) {
    let _ = SYNC.set(Sync::new(store, link_dir));
    // Files anything saved in the inbox but not filed before the last quit or crash.
    if let Some((s, link)) = SYNC.get().zip(crate::link_handle()) {
        for p in link.inbox() {
            let name = link.peer_name(&p.node_id);
            let _ = s.file(&p, &name);
        }
    }
}

/// Whether `conversation` is a copy of another computer's. Signal leaves those alone: the
/// computer that owns the conversation already relays it.
pub fn is_mirror(conversation: &str) -> bool {
    SYNC.get().is_some_and(|s| s.is_mirror(conversation))
}

/// Sends `e` to the paired computers if it belongs there. Never blocks on the network.
pub fn share(e: &Event) {
    let (Some(s), Some(link)) = (SYNC.get(), crate::link_handle()) else { return };
    if !link.enabled() || !link.has_peers() {
        return;
    }
    if let Some(payload) = s.outgoing(e) {
        let _ = link.send(PacketKind::Chat, payload);
    }
}

/// Files a packet that just arrived from a paired computer.
pub fn receive(packet: &PeerPacket) -> Option<Event> {
    let (s, link) = SYNC.get().zip(crate::link_handle())?;
    s.file(packet, &link.peer_name(&packet.node_id)).ok().flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(node: &str, id: &str, payload: Value) -> PeerPacket {
        PeerPacket {
            version: 1,
            id: id.into(),
            node_id: node.into(),
            timestamp_ms: 0,
            kind: PacketKind::Chat,
            seq: 0,
            payload,
            payload_sha256: String::new(),
            mac: String::new(),
        }
    }

    #[test]
    fn shared_messages_land_once_in_a_mirror_and_never_echo_back() {
        let root = std::env::temp_dir().join(format!("peer-sync-test-{}", uuid::Uuid::new_v4()));
        let (a_dir, b_dir) = (root.join("a"), root.join("b"));
        let a = Sync::new(Arc::new(Store::new(a_dir.join("c")).unwrap()), &a_dir);
        let b = Sync::new(Arc::new(Store::new(b_dir.join("c")).unwrap()), &b_dir);

        let conv = a.store.create("Plans").unwrap();
        let typed = a.store.append(&conv.id, "human", "message", vec![], "hello REQUEST: nothing").unwrap();
        let payload = a.outgoing(&typed).expect("a human message is shared");
        assert!(a.outgoing(&typed).is_none(), "the same event is sent once");

        let p = packet("node-a", "p1", payload);
        let filed = b.file(&p, "MacBook").unwrap().expect("filed");
        assert_eq!(b.store.meta(&filed.conversation).unwrap().title, "Plans (MacBook)");
        assert_eq!(filed.via.as_deref(), Some("MacBook"));
        assert!(filed.requests.is_empty(), "no Approve buttons for another computer's requests");
        assert!(b.file(&p, "MacBook").unwrap().is_none(), "a re-read packet is filed once");

        let reply = a.store.append(&conv.id, "claude", "message", vec![], "hi").unwrap();
        let second = b.file(&packet("node-a", "p2", a.outgoing(&reply).unwrap()), "MacBook").unwrap().unwrap();
        assert_eq!(second.conversation, filed.conversation, "same conversation, same mirror");
        assert_eq!(second.via, None);
        assert!(b.outgoing(&second).is_none(), "mirrors are never sent back");

        let note = a.store.append(&conv.id, "system", "system", vec![], "note").unwrap();
        assert!(a.outgoing(&note).is_none(), "status notes stay local");
        let _ = std::fs::remove_dir_all(&root);
    }
}
