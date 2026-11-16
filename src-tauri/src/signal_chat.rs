//! Carries the conversation over Signal, both ways, once Signal setup is verified.
//!
//! Consilium creates Signal groups on the linked account: "Consilium" for the whole council and
//! "Consilium · <AI>" for each member, so the human can message one AI on its own. What the human
//! types in those groups (or in Note to Self) on their phone becomes their message in the newest
//! conversation; "Approve" or "Deny" answers a single pending REQUEST. Each AI's message is sent
//! back to the group it was asked from, and Consilium's own notes go to the council group.
//!
//! Only messages from the linked account itself are accepted (Signal delivers what the human
//! sends from their phone to this linked device as a "sync" message), and only in Consilium's own
//! groups or Note to Self. Other groups and other people are ignored.

use crate::bridge::{self, BridgeConfig, ResolvedSignalAction};
use crate::engine::Engine;
use crate::store::Event;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Longest text sent in one Signal message; longer replies are cut with a pointer to Consilium.
const MAX_OUT: usize = 3500;

/// Ready-made messages sent to the phone, so the human always hears back, even when something
/// goes wrong before any AI answers.
pub mod notice {
    /// Sent as soon as a phone message is posted to the conversation.
    pub const RECEIVED: &str = "Consilium: Message received. Please wait for the AI response.";
    /// Sent when a phone "Approve" or "Deny" answered a pending request.
    pub const DECISION: &str = "Consilium: Decision received and passed to the AI that asked.";
    /// No AI in the council can take the message right now.
    pub const NO_AI: &str =
        "Consilium: Message received, but no AI can answer right now (all are out of usage or none are in the council). It's saved in the chat and will be answered once one is back.";
    /// The message couldn't be added to the conversation.
    pub const NOT_POSTED: &str =
        "Consilium: Sorry, your message couldn't be added to the chat. Please send it again, or open Consilium on the computer.";
    /// There is no conversation to post into.
    pub const NO_CONVERSATION: &str =
        "Consilium: Your message arrived, but there is no conversation open in Consilium. Start one on the computer, then send it again.";
    /// Signal reconnected after Consilium started or restarted.
    pub const BACK_ONLINE: &str =
        "Consilium: Back online. Messages you send here reach the council again.";
    /// A message was too long for Signal and was cut.
    pub const CUT: &str = "\n… (the rest is in Consilium)";
}

/// Where a phone message came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// The council group or Note to Self.
    Council,
    /// One AI's own group.
    Agent(String),
}

/// A message the human sent from their phone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Incoming {
    pub origin: Origin,
    pub text: String,
    pub timestamp: u64,
}

/// Reads a daemon `receive` notification. Returns the human's own message when it was sent into
/// one of Consilium's groups or to Note to Self; everything else is None.
pub fn parse_incoming(v: &Value, account: &str, groups: &bridge::SignalGroups) -> Option<Incoming> {
    if v.get("method").and_then(Value::as_str) != Some("receive") {
        return None;
    }
    let env = v.get("params")?.get("envelope")?;
    let from_self = env.get("sourceNumber").and_then(Value::as_str) == Some(account)
        || env.get("source").and_then(Value::as_str) == Some(account);
    if !from_self || account.is_empty() {
        return None;
    }
    let sent = env.pointer("/syncMessage/sentMessage")?;
    let text = sent.get("message").and_then(Value::as_str)?.trim();
    if text.is_empty() {
        return None;
    }
    let timestamp = sent.get("timestamp").and_then(Value::as_u64).unwrap_or(0);
    let origin = match sent.pointer("/groupInfo/groupId").and_then(Value::as_str) {
        Some(g) if !groups.main.is_empty() && g == groups.main => Origin::Council,
        Some(g) => Origin::Agent(groups.agents.iter().find(|(_, id)| id.as_str() == g)?.0.clone()),
        None => {
            let to_self = sent.get("destinationNumber").and_then(Value::as_str) == Some(account)
                || sent.get("destination").and_then(Value::as_str) == Some(account);
            if !to_self {
                return None;
            }
            Origin::Council
        }
    };
    Some(Incoming { origin, text: text.chars().take(8000).collect(), timestamp })
}

