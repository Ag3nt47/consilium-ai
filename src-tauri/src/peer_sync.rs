//! Copies conversations between the user's linked computers (Document 3, first step).
//!
//! Each chat message shown on one computer is sent to every paired computer as a `Chat` packet.
//! The receiver files it in a mirror conversation of its own, titled "<title> (<computer>)",
//! so the two councils never answer the same message: the computer the message was typed on
//! owns the turn. Mirrors are never sent back, which keeps two computers from echoing forever.
//! What the human types in a mirror (an answer to a REQUEST, or anything else) is not answered
//! here: it goes back to the computer that owns the conversation, which records it as
//! "You (<this computer>)" and runs the turn, so requests can be answered from either computer.
//!
//! Joint turns: both computers' AIs answer in one conversation.
//! - The human's message arriving in a mirror starts a turn there, with this computer's council.
//!   Their answers are sent back to the computer that owns the conversation and filed there as
//!   "<AI> (<computer>)", so the owner's AIs see them too (`take_contribution`).
//! - An AI answer from the other computer is read by a turn still running here (`Council::heard`).
//!   If none is, the AIs here get one turn of their own to react, at most once per human message
//!   on each computer (`may_react`), so two councils can never keep each other going.
//! - A conversation started before the computers were linked, or before a new computer was
//!   paired, is sent to it in full (`catch_up`), so its AIs can join in from the next message.
//!
//! Fail-safes, because AIs on two computers may both be working:
//! - Each AI from another computer is named "<AI> (<computer>)", e.g. "Claude (MacBook)", on
//!   screen and in every AI's transcript, so two Claudes are never mistaken for one another.
//! - Imported messages never act here: no REQUEST, ADD_AI, TASK or REMEMBER takes effect. Only a
//!   human message starts a turn freely; an AI's answer starts at most one per human message.
//! - Only the human and known AIs are accepted as senders; text is capped in size.
//! - A computer that sends more than `MAX_PER_MINUTE` messages in a minute is paused for the
//!   rest of that minute, with one note in the mirror, so a runaway loop can't flood this one.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::peer_protocol::{PacketKind, PeerPacket};
use crate::store::{Event, Store};

/// How many recently shared event ids are remembered, so an event shown twice is sent once.
const RECENT: usize = 4096;
/// Most messages accepted from one computer in any minute (by the time it sent them).
const MAX_PER_MINUTE: usize = 40;
/// Longest message text accepted from another computer, in characters.
const MAX_TEXT: usize = 64_000;
const MINUTE_MS: u64 = 60_000;
/// Most earlier messages sent when a conversation is first shared with linked computers.
const HISTORY_LIMIT: usize = 200;
/// Largest total text in one history packet, well under the link's 3 MB line limit.
const HISTORY_BYTES: usize = 1_500_000;
/// How many of this computer's most recent conversations are sent to a newly linked computer.
const CATCH_UP: usize = 20;
/// How often to check for conversations a linked computer hasn't been sent yet.
const CATCH_UP_EVERY: std::time::Duration = std::time::Duration::from_secs(30);

#[derive(Default, Serialize, Deserialize)]
struct Mirrors {
    /// "<node id>/<their conversation id>" -> the local mirror conversation id.
    map: HashMap<String, String>,
    /// Packet ids already filed, so re-reading the inbox at startup files nothing twice.
    #[serde(default)]
    filed: HashSet<String>,
    /// This computer's conversations whose earlier messages have been sent to linked computers.
    #[serde(default)]
    backfilled: HashSet<String>,
    /// This computer's conversations -> the linked computers (node ids) sent all of it.
    #[serde(default)]
    shared_with: HashMap<String, HashSet<String>>,
}

pub struct Sync {
    store: Arc<Store>,
    path: PathBuf,
    mirrors: Mutex<Mirrors>,
    recent: Mutex<(HashSet<String>, VecDeque<String>)>,
    /// Per computer: send times of recently accepted messages, and when it was last paused.
    rate: Mutex<HashMap<String, (VecDeque<u64>, u64)>>,
    /// This computer's node id, to pick out replies meant for it.
    me: Mutex<String>,
    /// Messages typed in a mirror here, as (mirror, text): already shown, so the owner's copy
    /// coming back is skipped once.
    sent: Mutex<Vec<(String, String)>>,
    relay: OnceLock<Relay>,
    wake: OnceLock<Wake>,
    /// Per conversation: the human message the AIs here last started a turn of their own to react
    /// to another computer's answer for (its sequence number).
    reacted: Mutex<HashMap<String, u64>>,
}

