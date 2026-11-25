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
//! - Imported messages never act here: no REQUEST, ADD_AI or REMEMBER takes effect. Only a
//!   human message starts a turn freely; an AI's answer starts at most one per human message.
//!   The one exception is TASK lines in an answer filed in the conversation's owner: one task board
//!   per conversation is kept there for every computer (`take_contribution`), and each change is
//!   sent to the copies as a snapshot (`share_board`, `take_board`), so task numbers match
//!   everywhere and two computers' AIs don't take the same work.
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
/// Turns another computer's AIs may start here by addressing AIs here by handle, per human message.
const MENTION_TURNS: u32 = 3;
/// How many of this computer's most recent conversations are sent to a newly linked computer.
const CATCH_UP: usize = 20;
/// How often to check for conversations a linked computer hasn't been sent yet.
const CATCH_UP_EVERY: std::time::Duration = std::time::Duration::from_secs(30);
/// `filed` marker for a packet whose message is being written: its retry checks the conversation first.
const PENDING: &str = "pending-event:";
const LEGACY_PENDING: &str = "pending:";
/// How many events to inspect when recovering a pending write from an older version.
const RECHECK_EVENTS: usize = 50;

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
    /// Mirrors whose board comes from the owner: mirror id -> when the newest snapshot was taken.
    #[serde(default)]
    boards: HashMap<String, u64>,
}

/// The text of a "tasks" packet: the owner's board for the conversation.
#[derive(Serialize, Deserialize)]
struct BoardSnapshot {
    at: u64,
    board: crate::tasks::Board,
}

pub struct Sync {
    store: Arc<Store>,
    lan_notes: crate::lan_notes::Inbox,
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
    /// Per conversation: the human message, and how many turns handles have started since it.
    addressed: Mutex<HashMap<String, (u64, u32)>>,
    /// Host resources reported by authenticated peer node ID.
    resources: Mutex<HashMap<String, PeerResourceReport>>,
}