/// REQUESTs in `events` not yet answered with "Approved:", "Denied:" or "Response to request:",
/// with the AI that asked each one.
pub fn pending_requests(events: &[Event]) -> Vec<(String, String)> {
    let mut open: Vec<(String, String)> = Vec::new();
    for e in events {
        if e.sender == "human" {
            let t = e.text.trim();
            open.retain(|(r, _)| {
                let r = r.trim();
                ![format!("Approved: {r}"), format!("Denied: {r}"), format!("Response to request: {r}")]
                    .iter()
                    .any(|a| t.starts_with(a.as_str()))
            });
        } else {
            for r in &e.requests {
                open.push((r.clone(), e.sender.clone()));
            }
        }
    }
    open
}

/// The text sent to Signal for one conversation event, or None when it isn't worth sending.
pub fn outbound_text(e: &Event) -> Option<String> {
    let name = match e.sender.as_str() {
        "human" => {
            // A message received from Signal should not be echoed back to the phone
            if e.via.as_deref().is_some_and(|v| v.eq_ignore_ascii_case("signal")) {
                return None;
            }
            "You".to_string()
        }
        crate::council::CONSILIUM if e.kind == "system" => "Consilium".to_string(),
        id if e.kind == "message" => crate::agents::by_id(id).map_or(id.to_string(), |a| a.name.to_string()),
        _ => return None,
    };
    let body = e.text.trim();
    if body.is_empty() {
        return None;
    }
    let mut msg = if name == "Consilium" {
        format!("[Consilium]\n{}", crate::redact::scrub(body))
    } else if name == "You" {
        if !e.to.is_empty() {
            let targets: Vec<String> = e
                .to
                .iter()
                .map(|t| crate::agents::by_id(t).map_or(t.clone(), |a| a.name.to_string()))
                .collect();
            format!("You (to {}): {}", targets.join(", "), crate::redact::scrub(body))
        } else {
            format!("You: {}", crate::redact::scrub(body))
        }
    } else {
        bridge::format_outbound_notification(&name, body)
    };
    if msg.chars().count() > MAX_OUT {
        msg = msg.chars().take(MAX_OUT).collect::<String>() + notice::CUT;
    }
    Some(msg)
}

fn addr(cfg: &BridgeConfig) -> Result<SocketAddr, String> {
    bridge::validate_loopback(&cfg.endpoint)?;
    let raw = cfg.endpoint.trim().trim_start_matches("tcp://").trim_start_matches("http://");
    raw.parse().map_err(|_| "bad Signal address".to_string())
}

/// One JSON-RPC call on its own connection; returns its `result`.
fn call(cfg: &BridgeConfig, method: &str, params: Value) -> Result<Value, String> {
    let s = TcpStream::connect_timeout(&addr(cfg)?, Duration::from_secs(5)).map_err(|e| e.to_string())?;
    s.set_read_timeout(Some(Duration::from_secs(30))).map_err(|e| e.to_string())?;
    let id = format!("consilium-{}", uuid::Uuid::new_v4());
    let req = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
    (&s).write_all(format!("{req}\n").as_bytes()).map_err(|e| e.to_string())?;
    for line in BufReader::new(&s).lines().take(10_000) {
        let line = line.map_err(|e| e.to_string())?;
        let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
        if v.get("id").and_then(Value::as_str) != Some(id.as_str()) {
            continue;
        }
        if let Some(e) = v.get("error") {
            let m = e.get("message").and_then(Value::as_str).unwrap_or("unknown error");
            return Err(crate::redact::scrub(m));
        }
        return Ok(v.get("result").cloned().unwrap_or(Value::Null));
    }
    Err("signal-cli closed the connection".into())
}

fn send_group(cfg: &BridgeConfig, group: &str, text: &str) -> Result<(), String> {
    call(cfg, "send", json!({"groupId": group, "message": text})).map(|_| ())
}

/// Sends to the account's own Note to Self. Consilium runs as a linked device of the human's own
/// number, so its groups have no other member and a group send reaches nobody; Note to Self is
/// the one chat Signal delivers to the human's phone.
fn send_self(cfg: &BridgeConfig, text: &str) -> Result<(), String> {
    call(cfg, "send", json!({"recipient": [cfg.account.clone()], "message": text})).map(|_| ())
}