/// Why an incoming message was turned away.
#[derive(Debug, PartialEq)]
enum Verdict {
    Accept,
    /// Over the limit; `true` the first time in this pause, so one note is written.
    Flooding(bool),
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
    /// Set when the human typed this in a mirror: the node id of the computer that owns
    /// `conversation`, which is its own id there. Every other computer ignores it.
    #[serde(default)]
    owner: Option<String>,
    /// With kind "history": the conversation's earlier messages, oldest first. Older versions
    /// skip anything that isn't kind "message", so they ignore it.
    #[serde(default)]
    history: Vec<Earlier>,
}

/// One earlier message in a history packet.
#[derive(Serialize, Deserialize)]
struct Earlier {
    id: String,
    sender: String,
    #[serde(default)]
    to: Vec<String>,
    text: String,
    #[serde(default)]
    via: Option<String>,
}

/// Records a message typed in a mirror in the owner's conversation and runs its turn:
/// (conversation, text, targets, sending computer's name).
type Relay = Box<dyn Fn(&str, &str, Vec<String>, &str) -> Result<Event, String> + Send + std::marker::Sync>;

/// Lets this computer's AIs answer a message filed from a linked computer:
/// (conversation, the event, whether they may start a turn of their own). See `Engine::from_linked`.
type Wake = Box<dyn Fn(&str, &Event, &dyn Fn() -> bool) + Send + std::marker::Sync>;

impl Sync {
    pub fn new(store: Arc<Store>, link_dir: &Path) -> Self {
        let path = link_dir.join("mirrors.json");
        let mirrors = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Sync {
            store,
            path,
            mirrors: Mutex::new(mirrors),
            recent: Mutex::new((HashSet::new(), VecDeque::new())),
            rate: Mutex::new(HashMap::new()),
            me: Mutex::new(String::new()),
            sent: Mutex::new(Vec::new()),
            relay: OnceLock::new(),
            wake: OnceLock::new(),
            reacted: Mutex::new(HashMap::new()),
        }
    }

    /// Whether the AIs here may start a turn of their own to react to another computer's AI in
    /// `conversation`: once per human message there.
    fn may_react(&self, conversation: &str) -> bool {
        let Some(human) = self
            .store
            .load(conversation)
            .unwrap_or_default()
            .into_iter()
            .rev()
            .find(|e| e.sender == "human" && e.kind == "message")
        else {
            return false;
        };
        let mut reacted = self.reacted.lock().unwrap();
        if reacted.get(conversation) == Some(&human.seq) {
            return false;
        }
        reacted.insert(conversation.to_string(), human.seq);
        true
    }

    /// Hands `e`, just filed in `conversation` from a linked computer, to this computer's AIs.
    fn notify(&self, conversation: &str, e: &Event) {
        if let Some(wake) = self.wake.get() {
            let human = e.sender == "human";
            wake(conversation, e, &|| human || self.may_react(conversation));
        }
    }

    /// Counts a message `node` sent at `ts`, against the per-minute limit.
    fn admit(&self, node: &str, ts: u64) -> Verdict {
        let mut rate = self.rate.lock().unwrap();
        let (times, paused_at) = rate.entry(node.to_string()).or_default();
        while times.front().is_some_and(|&t| t + MINUTE_MS <= ts) {
            times.pop_front();
        }
        if times.len() >= MAX_PER_MINUTE {
            let first = *paused_at == 0 || *paused_at + MINUTE_MS <= ts;
            if first {
                *paused_at = ts;
            }
            return Verdict::Flooding(first);
        }
        times.push_back(ts);
        Verdict::Accept
    }

    fn is_mirror(&self, conversation: &str) -> bool {
        self.mirrors.lock().unwrap().map.values().any(|v| v == conversation)
    }

    /// The owning computer's node id and its conversation id, if `conversation` is a mirror.
    fn origin(&self, conversation: &str) -> Option<(String, String)> {
        let m = self.mirrors.lock().unwrap();
        let key = m.map.iter().find(|(_, v)| *v == conversation)?.0;
        key.split_once('/').map(|(n, c)| (n.to_string(), c.to_string()))
    }

    /// Shows the human's `text` in `mirror` and returns it with the packet that carries it to
    /// the owning computer. Nothing runs here.
    fn reply(&self, mirror: &str, text: &str, targets: &[String]) -> Result<(Event, Value), String> {
        let (owner, conversation) = self.origin(mirror).ok_or("this isn't another computer's conversation")?;
        let event = self.store.append_imported(mirror, "human", "message", targets.to_vec(), text, None)?;
        self.sent.lock().unwrap().push((mirror.to_string(), text.to_string()));
        let payload = serde_json::to_value(Shared {
            conversation,
            title: String::new(),
            id: event.id.clone(),
            sender: "human".into(),
            kind: "message".into(),
            to: targets.to_vec(),
            text: text.to_string(),
            via: None,
            owner: Some(owner),
            history: vec![],
        })
        .map_err(|e| e.to_string())?;
        Ok((event, payload))
    }