#[derive(Clone, Serialize)]
pub struct PeerResourceReport {
    #[serde(flatten)]
    resources: crate::resources::HostResources,
    supports_request_id: bool,
    request_id: Option<String>,
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
            lan_notes: crate::lan_notes::Inbox::open(link_dir),
            path,
            mirrors: Mutex::new(mirrors),
            recent: Mutex::new((HashSet::new(), VecDeque::new())),
            rate: Mutex::new(HashMap::new()),
            me: Mutex::new(String::new()),
            sent: Mutex::new(Vec::new()),
            relay: OnceLock::new(),
            wake: OnceLock::new(),
            reacted: Mutex::new(HashMap::new()),
            addressed: Mutex::new(HashMap::new()),
            resources: Mutex::new(HashMap::new()),
        }
    }

    /// The newest human message in `conversation` (its sequence number).
    fn last_human(&self, conversation: &str) -> Option<u64> {
        self.store.load(conversation).unwrap_or_default().into_iter().rev().find(|e| e.sender == "human" && e.kind == "message").map(|e| e.seq)
    }

    /// Whether another computer's AI addressing AIs here by handle may start a turn for them:
    /// up to `MENTION_TURNS` times per human message, so two councils can't keep calling each other.
    fn may_address(&self, conversation: &str) -> bool {
        let Some(human) = self.last_human(conversation) else { return false };
        let mut a = self.addressed.lock().unwrap();
        let entry = a.entry(conversation.to_string()).or_insert((human, 0));
        if entry.0 != human {
            *entry = (human, 0);
        }
        if entry.1 >= MENTION_TURNS {
            return false;
        }
        entry.1 += 1;
        true
    }

    /// Whether the AIs here may start a turn of their own to react to another computer's AI in
    /// `conversation`: once per human message there.
    fn may_react(&self, conversation: &str) -> bool {
        let Some(human) = self.last_human(conversation) else { return false };
        let mut reacted = self.reacted.lock().unwrap();
        if reacted.get(conversation) == Some(&human) {
            return false;
        }
        reacted.insert(conversation.to_string(), human);
        true
    }

    /// Hands `e`, just filed in `conversation` from a linked computer, to this computer's AIs.
    fn notify(&self, conversation: &str, e: &Event) {
        if let Some(wake) = self.wake.get() {
            let human = e.sender == "human";
            let addressed = !human && !crate::prompt::mentioned_here(&e.text).is_empty();
            wake(conversation, e, &|| human || if addressed { self.may_address(conversation) } else { self.may_react(conversation) });
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
        let event = self.store.append_imported(mirror, "human", "message", targets.to_vec(), text, None, None)?;
        self.sent.lock().unwrap().push((mirror.to_string(), text.to_string()));
        // Sent here, so `outgoing` doesn't send it a second time when the window shows it.
        self.first_time(&event.id);
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

    /// `computer` is installing an update and restarting: this computer follows (see
    /// `follow::announced`), and a note in its newest conversation says so.
    fn take_update(&self, packet: &PeerPacket, shared: Shared, computer: &str) -> Result<Option<Event>, String> {
        {
            let mut m = self.mirrors.lock().unwrap();
            if !m.filed.insert(packet.id.clone()) {
                return Ok(None);
            }
            self.save(&m)?;
        }
        let Some(build) = serde_json::from_str::<Value>(&shared.text).ok().and_then(|v| crate::build_id::Build::from_peer(&v)) else {
            return Ok(None);
        };
        let this = crate::build_id::Build::this();
        let what = if !build.newer_than(&this) {
            format!("This computer already runs {}, so it stays as it is.", this.label())
        } else if build.reproducible() {
            "This computer is preparing the same build now and installs it as soon as it is ready.".to_string()
        } else {
            "This computer is checking GitHub for that release and installs it as soon as it is downloaded.".to_string()
        };
        crate::follow::announced(computer, build.clone());
        let Some(conv) = self.store.list().into_iter().find(|c| !self.is_mirror(&c.id)) else { return Ok(None) };
        let note = format!("{computer} is installing Consilium {} and restarting. {what}", build.label());
        self.store.append(&conv.id, crate::council::CONSILIUM, "system", vec![], &note).map(Some)
    }

    /// The human on `computer` pressed Request Update: this computer checks GitHub now and installs
    /// the newest staged build as soon as it has one, as if its own button had been pressed.
    fn take_update_request(&self, packet: &PeerPacket, computer: &str) -> Result<Option<Event>, String> {
        {
            let mut m = self.mirrors.lock().unwrap();
            if !m.filed.insert(packet.id.clone()) {
                return Ok(None);
            }
            self.save(&m)?;
        }
        crate::follow::install_requested(crate::store::now_ms());
        let Some(conv) = self.store.list().into_iter().find(|c| !self.is_mirror(&c.id)) else { return Ok(None) };
        let note = format!(
            "{computer} asked every linked computer to update Consilium. This computer (now {}) checks GitHub \
             now and installs any newer build it has staged or downloads in the next 30 minutes, then restarts; \
             it waits for a running command to finish first.",
            crate::build_id::Build::this().label()
        );
        self.store.append(&conv.id, crate::council::CONSILIUM, "system", vec![], &note).map(Some)
    }

    /// A linked computer requested a restart of Consilium.
    fn take_restart_request(&self, packet: &PeerPacket, shared: Shared, computer: &str) -> Result<Option<Event>, String> {
        let my_name = {
            crate::link_handle()
                .map(|l| l.status().name)
                .unwrap_or_default()
        };
        let target = shared.text.trim();
        let applies = target.is_empty()
            || target.eq_ignore_ascii_case("all")
            || (!my_name.is_empty() && target.eq_ignore_ascii_case(&my_name))
            || target.eq_ignore_ascii_case(&self.me.lock().unwrap());
        if !applies {
            return Ok(None);
        }
        {
            let mut m = self.mirrors.lock().unwrap();
            if !m.filed.insert(packet.id.clone()) {
                return Ok(None);
            }
            self.save(&m)?;
        }
        let Some(conv) = self.store.list().into_iter().find(|c| !self.is_mirror(&c.id)) else {
            crate::restart_this_computer();
            return Ok(None);
        };
        let note = format!(
            "{computer} requested a restart of Consilium on this computer. Restarting now; in-progress work will resume after."
        );
        let event = self.store.append(&conv.id, crate::council::CONSILIUM, "system", vec![], &note).map(Some)?;
        crate::restart_this_computer();
        Ok(event)
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
        // Pending while it's written and filed after, so a crash in between neither loses the
        // answer nor, when the inbox is replayed, files it twice (see `already_written`).
        if !valid || self.admit(&packet.node_id, packet.timestamp_ms) != Verdict::Accept {
            self.settle(&mut m, &packet.id, Some(seen))?;
            return Ok(None);
        }
        let via = shown_via(&shared.sender, &None, computer);
        let text = capped(shared.text);
        let pending = format!("{PENDING}{}", packet.id);
        if self.already_written(&m, &packet.id, &shared.conversation, &seen, &shared.sender, &text)? {
            self.settle(&mut m, &packet.id, Some(seen))?;
            return Ok(None);
        }
        m.filed.remove(&format!("{LEGACY_PENDING}{}", packet.id));
        m.filed.insert(pending);
        self.save(&m)?;
        let event = self.store.append_imported(&shared.conversation, &shared.sender, "message", shared.to, &text, via, Some(seen.clone()))?;
        self.settle(&mut m, &packet.id, Some(seen))?;
        drop(m);
        // The shared board lives here: the other computer's AI's TASK lines count, under its handle.
        let ops = crate::tasks::task_lines(&text);
        let who = format!("{}-{}", shared.sender, crate::prompt::slug(computer));
        if let Ok(Some(note)) = crate::tasks::apply(self.store.dir(), &shared.conversation, &who, &ops) {
            let _ = self.store.append(&shared.conversation, crate::council::CONSILIUM, "system", vec![], &note);
            if let Some(packet) = self.board_packet(&shared.conversation) {
                if let Some(link) = crate::link_handle() {
                    let _ = link.send(PacketKind::Chat, packet);
                }
            }
        }
        self.notify(&shared.conversation, &event);
        Ok(Some(event))
    }

    /// The packet carrying this computer's board for `conversation` to its copies, if it has been
    /// shared with a linked computer.
    fn board_packet(&self, conversation: &str) -> Option<Value> {
        {
            let m = self.mirrors.lock().unwrap();
            if m.map.values().any(|v| v == conversation) || !(m.backfilled.contains(conversation) || m.shared_with.contains_key(conversation)) {
                return None;
            }
        }
        let snapshot = BoardSnapshot { at: crate::store::now_ms(), board: crate::tasks::load(self.store.dir(), conversation) };
        serde_json::to_value(Shared {
            conversation: conversation.to_string(),
            title: String::new(),
            id: uuid::Uuid::new_v4().to_string(),
            sender: "human".into(),
            kind: "tasks".into(),
            to: vec![],
            text: serde_json::to_string(&snapshot).ok()?,
            via: None,
            owner: None,
            history: vec![],
        })
        .ok()
    }

    /// The owner `computer` sent its board for a conversation: it replaces this copy's board, with
    /// each owner shown as seen from here. Older snapshots than the one already taken are ignored.
    fn take_board(&self, packet: &PeerPacket, shared: Shared, computer: &str) -> Result<Option<Event>, String> {
        let Ok(mut snapshot) = serde_json::from_str::<BoardSnapshot>(&shared.text) else { return Ok(None) };
        let local = {
            let mut m = self.mirrors.lock().unwrap();
            let key = format!("{}/{}", packet.node_id, shared.conversation);
            let Some(local) = m.map.get(&key).filter(|id| self.store.meta(id).is_ok()).cloned() else { return Ok(None) };
            if m.boards.get(&local).is_some_and(|&at| at >= snapshot.at) {
                return Ok(None);
            }
            m.boards.insert(local.clone(), snapshot.at);
            self.save(&m)?;
            local
        };
        crate::tasks::localize(&mut snapshot.board, &crate::prompt::here_slug(), Some(&crate::prompt::slug(computer)));
        if !crate::tasks::replace(self.store.dir(), &local, &snapshot.board)? {
            return Ok(None);
        }
        let note = format!("Tasks: the shared board was updated on {computer}. {}", snapshot.board.progress());
        self.store.append_imported(&local, crate::council::CONSILIUM, "system", vec![], &note, None, None).map(Some)
    }

    /// Receives host resources reported by a linked peer computer.
    fn take_resources(&self, packet: &PeerPacket, shared: Shared) -> Result<Option<Event>, String> {
        let mut m = self.mirrors.lock().unwrap();
        if m.filed.contains(&packet.id) {
            return Ok(None);
        }
        m.filed.insert(packet.id.clone());
        let _ = self.save(&m);
        drop(m);

        if let Ok(res) = serde_json::from_str::<crate::resources::HostResources>(&shared.text) {
            let mut r = self.resources.lock().unwrap();
            let correlation = packet.payload.get("resource_request_id");
            r.insert(packet.node_id.clone(), PeerResourceReport {
                resources: res,
                supports_request_id: correlation.is_some(),
                request_id: correlation.and_then(Value::as_str).map(str::to_owned),
            });
        }
        Ok(None)
    }

    /// A packet carrying an AI's on/off switch state in `conversation`.
    fn switched_packet(&self, conversation: &str, agent: &str, on: bool) -> Option<Value> {
        let (conv_id, owner) = if let Some((owner, remote_conv)) = self.origin(conversation) {
            (remote_conv, Some(owner))
        } else {
            let m = self.mirrors.lock().unwrap();
            if !(m.backfilled.contains(conversation) || m.shared_with.contains_key(conversation)) {
                return None;
            }
            (conversation.to_string(), None)
        };
        let payload = serde_json::json!({
            "agent": agent,
            "on": on,
        });
        serde_json::to_value(Shared {
            conversation: conv_id,
            title: String::new(),
            id: uuid::Uuid::new_v4().to_string(),
            sender: "human".into(),
            kind: "switched".into(),
            to: vec![],
            text: serde_json::to_string(&payload).ok()?,
            via: None,
            owner,
            history: vec![],
        })
        .ok()
    }

    /// An AI was switched on or off in a linked conversation: updates local state and notifies.
    fn take_switched(&self, packet: &PeerPacket, shared: Shared, computer: &str) -> Result<Option<Event>, String> {
        let mut m = self.mirrors.lock().unwrap();
        if m.filed.contains(&packet.id) {
            return Ok(None);
        }
        m.filed.insert(packet.id.clone());
        let _ = self.save(&m);
        drop(m);

        #[derive(Deserialize)]
        struct SwitchedPayload {
            agent: String,
            on: bool,
        }
        let Ok(payload) = serde_json::from_str::<SwitchedPayload>(&shared.text) else {
            return Ok(None);
        };
        let is_owner = shared.owner.is_some();
        let local_conv = if let Some(owner) = &shared.owner {
            let me = self.me.lock().unwrap().clone();
            if owner != &me {
                return Ok(None);
            }
            if self.store.meta(&shared.conversation).is_err() {
                return Ok(None);
            }
            shared.conversation.clone()
        } else {
            let m = self.mirrors.lock().unwrap();
            let key = format!("{}/{}", packet.node_id, shared.conversation);
            let Some(local) = m.map.get(&key).filter(|id| self.store.meta(id).is_ok()).cloned() else {
                return Ok(None);
            };
            local
        };

        crate::switched::set(self.store.dir(), &local_conv, &payload.agent, payload.on)?;

        if is_owner {
            if let Some(link) = crate::link_handle().filter(|l| l.enabled() && l.has_peers()) {
                if let Some(p) = self.switched_packet(&local_conv, &payload.agent, payload.on) {
                    let _ = link.send(PacketKind::Chat, p);
                }
            }
        }

        let agent_name = crate::agents::by_id(&payload.agent).map_or(payload.agent.as_str(), |a| a.name);
        let state_str = if payload.on { "switched on" } else { "switched off" };
        let note = format!("Council: {agent_name} was {state_str} on {computer}.");
        let event = self.store.append_imported(&local_conv, crate::council::CONSILIUM, "system", vec![], &note, None, None)?;
        self.notify(&local_conv, &event);
        Ok(Some(event))
    }

    /// Shares this computer's host resources with linked peers.
    pub fn share_resources(&self, target_path: Option<&Path>) -> Result<(), String> {
        self.send_resources(target_path, None)
    }

    fn send_resources(&self, target_path: Option<&Path>, request: Option<(&str, &str)>) -> Result<(), String> {
        let Some(link) = crate::link_handle().filter(|l| l.enabled() && l.has_peers()) else { return Ok(()) };
        let host_res = crate::resources::HostResources::sample(&crate::prompt::here_slug(), target_path);
        let serialized = serde_json::to_string(&host_res).map_err(|e| e.to_string())?;
        let payload = serde_json::json!({
            "conversation": "",
            "title": "",
            "id": uuid::Uuid::new_v4().to_string(),
            "sender": "system",
            "kind": "resources",
            "resource_request_id": request.map(|(_, id)| id),
            "to": [],
            "text": serialized,
        });
        if let Some((node_id, _)) = request {
            link.send_to(node_id, PacketKind::Chat, payload)?;
        } else {
            link.send(PacketKind::Chat, payload)?;
        }
        Ok(())
    }

    /// Returns the newest host resources reported by each linked peer computer.
    pub fn peer_resources(&self) -> HashMap<String, crate::resources::HostResources> {
        self.resources.lock().unwrap().iter().map(|(id, report)| (id.clone(), report.resources.clone())).collect()
    }

    fn peer_resource_reports(&self) -> HashMap<String, PeerResourceReport> {
        self.resources.lock().unwrap().clone()
    }

    /// Records that event `id` is being sent; false if it already was.
    fn first_time(&self, id: &str) -> bool {
        let mut recent = self.recent.lock().unwrap();
        if !recent.0.insert(id.to_string()) {
            return false;
        }
        recent.1.push_back(id.to_string());
        if recent.1.len() > RECENT {
            if let Some(old) = recent.1.pop_front() {
                recent.0.remove(&old);
            }
        }
        true
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
        // In a copy, what was copied in (a human message carries the computer it came from) is
        // never sent back. What the human types there goes to the owner: from the window through
        // `reply`, and from the terminal, which has no link of its own, here, once the window shows
        // it. This computer's own AIs' answers go back to the owner too.
        if origin.is_some() && ((e.sender == "human" && e.via.is_some()) || !known_sender(&e.sender)) {
            return None;
        }
        if !self.first_time(&e.id) {
            return None;
        }
        if let Some((owner, conversation)) = origin {
            if e.sender == "human" {
                // Its copy coming back from the owner is already shown here.
                self.sent.lock().unwrap().push((e.conversation.clone(), e.text.trim().to_string()));
            }
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
            {
                let mut m = self.mirrors.lock().unwrap();
                m.shared_with.entry(c.id.clone()).or_default().extend(peers.iter().cloned());
                m.backfilled.insert(c.id.clone());
                let _ = self.save(&m);
            }
            if !crate::tasks::load(self.store.dir(), &c.id).tasks.is_empty() {
                out.extend(self.board_packet(&c.id));
            }
            for agent in crate::switched::off(self.store.dir(), &c.id) {
                out.extend(self.switched_packet(&c.id, &agent, false));
            }
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
        if shared.kind == crate::lan_notes::KIND {
            self.lan_notes.record(packet, computer, &shared.text)?;
            return Ok(None);
        }
        if shared.kind == "update" {
            return self.take_update(packet, shared, computer);
        }
        if shared.kind == "update_request" {
            return self.take_update_request(packet, computer);
        }
        if shared.kind == "restart_request" {
            return self.take_restart_request(packet, shared, computer);
        }
        if shared.kind == "tasks" {
            return self.take_board(packet, shared, computer);
        }
        if shared.kind == "resources" {
            return self.take_resources(packet, shared);
        }
        if shared.kind == "resources_request" {
            if crate::store::now_ms().abs_diff(packet.timestamp_ms) <= 120_000 {
                let data = crate::data_dir();
                self.send_resources(data.as_deref(), Some((&packet.node_id, &packet.id)))?;
            }
            return Ok(None);
        }
        if shared.kind == "switched" {
            return self.take_switched(packet, shared, computer);
        }
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
                self.store.append_imported(&local, crate::council::CONSILIUM, "system", vec![], &note, None, None)?;
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
        let text = capped(shared.text);
        // Pending while it's written and filed after: a crash in between is retried without a copy.
        let pending = format!("{PENDING}{}", packet.id);
        if self.already_written(&m, &packet.id, &local, &seen, &shared.sender, &text)? {
            self.settle(&mut m, &packet.id, Some(seen))?;
            return Ok(None);
        }
        m.filed.remove(&format!("{LEGACY_PENDING}{}", packet.id));
        m.filed.insert(pending);
        self.save(&m)?;
        let event = self.store.append_imported(&local, &shared.sender, &shared.kind, shared.to, &text, via, Some(seen.clone()))?;
        self.settle(&mut m, &packet.id, Some(seen))?;
        drop(m);
        // The AIs here answer the human's message too, and read the other computer's AIs.
        self.notify(&local, &event);
        Ok(Some(event))
    }

    /// Files a conversation's earlier messages in its mirror, skipping any already there (by id,
    /// or by sender and text for ones copied before ids were recorded). Returns the last one filed.
    fn file_history(&self, m: &mut Mirrors, node: &str, local: &str, earlier: Vec<Earlier>, computer: &str) -> Result<Option<Event>, String> {
        let mut sources = HashSet::new();
        let mut legacy = HashSet::new();
        for event in self.store.load(local)? {
            if let Some(source) = event.source_event {
                sources.insert(source);
            } else {
                legacy.insert((event.sender, event.text));
            }
        }
        let mut last = None;
        for item in earlier.into_iter().take(HISTORY_LIMIT) {
            let seen = format!("ev:{node}/{}", item.id);
            let text = capped(item.text);
            if !known_sender(&item.sender) || m.filed.contains(&seen) {
                continue;
            }
            if sources.contains(&seen) {
                m.filed.insert(seen);
                continue;
            }
            if legacy.contains(&(item.sender.clone(), crate::redact::scrub(&text))) {
                continue;
            }
            let via = shown_via(&item.sender, &item.via, computer);
            last = Some(self.store.append_imported(local, &item.sender, "message", item.to, &text, via, Some(seen.clone()))?);
            m.filed.insert(seen);
        }
        self.save(m)?;
        Ok(last)
    }

    /// Records `packet_id` (and its event key) as filed and clears its pending mark.
    fn settle(&self, m: &mut Mirrors, packet_id: &str, seen: Option<String>) -> Result<(), String> {
        m.filed.remove(&format!("{PENDING}{packet_id}"));
        m.filed.remove(&format!("{LEGACY_PENDING}{packet_id}"));
        m.filed.insert(packet_id.to_string());
        if let Some(seen) = seen {
            m.filed.insert(seen);
        }
        self.save(m)
    }

    /// Checks the log after an interrupted write. Older pending markers predate source IDs.
    fn already_written(
        &self,
        mirrors: &Mirrors,
        packet_id: &str,
        conversation: &str,
        source_event: &str,
        sender: &str,
        text: &str,
    ) -> Result<bool, String> {
        let legacy = mirrors.filed.contains(&format!("{LEGACY_PENDING}{packet_id}"));
        if !legacy && !mirrors.filed.contains(&format!("{PENDING}{packet_id}")) {
            return Ok(false);
        }
        let events = self.store.load(conversation)?;
        if events.iter().any(|event| event.source_event.as_deref() == Some(source_event)) {
            return Ok(true);
        }
        if legacy {
            let text = crate::redact::scrub(text);
            return Ok(events.iter().rev().take(RECHECK_EVENTS).any(|event| {
                event.source_event.is_none() && event.sender == sender && event.text == text
            }));
        }
        Ok(false)
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
        // So "@claude-<this computer>" from another computer reaches this computer's Claude.
        crate::prompt::set_here(&link.status().name);
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
    let engine = engine.clone();
    let spawner = engine.clone();
    spawner.spawn_task(Box::pin(async move {
        loop {
            if let (Some(s), Some(link)) = (SYNC.get(), crate::link_handle()) {
                if link.enabled() && link.has_peers() {
                    // The roll call: council members here that can answer right now.
                    let now = crate::store::now_ms();
                    let ready: Vec<String> = engine
                        .config
                        .get()
                        .members
                        .into_iter()
                        .filter(|m| crate::agents::by_id(m).is_some_and(crate::agents::installed) && engine.bench.until(m, now).is_none())
                        .collect();
                    crate::link::set_council(ready);
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
        let replayed = link.inbox();
        let mut failed = HashSet::new();
        for p in &replayed {
            let name = link.peer_name(&p.node_id);
            if s.file(p, &name).is_err() {
                failed.insert(p.id.clone());
            }
        }
        // Everything offered is filed (or had nothing to file): keep only failures and late arrivals.
        let offered: HashSet<String> = replayed.into_iter().map(|p| p.id).collect();
        let _ = link.retain_inbox(|p| failed.contains(&p.id) || !offered.contains(&p.id));
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
        if let Some(board) = s.board_packet(&e.conversation) {
            let _ = link.send(PacketKind::Chat, board);
        }
        for agent in crate::switched::off(s.store.dir(), &e.conversation) {
            if let Some(switched) = s.switched_packet(&e.conversation, &agent, false) {
                let _ = link.send(PacketKind::Chat, switched);
            }
        }
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

/// Sends `conversation`'s task board to the linked computers with a copy of it, after it changed
/// here. Does nothing for a copy (its owner keeps the board) or a conversation not shared yet.
pub fn share_board(conversation: &str) {
    let (Some(s), Some(link)) = (SYNC.get(), crate::link_handle()) else { return };
    if !link.enabled() || !link.has_peers() {
        return;
    }
    if let Some(packet) = s.board_packet(conversation) {
        let _ = link.send(PacketKind::Chat, packet);
    }
}

/// Sends an AI's switched on/off state in `conversation` to linked computers.
pub fn share_switched(conversation: &str, agent: &str, on: bool) {
    let (Some(s), Some(link)) = (SYNC.get(), crate::link_handle()) else { return };
    if !link.enabled() || !link.has_peers() {
        return;
    }
    if let Some(packet) = s.switched_packet(conversation, agent, on) {
        let _ = link.send(PacketKind::Chat, packet);
    }
}

/// The name of the computer that keeps `conversation`'s board, if it is a copy whose board comes
/// from there. TASK lines and board edits here then go to that computer instead of a local board.
pub fn board_owner(conversation: &str) -> Option<String> {
    let s = SYNC.get()?;
    let (node, _) = s.origin(conversation)?;
    if !s.mirrors.lock().unwrap().boards.contains_key(conversation) {
        return None;
    }
    Some(crate::link_handle().map(|l| l.peer_name(&node)).unwrap_or(node))
}

/// Tells every linked computer that this one is installing `build` and restarting, so they update
/// too. Queued durably, so it still goes out after the restart if the app quits first.
pub fn announce_update(build: &crate::build_id::Build) {
    let Some(link) = crate::link_handle().filter(|l| l.enabled() && l.has_peers()) else { return };
    let payload = serde_json::to_value(Shared {
        conversation: String::new(),
        title: String::new(),
        id: uuid::Uuid::new_v4().to_string(),
        sender: "human".into(),
        kind: "update".into(),
        to: vec![],
        text: serde_json::to_string(build).unwrap_or_default(),
        via: None,
        owner: None,
        history: vec![],
    });
    if let Ok(p) = payload {
        let _ = link.send(PacketKind::Chat, p);
    }
}

/// The Request Update button: asks every linked computer to update too. Queued durably, so it
/// still goes out if this computer restarts into its own update first. Returns how many linked
/// computers it was sent to.
pub fn request_update() -> Result<usize, String> {
    let Some(link) = crate::link_handle().filter(|l| l.enabled() && l.has_peers()) else { return Ok(0) };
    let payload = serde_json::to_value(Shared {
        conversation: String::new(),
        title: String::new(),
        id: uuid::Uuid::new_v4().to_string(),
        sender: "human".into(),
        kind: "update_request".into(),
        to: vec![],
        text: serde_json::to_string(&crate::build_id::Build::this()).unwrap_or_default(),
        via: None,
        owner: None,
        history: vec![],
    })
    .map_err(|e| e.to_string())?;
    link.send(PacketKind::Chat, payload)?;
    Ok(link.status().peers.len())
}

/// Requests a restart of Consilium on linked computers (or a specific target). Returns how many linked
/// computers it was sent to.
pub fn request_restart(target: Option<&str>) -> Result<usize, String> {
    let Some(link) = crate::link_handle().filter(|l| l.enabled() && l.has_peers()) else { return Ok(0) };
    let payload = serde_json::to_value(Shared {
        conversation: String::new(),
        title: String::new(),
        id: uuid::Uuid::new_v4().to_string(),
        sender: "human".into(),
        kind: "restart_request".into(),
        to: vec![],
        text: target.unwrap_or("all").to_string(),
        via: None,
        owner: None,
        history: vec![],
    })
    .map_err(|e| e.to_string())?;
    link.send(PacketKind::Chat, payload)?;
    Ok(link.status().peers.len())
}

/// Files a packet that just arrived from a paired computer.
pub fn receive(packet: &PeerPacket) -> Option<Event> {
    let (s, link) = SYNC.get().zip(crate::link_handle())?;
    s.file(packet, &link.peer_name(&packet.node_id)).ok().flatten()
}

pub(crate) fn lan_notes() -> Result<Vec<crate::lan_notes::Note>, String> {
    SYNC.get().ok_or("Computer sync has not finished starting.")?.lan_notes.list()
}

pub(crate) fn send_lan_note(link: &crate::link::Link, node_id: &str, text: &str) -> Result<String, String> {
    let text = crate::lan_notes::note_text(text)?;
    let payload = serde_json::json!({
        "conversation": "", "title": "", "id": "", "sender": "human",
        "kind": crate::lan_notes::KIND, "text": text,
    });
    link.send_lan_note(node_id, payload)
}

/// Shares this computer's host resources with linked peers.
pub fn share_resources(target_path: Option<&Path>) -> Result<(), String> {
    if let Some(s) = SYNC.get() {
        s.share_resources(target_path)?;
    }
    Ok(())
}

/// Returns the newest host resources reported by each linked peer computer.
pub fn peer_resources() -> HashMap<String, crate::resources::HostResources> {
    SYNC.get().map(|s| s.peer_resources()).unwrap_or_default()
}

pub fn peer_resource_reports() -> HashMap<String, PeerResourceReport> {
    SYNC.get().map(|s| s.peer_resource_reports()).unwrap_or_default()
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
    fn lan_notes_survive_replay_without_creating_conversations() {
        let root = std::env::temp_dir().join(format!("peer-lan-notes-{}", uuid::Uuid::new_v4()));
        let store = Arc::new(Store::new(root.join("conversations")).unwrap());
        let sync = Sync::new(store.clone(), &root);
        let note = packet("node-other", "note-1", serde_json::json!({
            "conversation": "", "title": "", "id": "", "sender": "human",
            "kind": "lan_note", "text": "  @claude\nTASK: take fixture  ",
        }));
        assert!(sync.file(&note, "Other computer").unwrap().is_none());
        let reopened = Sync::new(store.clone(), &root);
        assert!(reopened.file(&note, "Other computer").unwrap().is_none());
        let notes = serde_json::to_value(reopened.lan_notes.list().unwrap()).unwrap();
        assert_eq!(notes.as_array().unwrap().len(), 1);
        assert_eq!(notes[0]["source"], "Other computer");
        assert_eq!(notes[0]["text"], "@claude\nTASK: take fixture");
        assert!(store.list().is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_crash_while_a_message_is_pending_files_it_exactly_once() {
        let root = std::env::temp_dir().join(format!("peer-sync-pending-{}", uuid::Uuid::new_v4()));
        let (a_dir, b_dir) = (root.join("a"), root.join("b"));
        let a = Sync::new(Arc::new(Store::new(a_dir.join("c")).unwrap()), &a_dir);
        let b = Sync::new(Arc::new(Store::new(b_dir.join("c")).unwrap()), &b_dir);
        let conv = a.store.create("Plans").unwrap();
        let typed = a.store.append(&conv.id, "human", "message", vec![], "hello").unwrap();
        let p1 = packet("node-a", "p1", a.outgoing(&typed).unwrap());
        let filed = b.file(&p1, "MacBook").unwrap().expect("filed");
        let count = |text: &str| b.store.load(&filed.conversation).unwrap().iter().filter(|e| e.text == text).count();

        // Crash after the write, before it was recorded as filed: only the pending mark survived.
        {
            let mut m = b.mirrors.lock().unwrap();
            m.filed.retain(|k| k != "p1" && !k.starts_with("ev:"));
            m.filed.insert(format!("{PENDING}p1"));
        }
        assert!(b.file(&p1, "MacBook").unwrap().is_none(), "already in the chat, so not written again");
        assert_eq!(count("hello"), 1);
        {
            let m = b.mirrors.lock().unwrap();
            assert!(m.filed.contains("p1") && !m.filed.contains(&format!("{PENDING}p1")), "settled as filed");
        }

        // Crash before the write: pending, but not in the chat, so the retry writes it once.
        // Identical sender and text can belong to a different source event.
        let next = a.store.append(&conv.id, "human", "message", vec![], "hello").unwrap();
        let p2 = packet("node-a", "p2", a.outgoing(&next).unwrap());
        b.mirrors.lock().unwrap().filed.insert(format!("{PENDING}p2"));
        assert!(b.file(&p2, "MacBook").unwrap().is_some(), "a lost write is retried");
        assert!(b.file(&p2, "MacBook").unwrap().is_none());
        assert_eq!(count("hello"), 2);
        let _ = std::fs::remove_dir_all(&root);
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

    /// The startup replay of `inbox.jsonl` after a quit or crash files each packet once, even
    /// though a fresh `Sync` reads it again (the filed ids are saved with the mirrors).
    #[test]
    fn replaying_the_inbox_after_a_restart_files_nothing_twice() {
        let root = std::env::temp_dir().join(format!("peer-sync-replay-{}", uuid::Uuid::new_v4()));
        let (a_dir, b_dir) = (root.join("a"), root.join("b"));
        let a = Sync::new(Arc::new(Store::new(a_dir.join("c")).unwrap()), &a_dir);
        let conv = a.store.create("Plans").unwrap();
        let typed = a.store.append(&conv.id, "human", "message", vec![], "hello").unwrap();
        let p = packet("node-a", "p1", a.outgoing(&typed).unwrap());

        let b = Sync::new(Arc::new(Store::new(b_dir.join("c")).unwrap()), &b_dir);
        let filed = b.file(&p, "MacBook").unwrap().expect("filed");
        drop(b);
        // Restart: a new Sync over the same folder replays the whole inbox.
        let b = Sync::new(Arc::new(Store::new(b_dir.join("c")).unwrap()), &b_dir);
        assert!(b.file(&p, "MacBook").unwrap().is_none(), "filed once across a restart");
        let copies = b.store.load(&filed.conversation).unwrap().iter().filter(|e| e.text == "hello").count();
        assert_eq!(copies, 1);
        // A packet that arrived but wasn't filed before the crash is filed on replay.
        let more = a.store.append(&conv.id, "human", "message", vec![], "after").unwrap();
        let q = packet("node-a", "p2", a.outgoing(&more).unwrap());
        assert_eq!(b.file(&q, "MacBook").unwrap().expect("filed on replay").conversation, filed.conversation);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn switched_state_syncs_between_owner_and_mirror() {
        let root = std::env::temp_dir().join(format!("peer-sync-switched-{}", uuid::Uuid::new_v4()));
        let (a_dir, b_dir) = (root.join("a"), root.join("b"));
        let a = Sync::new(Arc::new(Store::new(a_dir.join("c")).unwrap()), &a_dir);
        let b = Sync::new(Arc::new(Store::new(b_dir.join("c")).unwrap()), &b_dir);
        *a.me.lock().unwrap() = "node-a".into();
        *b.me.lock().unwrap() = "node-b".into();

        let conv = a.store.create("Plans").unwrap();
        let typed = a.store.append(&conv.id, "human", "message", vec![], "hello").unwrap();
        let payload = a.outgoing(&typed).expect("a human message is shared");
        let filed = b.file(&packet("node-a", "p1", payload), "MacBook").unwrap().expect("filed");
        let mirror_conv = filed.conversation;
        a.mirrors.lock().unwrap().backfilled.insert(conv.id.clone());

        // 1. Owner switches an AI off, packet sent to mirror
        crate::switched::set(a.store.dir(), &conv.id, "claude", false).unwrap();
        let p_off = a.switched_packet(&conv.id, "claude", false).expect("switched packet generated");
        let res = b.file(&packet("node-a", "p_sw_1", p_off), "MacBook").unwrap().expect("event logged");
        assert!(res.text.contains("Claude was switched off on MacBook"));
        assert!(crate::switched::is_off(b.store.dir(), &mirror_conv, "claude"), "claude is now off in mirror");

        // 2. Mirror switches an AI on, packet sent back to owner
        crate::switched::set(b.store.dir(), &mirror_conv, "claude", true).unwrap();
        let p_on = b.switched_packet(&mirror_conv, "claude", true).expect("mirror switched packet generated");
        let res_owner = a.file(&packet("node-b", "p_sw_2", p_on), "MSI").unwrap().expect("event logged on owner");
        assert!(res_owner.text.contains("Claude was switched on on MSI"));
        assert!(!crate::switched::is_off(a.store.dir(), &conv.id, "claude"), "claude is now on in owner");

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
    fn one_task_board_is_kept_by_the_owner_and_copied_to_every_linked_computer() {
        let root = std::env::temp_dir().join(format!("peer-sync-board-{}", uuid::Uuid::new_v4()));
        let (a_dir, b_dir) = (root.join("a"), root.join("b"));
        let a = Sync::new(Arc::new(Store::new(a_dir.join("c")).unwrap()), &a_dir);
        let b = Sync::new(Arc::new(Store::new(b_dir.join("c")).unwrap()), &b_dir);
        *a.me.lock().unwrap() = "node-a".into();
        *b.me.lock().unwrap() = "node-b".into();
        let conv = a.store.create("Plans").unwrap();
        let ask = a.store.append(&conv.id, "human", "message", vec![], "Build it").unwrap();
        let mirror = b.file(&packet("node-a", "p1", a.outgoing(&ask).unwrap()), "PC").unwrap().unwrap().conversation;
        a.mirrors.lock().unwrap().backfilled.insert(conv.id.clone());
        assert!(b.board_packet(&mirror).is_none(), "a copy never sends its board");
        assert!(board_owner_of(&b, &mirror).is_none(), "no snapshot yet, so the copy still keeps its own");

        // A's Gemini takes a task on A; B's Claude, answering in the copy, takes another.
        crate::tasks::apply(a.store.dir(), &conv.id, "gemini", &crate::tasks::task_lines("TASK: take Fix the helper")).unwrap();
        let answer = b.store.append(&mirror, "claude", "message", vec![], "I'll do it.\nTASK: take Shared board").unwrap();
        a.file(&packet("node-b", "c1", b.outgoing(&answer).unwrap()), "MacBook").unwrap().unwrap();
        let owned = crate::tasks::load(a.store.dir(), &conv.id);
        let owners: Vec<_> = owned.tasks.iter().map(|t| (t.id, t.owner.clone().unwrap())).collect();
        assert_eq!(owners, vec![(1, "gemini".to_string()), (2, "claude-macbook".to_string())]);

        // The owner's snapshot replaces the copy's board, with A's own AIs named for A.
        let snap = a.board_packet(&conv.id).expect("shared conversations send their board");
        let note = b.file(&packet("node-a", "t1", snap.clone()), "PC").unwrap().expect("board replaced");
        assert!(note.text.contains("2 in progress"), "{}", note.text);
        let copy = crate::tasks::load(b.store.dir(), &mirror);
        assert_eq!(copy.tasks[0].owner.as_deref(), Some("gemini-pc"));
        assert_eq!(copy.tasks.len(), 2);
        assert!(b.file(&packet("node-a", "t1b", snap), "PC").unwrap().is_none(), "an older or equal snapshot changes nothing");
        assert!(board_owner_of(&b, &mirror).is_some(), "now the owner keeps it");

        let completion = b.store.append(
            &mirror, "claude", "message", vec![], "TASK: done #1\nTASK: done #2",
        ).unwrap();
        a.file(&packet("node-b", "c2", b.outgoing(&completion).unwrap()), "MacBook").unwrap().unwrap();
        let owned = crate::tasks::load(a.store.dir(), &conv.id);
        assert_eq!(owned.tasks[0].status, crate::tasks::Status::Doing);
        assert_eq!(owned.tasks[1].status, crate::tasks::Status::Done);
        let _ = std::fs::remove_dir_all(&root);
    }

    fn board_owner_of(s: &Sync, conversation: &str) -> Option<String> {
        s.origin(conversation).filter(|_| s.mirrors.lock().unwrap().boards.contains_key(conversation)).map(|(n, _)| n)
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

    #[test]
    fn addressing_ais_here_by_handle_starts_at_most_three_turns_per_human_message() {
        crate::prompt::set_here("Windows PC");
        let root = std::env::temp_dir().join(format!("peer-sync-handle-{}", uuid::Uuid::new_v4()));
        let (a_dir, b_dir) = (root.join("a"), root.join("b"));
        let a = Sync::new(Arc::new(Store::new(a_dir.join("c")).unwrap()), &a_dir);
        let b = Sync::new(Arc::new(Store::new(b_dir.join("c")).unwrap()), &b_dir);
        let b_woke = record_wakes(&b);
        let conv = a.store.create("Plans").unwrap();
        let ask = a.store.append(&conv.id, "human", "message", vec![], "split the work").unwrap();
        b.file(&packet("node-a", "p0", a.outgoing(&ask).unwrap()), "MacBook").unwrap().unwrap();
        for i in 0..5 {
            let m = a.store.append(&conv.id, "claude", "message", vec![], &format!("@gemini-windows-pc take part {i}")).unwrap();
            b.file(&packet("node-a", &format!("p{}", i + 1), a.outgoing(&m).unwrap()), "MacBook").unwrap().unwrap();
        }
        let starts: Vec<bool> = b_woke.lock().unwrap().iter().skip(1).map(|w| w.2).collect();
        assert_eq!(starts, vec![true, true, true, false, false]);
        // A plain @gemini from there means that computer's Gemini: only the usual one reaction.
        let plain = a.store.append(&conv.id, "claude", "message", vec![], "@gemini what do you think?").unwrap();
        b.file(&packet("node-a", "p9", a.outgoing(&plain).unwrap()), "MacBook").unwrap().unwrap();
        assert_eq!(b_woke.lock().unwrap().last().map(|w| w.2), Some(true), "its one reaction for this human message");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Both tests below take `follow`'s one-shot release-check flag, which is shared by the
    /// whole process; run in parallel, one could take the flag the other set.
    static FOLLOW_FLAGS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn an_update_on_one_computer_tells_the_others_to_follow() {
        let _flags = FOLLOW_FLAGS.lock().unwrap_or_else(|e| e.into_inner());
        let root = std::env::temp_dir().join(format!("peer-sync-update-{}", uuid::Uuid::new_v4()));
        let b = Sync::new(Arc::new(Store::new(root.join("c")).unwrap()), &root);
        let conv = b.store.create("Plans").unwrap();
        let this = crate::build_id::Build::this();
        let newer = crate::build_id::Build { version: this.version.clone(), number: this.number + 3, commit: "abcdef123456".into() };
        let announce = |id: &str, build: &crate::build_id::Build| {
            packet(
                "node-a",
                id,
                serde_json::json!({"conversation": "", "title": "", "id": id, "sender": "human", "kind": "update", "text": serde_json::to_string(build).unwrap()}),
            )
        };
        let note = b.file(&announce("u1", &newer), "MacBook").unwrap().expect("a note");
        assert_eq!(note.conversation, conv.id);
        assert!(note.text.starts_with("MacBook is installing Consilium") && note.text.contains("preparing the same build"), "{}", note.text);
        assert!(b.file(&announce("u1", &newer), "MacBook").unwrap().is_none(), "once");
        assert!(crate::follow::take_release_check(), "the window checks GitHub at once");
        let older = b.file(&announce("u2", &this), "MacBook").unwrap().unwrap();
        assert!(older.text.contains("already runs"), "{}", older.text);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_request_update_from_one_computer_makes_the_others_install() {
        let _flags = FOLLOW_FLAGS.lock().unwrap_or_else(|e| e.into_inner());
        let root = std::env::temp_dir().join(format!("peer-sync-request-{}", uuid::Uuid::new_v4()));
        let b = Sync::new(Arc::new(Store::new(root.join("c")).unwrap()), &root);
        let conv = b.store.create("Plans").unwrap();
        let request = packet(
            "node-a",
            "r1",
            serde_json::json!({"conversation": "", "title": "", "id": "r1", "sender": "human", "kind": "update_request", "text": ""}),
        );
        let note = b.file(&request, "MacBook").unwrap().expect("a note");
        assert_eq!(note.conversation, conv.id);
        assert!(note.text.starts_with("MacBook asked every linked computer to update"), "{}", note.text);
        let now = crate::store::now_ms();
        let request_at = crate::follow::pending_install_request(now).expect("installs even with automatic install off");
        assert!(crate::follow::pending_install_request(now + 31 * 60_000).is_none(), "the request runs out");
        assert!(crate::follow::take_release_check(), "the window checks GitHub at once");
        assert!(b.file(&request, "MacBook").unwrap().is_none(), "once");
        crate::follow::consume_install_request(request_at);
        assert!(crate::follow::pending_install_request(now).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_restart_request_from_one_computer_records_note() {
        let root = std::env::temp_dir().join(format!("peer-sync-restart-{}", uuid::Uuid::new_v4()));
        let b = Sync::new(Arc::new(Store::new(root.join("c")).unwrap()), &root);
        let conv = b.store.create("Plans").unwrap();
        let request = packet(
            "node-a",
            "rst1",
            serde_json::json!({"conversation": "", "title": "", "id": "rst1", "sender": "human", "kind": "restart_request", "text": "all"}),
        );
        let note = b.file(&request, "MacBook").unwrap().expect("a note");
        assert_eq!(note.conversation, conv.id);
        assert!(note.text.contains("MacBook requested a restart of Consilium"), "{}", note.text);
        assert!(b.file(&request, "MacBook").unwrap().is_none(), "once");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_message_typed_in_a_copy_from_the_terminal_reaches_the_owner_once() {
        let root = std::env::temp_dir().join(format!("peer-sync-terminal-{}", uuid::Uuid::new_v4()));
        let (a_dir, b_dir) = (root.join("a"), root.join("b"));
        let a = Sync::new(Arc::new(Store::new(a_dir.join("c")).unwrap()), &a_dir);
        let b = Sync::new(Arc::new(Store::new(b_dir.join("c")).unwrap()), &b_dir);
        *a.me.lock().unwrap() = "node-a".into();
        *b.me.lock().unwrap() = "node-b".into();
        let relayed = Arc::new(Mutex::new(Vec::new()));
        let r = relayed.clone();
        let store = a.store.clone();
        let _ = a.relay.set(Box::new(move |c: &str, t: &str, to: Vec<String>, pc: &str| {
            r.lock().unwrap().push(t.to_string());
            store.append_with_attachments_via(c, to, t, vec![], Some(pc))
        }));
        let conv = a.store.create("Plans").unwrap();
        let ask = a.store.append(&conv.id, "human", "message", vec![], "hello").unwrap();
        let mirror = b.file(&packet("node-a", "p1", a.outgoing(&ask).unwrap()), "MacBook").unwrap().unwrap();
        assert!(b.outgoing(&mirror).is_none(), "what was copied in isn't sent back");

        // The terminal writes straight into the copy; the window then shows it and it is sent.
        let typed = b.store.append(&mirror.conversation, "human", "message", vec![], "please update").unwrap();
        let payload = b.outgoing(&typed).expect("sent to the owner");
        assert!(b.outgoing(&typed).is_none(), "once");
        a.file(&packet("node-b", "r1", payload), "PC").unwrap().unwrap();
        assert_eq!(relayed.lock().unwrap().clone(), vec!["please update".to_string()], "the owner records it and runs its turn");

        // Typed in the window instead: `reply` sends it, and showing it doesn't send it again.
        let (shown, _) = b.reply(&mirror.conversation, "and restart", &[]).unwrap();
        assert!(b.outgoing(&shown).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn linked_computers_share_and_record_host_resources() {
        let root = std::env::temp_dir().join(format!("peer-sync-res-{}", uuid::Uuid::new_v4()));
        let b = Sync::new(Arc::new(Store::new(root.join("c")).unwrap()), &root);
        let sample = crate::resources::HostResources {
            computer: "MSI-AGENT47".into(),
            os: "windows".into(),
            build_arch: Some("aarch64".into()),
            free_disk_bytes: 15 * 1024 * 1024 * 1024,
            total_disk_bytes: 1024 * 1024 * 1024 * 1024,
            cpu_percent: 18,
            memory_percent: 32,
            total_memory_mb: 16384,
            available_memory_mb: Some(11000),
            cores: 16,
            tools: vec!["cargo".into(), "git".into(), "ggml-rpc-server".into()],
            updated_at: 100000,
        };
        let packet = packet(
            "node-windows",
            "res1",
            serde_json::json!({
                "conversation": "",
                "title": "",
                "id": "res1",
                "sender": "system",
                "kind": "resources",
                "text": serde_json::to_string(&sample).unwrap(),
            }),
        );
        let res = b.file(&packet, "MSI-AGENT47").unwrap();
        assert!(res.is_none());
        let mut second = packet.clone();
        second.id = "res2".into();
        second.node_id = "node-second".into();
        second.payload["resource_request_id"] = serde_json::json!("memory-request-1");
        b.file(&second, "MSI-AGENT47").unwrap();
        let recorded = b.peer_resources();
        assert_eq!(recorded.len(), 2);
        assert_eq!(recorded.get("node-windows"), Some(&sample));
        assert_eq!(recorded.get("node-second"), Some(&sample));
        let reports = b.peer_resource_reports();
        assert!(!reports["node-windows"].supports_request_id);
        assert!(reports["node-second"].supports_request_id);
        assert_eq!(reports["node-second"].request_id.as_deref(), Some("memory-request-1"));
        second.id = "res3".into();
        second.payload["resource_request_id"] = Value::Null;
        b.file(&second, "MSI-AGENT47").unwrap();
        let reports = b.peer_resource_reports();
        assert!(reports["node-second"].supports_request_id);
        assert!(reports["node-second"].request_id.is_none());
        let _ = std::fs::remove_dir_all(&root);
    }
}