/// The Note to Self text for a reply, naming the private group when it answers one.
pub fn self_text(text: &str, private_to: Option<&str>) -> String {
    match private_to {
        Some(name) => format!("(Consilium · {name}, private)\n{text}"),
        None => text.to_string(),
    }
}

fn create_group(cfg: &BridgeConfig, name: &str, description: &str) -> Result<String, String> {
    let r = call(cfg, "updateGroup", json!({"name": name, "description": description}))?;
    r.get("groupId").and_then(Value::as_str).map(String::from).ok_or_else(|| "signal-cli gave no group ID".into())
}

/// Creates the council group and one group per member that don't exist yet, saves their IDs and
/// sends each new group a first message saying who's on the other end.
pub fn ensure_groups(cfg_path: &std::path::Path, members: &[String]) -> Result<BridgeConfig, String> {
    let mut cfg = BridgeConfig::load(cfg_path)?;
    let device = crate::signal_setup::device_name();
    if cfg.groups.main.is_empty() {
        let id = create_group(&cfg, "Consilium", "The Consilium council: messages here go to every AI.")?;
        cfg.groups.main = id.clone();
        cfg.save(cfg_path)?;
        let _ = send_group(
            &cfg,
            &id,
            &format!(
                "[Consilium]\nThis is the council chat for {device}. What you send here goes to every AI in the \
                 council, and their replies come back here. Reply Approve or Deny to answer a request when only \
                 one is waiting. Each AI also has its own group for messaging it on its own."
            ),
        );
    }
    for m in members {
        if cfg.groups.agents.contains_key(m) {
            continue;
        }
        let Some(agent) = crate::agents::by_id(m) else { continue };
        let id = create_group(
            &cfg,
            &format!("Consilium · {}", agent.name),
            &format!("Messages here go only to {}.", agent.name),
        )?;
        cfg.groups.agents.insert(m.clone(), id.clone());
        cfg.save(cfg_path)?;
        let _ = send_group(
            &cfg,
            &id,
            &format!(
                "[{}]\nHi, this is {} in Consilium on {device}. What you send in this group goes only to me, \
                 and I'll answer here.",
                agent.name, agent.name
            ),
        );
    }
    Ok(cfg)
}

/// Runs for the life of the app: links Signal and the newest conversation in both directions
/// while Signal is switched on and verified.
pub fn run(engine: Arc<Engine>, cfg_path: PathBuf) {
    std::thread::spawn(move || loop {
        if let Err(e) = session(&engine, &cfg_path) {
            eprintln!("signal chat: {e}");
        }
        std::thread::sleep(Duration::from_secs(5));
    });
}

struct Outbox {
    /// Cursors for active conversations: conversation ID -> highest seq mirrored.
    cursors: HashMap<String, u64>,
    /// Human events Consilium created from phone messages, and where each came from.
    origins: HashMap<String, Origin>,
}