    /// Hands a reply typed on `computer` to this computer's own conversation.
    fn take_reply(&self, packet: &PeerPacket, shared: Shared, computer: &str) -> Result<Option<Event>, String> {
        let mut m = self.mirrors.lock().unwrap();
        if m.filed.contains(&packet.id) {
            return Ok(None);
        }
        if shared.owner.as_deref() != Some(self.me.lock().unwrap().as_str()) {
            return Ok(None);
        }
        let valid = shared.sender == "human"
            && !m.map.values().any(|v| *v == shared.conversation)
            && self.store.meta(&shared.conversation).is_ok()
            && !shared.text.trim().is_empty()
            && shared.text.chars().count() <= MAX_TEXT;
        // Recorded before acting, so a crash can't make the same answer count twice.
        m.filed.insert(packet.id.clone());
        self.save(&m)?;
        drop(m);
        if !valid || self.admit(&packet.node_id, packet.timestamp_ms) != Verdict::Accept {
            return Ok(None);
        }
        match self.relay.get() {
            Some(relay) => relay(&shared.conversation, shared.text.trim(), shared.to, computer).map(Some),
            None => Ok(None),
        }
    }

    /// An AI on `computer` answered in a copy of one of this computer's conversations: files the
    /// answer there as "<AI> (<computer>)" for the AIs here to see.
    fn take_contribution(&self, packet: &PeerPacket, shared: Shared, computer: &str) -> Result<Option<Event>, String> {
        let mut m = self.mirrors.lock().unwrap();
        if m.filed.contains(&packet.id) || shared.owner.as_deref() != Some(self.me.lock().unwrap().as_str()) {
            return Ok(None);
        }
        let seen = format!("ev:{}/{}", packet.node_id, shared.id);
        let valid = shared.kind == "message"
            && shared.sender != "human"
            && known_sender(&shared.sender)
            && !m.filed.contains(&seen)
            && !m.map.values().any(|v| *v == shared.conversation)
            && self.store.meta(&shared.conversation).is_ok()
            && !shared.text.trim().is_empty();
        // Recorded before acting, so a crash can't file the same answer twice.
        m.filed.insert(packet.id.clone());
        m.filed.insert(seen);
        self.save(&m)?;
        drop(m);
        if !valid || self.admit(&packet.node_id, packet.timestamp_ms) != Verdict::Accept {
            return Ok(None);
        }
        let via = shown_via(&shared.sender, &None, computer);
        let event = self.store.append_imported(&shared.conversation, &shared.sender, "message", shared.to, &capped(shared.text), via)?;
        self.notify(&shared.conversation, &event);
        Ok(Some(event))
    }

    /// The payload to send for `e`, or None if it shouldn't leave this computer.
    fn outgoing(&self, e: &Event) -> Option<Value> {
        if e.kind != "message" || e.text.trim().is_empty() {
            return None;
        }
        // Another computer's AI, filed here: it has already been everywhere it needs to be.
        if e.sender != "human" && e.via.is_some() {
            return None;
        }
        let origin = self.origin(&e.conversation);
        // In a copy, only this computer's own AIs' answers go back, to the owner; what the human
        // types there goes through `reply`, and what was copied in is never sent back.
        if origin.is_some() && (e.sender == "human" || !known_sender(&e.sender)) {
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
        if let Some((owner, conversation)) = origin {
            return serde_json::to_value(Shared {
                conversation,
                title: String::new(),
                id: e.id.clone(),
                sender: e.sender.clone(),
                kind: e.kind.clone(),
                to: e.to.clone(),
                text: e.text.clone(),
                via: None,
                owner: Some(owner),
                history: vec![],
            })
            .ok();
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
            owner: None,
            history: vec![],
        })
        .ok()
    }

    /// The first time a conversation of this computer's is shared, its earlier messages (up to
    /// `HISTORY_LIMIT`, newest kept), as one packet sent before `e`; afterwards None.
    fn history_for(&self, e: &Event) -> Option<Value> {
        if e.kind != "message" || self.is_mirror(&e.conversation) {
            return None;
        }
        {
            let mut m = self.mirrors.lock().unwrap();
            if !m.backfilled.insert(e.conversation.clone()) {
                return None;
            }
            let _ = self.save(&m);
        }
        self.history(&e.conversation, Some(&e.id), &format!("history-{}", e.id))
    }