fn session(engine: &Arc<Engine>, cfg_path: &std::path::Path) -> Result<(), String> {
    crate::link::set_signal_on(false);
    let cfg = BridgeConfig::load(cfg_path)?;
    if !cfg.enabled || !bridge::valid_account(&cfg.account) || cfg.allowed_acis.is_empty() {
        // Not set up, switched off, or the setup's round trip hasn't been verified yet.
        return Ok(());
    }
    let members = engine.config.get().members;
    let cfg = ensure_groups(cfg_path, &members)?;
    let s = TcpStream::connect_timeout(&addr(&cfg)?, Duration::from_secs(5)).map_err(|e| e.to_string())?;
    // Short, so AI replies go out to the phone within a quarter of a second.
    s.set_read_timeout(Some(Duration::from_millis(250))).map_err(|e| e.to_string())?;
    let mut reader = BufReader::new(s);
    crate::link::set_signal_on(true);
    // Whatever way the session ends, linked computers stop counting on this one.
    struct Off;
    impl Drop for Off {
        fn drop(&mut self) {
            crate::link::set_signal_on(false);
        }
    }
    let _off = Off;
    let mut seen: VecDeque<u64> = VecDeque::new();
    let mut seen_set: HashSet<u64> = HashSet::new();
    let mut out = Outbox { cursors: HashMap::new(), origins: HashMap::new() };
    let mut line = String::new();
    static ANNOUNCED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !ANNOUNCED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        let _ = send_self(&cfg, notice::BACK_ONLINE);
    }
    loop {
        // Stop if Signal was switched off or re-set up; the outer loop starts again.
        let now = BridgeConfig::load(cfg_path)?;
        if !now.enabled || now.account != cfg.account || now.groups != cfg.groups {
            return Ok(());
        }
        if engine.config.get().members.iter().any(|m| !cfg.groups.agents.contains_key(m)) {
            return Ok(()); // a new member needs its own group
        }
        forward(engine, &cfg, &mut out);
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => return Err("signal-cli closed the connection".into()),
            Ok(_) => {}
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => continue,
            Err(e) => return Err(e.to_string()),
        }
        if line.len() > 512 * 1024 {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
        let Some(msg) = parse_incoming(&v, &cfg.account, &cfg.groups) else { continue };
        // The setup's own test reply in Note to Self isn't a message for the council.
        if msg.origin == Origin::Council && crate::signal_setup::state().phase == "verifying" {
            continue;
        }
        if msg.timestamp != 0 {
            if !seen_set.insert(msg.timestamp) {
                continue;
            }
            seen.push_back(msg.timestamp);
            if seen.len() > 2000 {
                if let Some(old) = seen.pop_front() {
                    seen_set.remove(&old);
                }
            }
        }
        // Every linked computer receives each phone message; only one of them answers it.
        if !crate::link_answers_signal() {
            continue;
        }
        let reply = deliver(engine, &mut out, msg);
        if let Err(e) = send_self(&cfg, reply) {
            eprintln!("signal chat: couldn't acknowledge the phone message: {e}");
        }
    }
}

/// Posts a phone message as the human's message in the newest conversation, and returns the
/// ready-made notice that tells the phone what happened to it.
fn deliver(engine: &Arc<Engine>, out: &mut Outbox, msg: Incoming) -> &'static str {
    let Some(conv) = engine.store.list().into_iter().find(|c| !crate::peer_sync::is_mirror(&c.id)) else {
        return notice::NO_CONVERSATION;
    };
    let members = engine.config.get().members;
    let events = engine.store.load(&conv.id).unwrap_or_default();
    let recent = &events[events.len().saturating_sub(400)..];
    let pending = pending_requests(recent);
    let auto = BridgeConfig::load(&bridge::config_path(None)).map(|c| c.auto_forward_decisions).unwrap_or(true);
    let (text, targets, decision) = match &msg.origin {
        Origin::Agent(a) => {
            let mine: Vec<String> = pending.iter().filter(|(_, by)| by == a).map(|(r, _)| r.clone()).collect();
            match bridge::resolve_incoming_message(&msg.text, &mine, auto) {
                ResolvedSignalAction::Decision { formatted_message, .. } => (formatted_message, vec![a.clone()], true),
                _ => (msg.text.clone(), vec![a.clone()], false),
            }
        }
        Origin::Council => {
            let reqs: Vec<String> = pending.iter().map(|(r, _)| r.clone()).collect();
            match bridge::resolve_incoming_message(&msg.text, &reqs, auto) {
                ResolvedSignalAction::Decision { request, formatted_message, .. } => {
                    let by = pending.iter().find(|(r, _)| *r == request).map(|(_, b)| b.clone());
                    let to = by.filter(|b| members.contains(b)).map_or(members.clone(), |b| vec![b]);
                    (formatted_message, to, true)
                }
                _ => (msg.text.clone(), members.clone(), false),
            }
        }
    };
    let targets: Vec<String> = targets.into_iter().filter(|t| members.contains(t)).collect();
    let nobody = targets.is_empty();
    match engine.send_via(&conv.id, &text, vec![], targets, 1, Some("Signal")) {
        Ok(e) => {
            out.origins.insert(e.id.clone(), msg.origin);
            if out.origins.len() > 500 {
                out.origins.clear();
            }
            receipt(nobody, decision)
        }
        Err(e) => {
            eprintln!("signal chat: couldn't post the phone message: {e}");
            notice::NOT_POSTED
        }
    }
}