    /// A history packet with `conversation`'s earlier messages (up to `HISTORY_LIMIT`, newest
    /// kept), leaving out `except`; None if there are none.
    fn history(&self, conversation: &str, except: Option<&str>, id: &str) -> Option<Value> {
        let events = self.store.load(conversation).ok()?;
        let mut earlier: Vec<Earlier> = Vec::new();
        let mut bytes = 0;
        for ev in events.iter().rev().filter(|ev| ev.kind == "message" && Some(ev.id.as_str()) != except && !ev.text.trim().is_empty()) {
            bytes += ev.text.len();
            if earlier.len() >= HISTORY_LIMIT || bytes > HISTORY_BYTES {
                break;
            }
            earlier.push(Earlier { id: ev.id.clone(), sender: ev.sender.clone(), to: ev.to.clone(), text: ev.text.clone(), via: ev.via.clone() });
        }
        if earlier.is_empty() {
            return None;
        }
        earlier.reverse();
        let title = self.store.meta(conversation).map(|m| m.title).unwrap_or_default();
        serde_json::to_value(Shared {
            conversation: conversation.to_string(),
            title,
            id: id.to_string(),
            sender: "human".into(),
            kind: "history".into(),
            to: vec![],
            text: String::new(),
            via: None,
            owner: None,
            history: earlier,
        })
        .ok()
    }

    /// History packets for this computer's recent conversations that one of `peers` (node ids of
    /// the paired computers) hasn't been sent in full, so a conversation started before they
    /// were linked shows up there too. Each is sent once per computer; the receiver files every
    /// message once however often it arrives.
    fn catch_up(&self, peers: &[String]) -> Vec<Value> {
        if peers.is_empty() {
            return vec![];
        }
        let mut convs: Vec<_> = self.store.list().into_iter().filter(|c| !self.is_mirror(&c.id)).collect();
        convs.sort_by_key(|c| std::cmp::Reverse(c.updated));
        let mut out = Vec::new();
        for c in convs.into_iter().take(CATCH_UP) {
            let missing = {
                let m = self.mirrors.lock().unwrap();
                let had = m.shared_with.get(&c.id);
                peers.iter().any(|p| !had.is_some_and(|h| h.contains(p)))
            };
            if !missing {
                continue;
            }
            if let Some(packet) = self.history(&c.id, None, &format!("catch-up-{}-{}", c.id, crate::store::now_ms())) {
                out.push(packet);
            }
            let mut m = self.mirrors.lock().unwrap();
            m.shared_with.entry(c.id.clone()).or_default().extend(peers.iter().cloned());
            m.backfilled.insert(c.id);
            let _ = self.save(&m);
        }
        out
    }

    /// Files a received chat packet in its mirror conversation. Returns the new event, if any.
    /// `computer` is the sending computer's name.
    pub fn file(&self, packet: &PeerPacket, computer: &str) -> Result<Option<Event>, String> {
        if packet.kind != PacketKind::Chat {
            return Ok(None);
        }
        let shared: Shared = serde_json::from_value(packet.payload.clone()).map_err(|e| e.to_string())?;
        if shared.owner.is_some() {
            if shared.sender == "human" {
                return self.take_reply(packet, shared, computer);
            }
            return self.take_contribution(packet, shared, computer);
        }
        if !matches!(shared.kind.as_str(), "message" | "history") || !crate::store::valid_id(&shared.conversation) || !known_sender(&shared.sender) {
            return Ok(None);
        }
        let mut m = self.mirrors.lock().unwrap();
        if m.filed.contains(&packet.id) {
            return Ok(None);
        }
        let flooding = self.admit(&packet.node_id, packet.timestamp_ms);
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
        if let Verdict::Flooding(first) = flooding {
            // Dropped for good: marked filed so a restart doesn't let the flood in after all.
            m.filed.insert(packet.id.clone());
            self.save(&m)?;
            if first {
                let note = format!(
                    "Paused copying from {computer} for a minute: it sent more than {MAX_PER_MINUTE} messages in a minute, \
                     which usually means AIs are caught in a loop. Messages it sent during the pause are not copied here."
                );
                self.store.append_imported(&local, crate::council::CONSILIUM, "system", vec![], &note, None)?;
            }
            return Ok(None);
        }
        if shared.kind == "history" {
            return self.file_history(&mut m, &packet.node_id, &local, shared.history, computer);
        }
        let seen = format!("ev:{}/{}", packet.node_id, shared.id);
        if m.filed.contains(&seen) {
            m.filed.insert(packet.id.clone());
            self.save(&m)?;
            return Ok(None);
        }
        // The human's own reply typed here, coming back from the owner: already shown.
        if shared.sender == "human" {
            let mut sent = self.sent.lock().unwrap();
            if let Some(i) = sent.iter().position(|(c, t)| *c == local && *t == shared.text.trim()) {
                sent.remove(i);
                m.filed.insert(packet.id.clone());
                self.save(&m)?;
                return Ok(None);
            }
        }
        let via = shown_via(&shared.sender, &shared.via, computer);
        let event = self.store.append_imported(&local, &shared.sender, &shared.kind, shared.to, &capped(shared.text), via)?;
        m.filed.insert(packet.id.clone());
        m.filed.insert(seen);
        self.save(&m)?;
        drop(m);
        // The AIs here answer the human's message too, and read the other computer's AIs.
        self.notify(&local, &event);
        Ok(Some(event))
    }