/// The notice for a phone message that was posted.
pub fn receipt(nobody_can_answer: bool, decision: bool) -> &'static str {
    match (nobody_can_answer, decision) {
        (true, _) => notice::NO_AI,
        (false, true) => notice::DECISION,
        (false, false) => notice::RECEIVED,
    }
}

/// Sends new messages (AI replies, in-app human messages, Consilium notes) across active conversations to Signal.
fn forward(engine: &Arc<Engine>, cfg: &BridgeConfig, out: &mut Outbox) {
    let convs = engine.store.list();
    if convs.is_empty() {
        return;
    }
    for conv in convs.into_iter().filter(|c| !crate::peer_sync::is_mirror(&c.id)).take(10) {
        let events = engine.store.load(&conv.id).unwrap_or_default();
        let newest = events.last().map_or(0, |e| e.seq);
        let cursor = match out.cursors.get(&conv.id) {
            Some(&c) => c,
            None => {
                // Start mirroring from now: older messages aren't replayed to the phone.
                out.cursors.insert(conv.id.clone(), newest);
                continue;
            }
        };
        if newest <= cursor {
            continue;
        }
        let mut last_human: Option<&Event> = events.iter().rev().find(|e| e.seq <= cursor && e.sender == "human");
        for e in events.iter().filter(|e| e.seq > cursor) {
            if e.sender == "human" {
                last_human = Some(e);
            }
            let Some(text) = outbound_text(e) else { continue };
            // An AI answering a message sent from its own group answers there.
            let asked_privately = last_human
                .and_then(|h| out.origins.get(&h.id))
                .is_some_and(|o| *o == Origin::Agent(e.sender.clone()));
            let private_to = asked_privately
                .then(|| crate::agents::by_id(&e.sender).map_or(e.sender.clone(), |a| a.name.to_string()));
            let formatted = self_text(&text, private_to.as_deref());
            if let Err(err) = send_self(cfg, &formatted) {
                eprintln!("signal chat: couldn't send to Signal: {err}");
            }
            let group = if asked_privately { cfg.groups.agents.get(&e.sender) } else { None };
            let group = group.unwrap_or(&cfg.groups.main);
            if !group.is_empty() {
                let _ = send_group(cfg, group, &text);
            }
        }
        out.cursors.insert(conv.id, newest);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACC: &str = "+15551234567";

    fn groups() -> bridge::SignalGroups {
        let mut g = bridge::SignalGroups { main: "MAIN=".into(), ..Default::default() };
        g.agents.insert("claude".into(), "CLAUDE=".into());
        g
    }

    fn sync(source: &str, sent: Value) -> Value {
        json!({"method": "receive", "params": {"envelope": {"sourceNumber": source, "syncMessage": {"sentMessage": sent}}}})
    }

    #[test]
    fn accepts_the_humans_own_messages_in_consiliums_groups() {
        let g = groups();
        let main = sync(ACC, json!({"message": "hi all", "timestamp": 7, "groupInfo": {"groupId": "MAIN="}}));
        assert_eq!(
            parse_incoming(&main, ACC, &g),
            Some(Incoming { origin: Origin::Council, text: "hi all".into(), timestamp: 7 })
        );
        let own = sync(ACC, json!({"message": "just you", "timestamp": 8, "groupInfo": {"groupId": "CLAUDE="}}));
        assert_eq!(parse_incoming(&own, ACC, &g).unwrap().origin, Origin::Agent("claude".into()));
        let note = sync(ACC, json!({"message": "note", "destinationNumber": ACC}));
        assert_eq!(parse_incoming(&note, ACC, &g).unwrap().origin, Origin::Council);
    }

    #[test]
    fn ignores_other_groups_other_chats_and_other_people() {
        let g = groups();
        let other_group = sync(ACC, json!({"message": "x", "groupInfo": {"groupId": "FAMILY="}}));
        assert_eq!(parse_incoming(&other_group, ACC, &g), None);
        let to_friend = sync(ACC, json!({"message": "x", "destinationNumber": "+15550000000"}));
        assert_eq!(parse_incoming(&to_friend, ACC, &g), None);
        let stranger = sync("+15559999999", json!({"message": "x", "groupInfo": {"groupId": "MAIN="}}));
        assert_eq!(parse_incoming(&stranger, ACC, &g), None);
        let incoming = json!({"method": "receive", "params": {"envelope": {"sourceNumber": "+15559999999",
            "dataMessage": {"message": "x", "groupInfo": {"groupId": "MAIN="}}}}});
        assert_eq!(parse_incoming(&incoming, ACC, &g), None);
        assert_eq!(parse_incoming(&main_without_setup(), ACC, &bridge::SignalGroups::default()), None);
    }

    fn main_without_setup() -> Value {
        sync(ACC, json!({"message": "x", "groupInfo": {"groupId": ""}}))
    }

    fn ev(seq: u64, sender: &str, kind: &str, text: &str, requests: Vec<&str>) -> Event {
        let mut e: Event = serde_json::from_value(json!({
            "id": format!("e{seq}"), "seq": seq, "ts": 0, "conversation": "c", "sender": sender,
            "kind": kind, "text": text
        }))
        .unwrap();
        e.requests = requests.into_iter().map(String::from).collect();
        e
    }

    #[test]
    fn every_phone_message_gets_a_notice_back() {
        assert_eq!(receipt(false, false), notice::RECEIVED);
        assert_eq!(receipt(false, true), notice::DECISION);
        assert_eq!(receipt(true, false), notice::NO_AI);
        assert_eq!(receipt(true, true), notice::NO_AI);
        for n in [notice::RECEIVED, notice::DECISION, notice::NO_AI, notice::NOT_POSTED, notice::NO_CONVERSATION, notice::BACK_ONLINE] {
            assert!(n.starts_with("Consilium: "), "{n}");
        }
    }

    #[test]
    fn private_replies_name_their_group() {
        assert_eq!(self_text("[Claude] hi", None), "[Claude] hi");
        assert_eq!(self_text("[Claude] hi", Some("Claude")), "(Consilium · Claude, private)\n[Claude] hi");
    }

    #[test]
    fn answered_requests_are_no_longer_pending() {
        let events = vec![
            ev(1, "claude", "message", "a", vec!["May I delete X?"]),
            ev(2, "gemini", "message", "b", vec!["Install Y?"]),
            ev(3, "human", "message", "Approved: May I delete X?", vec![]),
        ];
        assert_eq!(pending_requests(&events), vec![("Install Y?".to_string(), "gemini".to_string())]);
    }

    #[test]
    fn messages_and_consilium_notes_are_sent() {
        let mut from_phone = ev(1, "human", "message", "hi", vec![]);
        from_phone.via = Some("Signal".into());
        assert_eq!(outbound_text(&from_phone), None);
        let from_mac = ev(2, "human", "message", "hi from mac", vec![]);
        assert_eq!(outbound_text(&from_mac), Some("You: hi from mac".to_string()));
        assert!(outbound_text(&ev(3, "claude", "message", "done", vec![])).unwrap().starts_with("Claude: done"));
        assert!(outbound_text(&ev(4, "consilium", "system", "updated", vec![])).unwrap().starts_with("[Consilium]\nupdated"));
        assert_eq!(outbound_text(&ev(5, "claude", "seen", "", vec![])), None);
        let long = outbound_text(&ev(6, "claude", "message", &"x".repeat(9000), vec![])).unwrap();
        assert!(long.chars().count() < MAX_OUT + 50 && long.ends_with("(the rest is in Consilium)"));
    }
}

#[cfg(test)]
mod live {
    /// Creates the real groups on the linked account. Run by hand: `cargo test --lib live_groups -- --ignored`.
    #[test]
    #[ignore]
    fn live_groups() {
        let members: Vec<String> = ["claude", "gpt", "gemini"].iter().map(|s| s.to_string()).collect();
        let cfg = super::ensure_groups(&crate::bridge::config_path(None), &members).unwrap();
        assert!(!cfg.groups.main.is_empty());
        assert_eq!(cfg.groups.agents.len(), 3);
    }
}