    /// Files a conversation's earlier messages in its mirror, skipping any already there (by id,
    /// or by sender and text for ones copied before ids were recorded). Returns the last one filed.
    fn file_history(&self, m: &mut Mirrors, node: &str, local: &str, earlier: Vec<Earlier>, computer: &str) -> Result<Option<Event>, String> {
        let have: HashSet<(String, String)> =
            self.store.load(local).unwrap_or_default().into_iter().map(|e| (e.sender, e.text)).collect();
        let mut last = None;
        for item in earlier.into_iter().take(HISTORY_LIMIT) {
            let seen = format!("ev:{node}/{}", item.id);
            let text = capped(item.text);
            if !known_sender(&item.sender) || m.filed.contains(&seen) || have.contains(&(item.sender.clone(), text.clone())) {
                continue;
            }
            let via = shown_via(&item.sender, &item.via, computer);
            last = Some(self.store.append_imported(local, &item.sender, "message", item.to, &text, via)?);
            m.filed.insert(seen);
        }
        self.save(m)?;
        Ok(last)
    }

    fn save(&self, m: &Mirrors) -> Result<(), String> {
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(m).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        crate::store::replace_file(&tmp, &self.path).map_err(|e| e.to_string())
    }
}

/// A human message shows as "You (<computer>)", or "You (Signal)" if it came by phone.
/// An AI shows as "<AI> (<computer>)", so it's never taken for this computer's own AI.
fn shown_via(sender: &str, via: &Option<String>, computer: &str) -> Option<String> {
    match (sender, via) {
        ("human", Some(v)) => Some(v.clone()),
        _ => Some(computer.to_string()),
    }
}

/// `text`, cut to `MAX_TEXT` characters with a note if it was longer.
fn capped(text: String) -> String {
    if text.chars().count() <= MAX_TEXT {
        return text;
    }
    let mut t: String = text.chars().take(MAX_TEXT).collect();
    t.push_str("\n\n… cut short here: the message was too long to copy between computers.");
    t
}

/// Only the human and known AIs may appear in a copied conversation.
fn known_sender(sender: &str) -> bool {
    sender == "human" || crate::agents::all().into_iter().any(|a| a.id == sender)
}

/// Starts syncing with `engine`'s store. Call once, after the link has opened.
pub fn init(engine: Arc<crate::engine::Engine>, link_dir: &Path) {
    let s = Sync::new(engine.store.clone(), link_dir);
    if let Some(link) = crate::link_handle() {
        *s.me.lock().unwrap() = link.node_id();
    }
    let relaying = engine.clone();
    let _ = s.relay.set(Box::new(move |conversation: &str, text: &str, targets: Vec<String>, computer: &str| {
        relaying.send_via(conversation, text, vec![], targets, 1, Some(computer))
    }));
    let waking = engine.clone();
    let _ = s.wake.set(Box::new(move |conversation: &str, e: &Event, may_start: &dyn Fn() -> bool| {
        waking.from_linked(conversation, e, may_start)
    }));
    // Sends conversations a linked computer doesn't have yet, now and whenever one is paired.
    engine.spawn_task(Box::pin(async {
        loop {
            if let (Some(s), Some(link)) = (SYNC.get(), crate::link_handle()) {
                if link.enabled() && link.has_peers() {
                    let peers: Vec<String> = link.status().peers.into_iter().map(|p| p.node_id).collect();
                    for packet in s.catch_up(&peers) {
                        let _ = link.send(PacketKind::Chat, packet);
                    }
                }
            }
            tokio::time::sleep(CATCH_UP_EVERY).await;
        }
    }));
    let _ = SYNC.set(s);
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
    if let Some(history) = s.history_for(e) {
        let _ = link.send(PacketKind::Chat, history);
    }
    if let Some(payload) = s.outgoing(e) {
        let _ = link.send(PacketKind::Chat, payload);
    }
}

/// Sends what the human typed in the mirror `conversation` to the computer that owns it, and
/// returns it as shown here. That computer answers, and so do this computer's AIs (see
/// `Engine::from_linked`), whose answers go back to it.
pub fn reply(conversation: &str, text: &str, targets: &[String]) -> Result<Event, String> {
    let s = SYNC.get().ok_or("linking isn't running")?;
    let link = crate::link_handle().filter(|l| l.enabled() && l.has_peers());
    let Some(link) = link else {
        return Err("This conversation belongs to another computer, which isn't linked right now. \
                    Turn linking on in Settings, or answer on that computer."
            .into());
    };
    let (event, payload) = s.reply(conversation, text, targets)?;
    link.send(PacketKind::Chat, payload)?;
    Ok(event)
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
        assert!(b.file(&p, "MacBook").unwrap().is_none(), "a re-read packet is filed once");

        let reply = a.store.append(&conv.id, "claude", "message", vec![], "hi").unwrap();
        let second = b.file(&packet("node-a", "p2", a.outgoing(&reply).unwrap()), "MacBook").unwrap().unwrap();
        assert_eq!(second.conversation, filed.conversation, "same conversation, same mirror");
        assert_eq!(second.via.as_deref(), Some("MacBook"), "another computer's Claude is named for it");
        assert!(b.outgoing(&second).is_none(), "mirrors are never sent back");

        let note = a.store.append(&conv.id, "system", "system", vec![], "note").unwrap();
        assert!(a.outgoing(&note).is_none(), "status notes stay local");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn earlier_messages_arrive_once_when_a_conversation_is_first_shared() {
        let root = std::env::temp_dir().join(format!("peer-sync-history-{}", uuid::Uuid::new_v4()));
        let (a_dir, b_dir) = (root.join("a"), root.join("b"));
        let a = Sync::new(Arc::new(Store::new(a_dir.join("c")).unwrap()), &a_dir);
        let b = Sync::new(Arc::new(Store::new(b_dir.join("c")).unwrap()), &b_dir);
        let conv = a.store.create("Plans").unwrap();
        a.store.append(&conv.id, "human", "message", vec![], "first").unwrap();
        a.store.append(&conv.id, "system", "system", vec![], "a note").unwrap();
        let old = a.store.append(&conv.id, "claude", "message", vec![], "second").unwrap();
        // "second" was already copied live before this version: it mustn't appear twice.
        b.file(&packet("node-a", "p0", a.outgoing(&old).unwrap()), "MacBook").unwrap().unwrap();

        let now = a.store.append(&conv.id, "human", "message", vec![], "third").unwrap();
        let history = a.history_for(&now).expect("earlier messages go first");
        assert!(a.history_for(&now).is_none(), "history is sent once per conversation");
        b.file(&packet("node-a", "h1", history.clone()), "MacBook").unwrap();
        b.file(&packet("node-a", "h2", history), "MacBook").unwrap();
        b.file(&packet("node-a", "p1", a.outgoing(&now).unwrap()), "MacBook").unwrap().unwrap();

        let mirror = b.mirrors.lock().unwrap().map.values().next().cloned().unwrap();
        let texts: Vec<String> = b.store.load(&mirror).unwrap().into_iter().map(|e| e.text).collect();
        assert_eq!(texts, vec!["second", "first", "third"], "each message once, no status notes");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_answer_typed_in_a_mirror_runs_on_the_owner_and_shows_once() {
        let root = std::env::temp_dir().join(format!("peer-sync-reply-{}", uuid::Uuid::new_v4()));
        let (a_dir, b_dir) = (root.join("a"), root.join("b"));
        let a = Sync::new(Arc::new(Store::new(a_dir.join("c")).unwrap()), &a_dir);
        let b = Sync::new(Arc::new(Store::new(b_dir.join("c")).unwrap()), &b_dir);
        *a.me.lock().unwrap() = "node-a".into();
        *b.me.lock().unwrap() = "node-b".into();
        let store = a.store.clone();
        let _ = a.relay.set(Box::new(move |c: &str, t: &str, to: Vec<String>, pc: &str| {
            store.append_with_attachments_via(c, to, t, vec![], Some(pc))
        }));

        let conv = a.store.create("Plans").unwrap();
        let ask = a.store.append(&conv.id, "claude", "message", vec![], "REQUEST: May I build?").unwrap();
        let mirror = b.file(&packet("node-a", "p1", a.outgoing(&ask).unwrap()), "MacBook").unwrap().unwrap();

        assert_eq!(mirror.requests, vec!["May I build?".to_string()], "requests keep their buttons in the mirror");
        let (shown, payload) = b.reply(&mirror.conversation, "Approved: May I build?", &["claude".into()]).unwrap();
        assert_eq!(shown.sender, "human");
        assert!(b.outgoing(&shown).is_none(), "the reply travels only as the addressed packet");

        let theirs = PeerPacket { node_id: "node-b".into(), ..packet("node-b", "r1", payload.clone()) };
        let elsewhere = Sync::new(Arc::new(Store::new(root.join("c3")).unwrap()), &root.join("c3l"));
        *elsewhere.me.lock().unwrap() = "node-c".into();
        assert!(elsewhere.file(&theirs, "PC").unwrap().is_none(), "a third computer ignores it");

        let taken = a.file(&theirs, "PC").unwrap().expect("the owner records it");
        assert_eq!((taken.conversation.as_str(), taken.via.as_deref()), (conv.id.as_str(), Some("PC")));
        assert!(a.file(&theirs, "PC").unwrap().is_none(), "an answer counts once");

        let back = b.file(&packet("node-a", "p2", a.outgoing(&taken).unwrap()), "MacBook").unwrap();
        assert!(back.is_none(), "the human's own reply isn't shown twice in the mirror");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn floods_unknown_senders_and_huge_messages_are_held_back() {
        let root = std::env::temp_dir().join(format!("peer-sync-guard-{}", uuid::Uuid::new_v4()));
        let b = Sync::new(Arc::new(Store::new(root.join("c")).unwrap()), &root);
        let msg = |sender: &str, text: &str| {
            serde_json::json!({"conversation": "c0ffee00-0000-4000-8000-000000000000", "title": "Loop", "id": uuid::Uuid::new_v4().to_string(),
                "sender": sender, "kind": "message", "text": text})
        };
        let at = |id: &str, ts: u64, payload: Value| PeerPacket { timestamp_ms: ts, ..packet("node-a", id, payload) };

        assert!(b.file(&at("u", 1, msg("someone", "hi")), "PC").unwrap().is_none(), "unknown sender refused");

        let mut accepted = 0;
        for i in 0..MAX_PER_MINUTE + 5 {
            if b.file(&at(&format!("f{i}"), 1_000 + i as u64, msg("claude", "again")), "PC").unwrap().is_some() {
                accepted += 1;
            }
        }
        assert_eq!(accepted, MAX_PER_MINUTE, "a flood is cut off at the limit");
        let conv = b.mirrors.lock().unwrap().map.values().next().cloned().unwrap();
        let notes = b.store.load(&conv).unwrap().into_iter().filter(|e| e.kind == "system").count();
        assert_eq!(notes, 1, "one pause note, not one per dropped message");
        assert!(b.file(&at("later", 1_000 + MINUTE_MS + 100, msg("claude", "ok")), "PC").unwrap().is_some(), "resumes next minute");

        let big = "a".repeat(MAX_TEXT + 10);
        let e = b.file(&at("big", 10 * MINUTE_MS, msg("gemini", &big)), "PC").unwrap().unwrap();
        assert!(e.text.chars().count() < MAX_TEXT + 200 && e.text.contains("cut short"));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Records what the AIs on one computer were woken for: (conversation, sender, may start).
    fn record_wakes(s: &Sync) -> Arc<Mutex<Vec<(String, String, bool)>>> {
        let log = Arc::new(Mutex::new(Vec::new()));
        let l = log.clone();
        let _ = s.wake.set(Box::new(move |c: &str, e: &Event, may_start: &dyn Fn() -> bool| {
            l.lock().unwrap().push((c.to_string(), e.sender.clone(), may_start()));
        }));
        log
    }

    #[test]
    fn both_computers_ais_answer_in_one_conversation_without_looping() {
        let root = std::env::temp_dir().join(format!("peer-sync-joint-{}", uuid::Uuid::new_v4()));
        let (a_dir, b_dir) = (root.join("a"), root.join("b"));
        let a = Sync::new(Arc::new(Store::new(a_dir.join("c")).unwrap()), &a_dir);
        let b = Sync::new(Arc::new(Store::new(b_dir.join("c")).unwrap()), &b_dir);
        *a.me.lock().unwrap() = "node-a".into();
        *b.me.lock().unwrap() = "node-b".into();
        let (a_woke, b_woke) = (record_wakes(&a), record_wakes(&b));

        // The human asks on A; B's AIs are woken to answer it in their copy.
        let conv = a.store.create("Plans").unwrap();
        let ask = a.store.append(&conv.id, "human", "message", vec![], "How should we build it?").unwrap();
        let mirror = b.file(&packet("node-a", "p1", a.outgoing(&ask).unwrap()), "PC").unwrap().unwrap().conversation;
        assert_eq!(b_woke.lock().unwrap().clone(), vec![(mirror.clone(), "human".to_string(), true)]);

        // B's Claude answers in the copy; the answer goes back to A's own conversation, named for B.
        let answer = b.store.append(&mirror, "claude", "message", vec![], "Start with the supervisor.").unwrap();
        let back = b.outgoing(&answer).expect("an answer made in a copy goes back to its owner");
        let filed = a.file(&packet("node-b", "c1", back.clone()), "MacBook").unwrap().expect("filed on the owner");
        assert_eq!((filed.conversation.as_str(), filed.sender.as_str(), filed.via.as_deref()), (conv.id.as_str(), "claude", Some("MacBook")));
        assert!(a.file(&packet("node-b", "c1", back), "MacBook").unwrap().is_none(), "filed once");
        assert!(a.outgoing(&filed).is_none(), "never echoed back to the computer it came from");
        assert_eq!(a_woke.lock().unwrap().last().cloned(), Some((conv.id.clone(), "claude".to_string(), true)));

        // A second answer from B for the same human message can't start another turn on A.
        let more = b.store.append(&mirror, "gemini", "message", vec![], "Agreed.").unwrap();
        a.file(&packet("node-b", "c2", b.outgoing(&more).unwrap()), "MacBook").unwrap().unwrap();
        assert_eq!(a_woke.lock().unwrap().last().map(|w| w.2), Some(false), "one reaction per human message");

        // A's own answer reaches B's copy; B may react once too, then no more.
        let reply = a.store.append(&conv.id, "gpt", "message", vec![], "I'll take the tests.").unwrap();
        b.file(&packet("node-a", "p2", a.outgoing(&reply).unwrap()), "PC").unwrap().unwrap();
        let again = a.store.append(&conv.id, "claude", "message", vec![], "And I the docs.").unwrap();
        b.file(&packet("node-a", "p3", a.outgoing(&again).unwrap()), "PC").unwrap().unwrap();
        let reacts: Vec<bool> = b_woke.lock().unwrap().iter().skip(1).map(|w| w.2).collect();
        assert_eq!(reacts, vec![true, false]);
        // What was copied into B's copy never goes anywhere from B.
        let copied = b.store.load(&mirror).unwrap();
        assert!(copied.iter().filter(|e| e.via.is_some()).all(|e| b.outgoing(e).is_none()));

        // The human's next message lets each side react once more.
        let next = a.store.append(&conv.id, "human", "message", vec![], "Go ahead.").unwrap();
        b.file(&packet("node-a", "p4", a.outgoing(&next).unwrap()), "PC").unwrap().unwrap();
        let late = b.store.append(&mirror, "claude", "message", vec![], "On it.").unwrap();
        a.file(&packet("node-b", "c3", b.outgoing(&late).unwrap()), "MacBook").unwrap().unwrap();
        assert_eq!(a_woke.lock().unwrap().last().map(|w| w.2), Some(true));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn answers_only_go_to_the_owner_and_only_from_known_ais() {
        let root = std::env::temp_dir().join(format!("peer-sync-owner-{}", uuid::Uuid::new_v4()));
        let (a_dir, b_dir) = (root.join("a"), root.join("b"));
        let a = Sync::new(Arc::new(Store::new(a_dir.join("c")).unwrap()), &a_dir);
        let b = Sync::new(Arc::new(Store::new(b_dir.join("c")).unwrap()), &b_dir);
        *a.me.lock().unwrap() = "node-a".into();
        let conv = a.store.create("Plans").unwrap();
        let ask = a.store.append(&conv.id, "human", "message", vec![], "hi").unwrap();
        let mirror = b.file(&packet("node-a", "p1", a.outgoing(&ask).unwrap()), "PC").unwrap().unwrap().conversation;
        let answer = b.store.append(&mirror, "claude", "message", vec![], "hello").unwrap();
        let mut forged = b.outgoing(&answer).unwrap();
        forged["owner"] = "node-x".into();
        assert!(a.file(&packet("node-b", "c1", forged), "MacBook").unwrap().is_none(), "meant for another computer");
        let mut stranger = b.outgoing(&b.store.append(&mirror, "claude", "message", vec![], "again").unwrap()).unwrap();
        stranger["sender"] = "intruder".into();
        assert!(a.file(&packet("node-b", "c2", stranger), "MacBook").unwrap().is_none(), "unknown sender");
        let note = b.store.append(&mirror, "consilium", "system", vec![], "a note").unwrap();
        assert!(b.outgoing(&note).is_none(), "status notes stay local");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_conversation_from_before_linking_reaches_each_computer_once() {
        let root = std::env::temp_dir().join(format!("peer-sync-catchup-{}", uuid::Uuid::new_v4()));
        let (a_dir, b_dir) = (root.join("a"), root.join("b"));
        let a = Sync::new(Arc::new(Store::new(a_dir.join("c")).unwrap()), &a_dir);
        let b = Sync::new(Arc::new(Store::new(b_dir.join("c")).unwrap()), &b_dir);
        let conv = a.store.create("Old plans").unwrap();
        a.store.append(&conv.id, "human", "message", vec![], "first").unwrap();
        a.store.append(&conv.id, "claude", "message", vec![], "second").unwrap();
        let sent = a.catch_up(&["node-b".to_string()]);
        assert_eq!(sent.len(), 1);
        assert!(a.catch_up(&["node-b".to_string()]).is_empty(), "sent once per computer");
        assert_eq!(a.catch_up(&["node-b".to_string(), "node-c".to_string()]).len(), 1, "a newly paired computer gets it too");
        b.file(&packet("node-a", "h1", sent[0].clone()), "PC").unwrap();
        b.file(&packet("node-a", "h2", sent[0].clone()), "PC").unwrap();
        let mirror = b.mirrors.lock().unwrap().map.values().next().cloned().unwrap();
        assert_eq!(b.store.meta(&mirror).unwrap().title, "Old plans (PC)");
        let texts: Vec<String> = b.store.load(&mirror).unwrap().into_iter().map(|e| e.text).collect();
        assert_eq!(texts, vec!["first", "second"], "each message once");
        assert!(b.catch_up(&["node-a".to_string()]).is_empty(), "copies aren't sent back");
        let _ = std::fs::remove_dir_all(&root);
    }
}
