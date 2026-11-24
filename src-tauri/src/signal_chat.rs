//! Carries the conversation over Signal, both ways, once Signal setup is verified.
//!
//! Consilium creates Signal groups on the linked account: "Consilium" for the whole council and
//! "Consilium · <AI>" for each member, so the human can message one AI on its own. What the human
//! types in those groups on their phone becomes their message in the newest
//! conversation; "Approve" or "Deny" answers a single pending REQUEST. Each AI's message is sent
//! back to the group it was asked from, and Consilium's own notes go to the council group.
//!
//! Only messages from the linked account itself are accepted (Signal delivers what the human
//! sends from their phone to this linked device as a "sync" message), and only in Consilium's own
//! groups. Other chats and other people are ignored.

use crate::bridge::{self, BridgeConfig, ResolvedSignalAction};
use crate::engine::Engine;
use crate::store::Event;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[path = "signal_subscriber.rs"]
mod subscriber;

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
    /// The Consilium council group.
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
    /// Files sent with the message (pictures, documents), still on Signal's side.
    pub files: Vec<RemoteFile>,
    /// The configured Signal group it was sent in.
    pub group: String,
}

/// A file attached to a phone message, fetched from signal-cli with `getAttachment`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteFile {
    pub id: String,
    pub name: String,
}

/// Most files taken from one phone message.
const MAX_FILES: usize = 10;

/// A file name for a Signal attachment: its own name, or one made from its type.
fn attachment_name(a: &Value, id: &str) -> String {
    if let Some(n) = a.get("filename").and_then(Value::as_str).map(str::trim).filter(|n| !n.is_empty()) {
        return n.to_string();
    }
    let ext = match a.get("contentType").and_then(Value::as_str).unwrap_or("") {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/heic" => "heic",
        "application/pdf" => "pdf",
        "text/plain" => "txt",
        "video/mp4" => "mp4",
        "audio/aac" => "aac",
        "audio/mpeg" => "mp3",
        _ => "bin",
    };
    let short: String = id.chars().filter(char::is_ascii_alphanumeric).take(8).collect();
    format!("signal-{short}.{ext}")
}

/// Decodes standard base64 (as signal-cli returns attachment data); None if it isn't valid.
fn decode_base64(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' | b'\n' | b'\r' | b' ' => continue,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// Downloads one attachment of a phone message from signal-cli.
fn fetch_file(cfg: &BridgeConfig, group: &str, f: &RemoteFile) -> Result<Vec<u8>, String> {
    let r = call(cfg, "getAttachment", json!({"id": f.id, "groupId": group}))?;
    let data = r.get("data").and_then(Value::as_str).or_else(|| r.as_str()).ok_or("no data")?;
    if data.len() as u64 > crate::attach::MAX_BYTES * 4 / 3 + 16 {
        return Err("too large".into());
    }
    decode_base64(data).ok_or_else(|| "bad data".to_string())
}

/// The envelope of a daemon `receive` notification. A client that sent `subscribeReceive` (as
/// Consilium does, since the daemon runs in manual receive mode) gets it at
/// `params.result.envelope`; a daemon that receives on its own puts it at `params.envelope`.
pub fn receive_envelope(v: &Value) -> Option<&Value> {
    if v.get("method").and_then(Value::as_str) != Some("receive") {
        return None;
    }
    let params = v.get("params")?;
    params.pointer("/result/envelope").or_else(|| params.get("envelope")).filter(|e| e.is_object())
}

/// Reads a daemon `receive` notification. Returns the human's own message when it was sent into
/// one of Consilium's groups; everything else is None.
pub fn parse_incoming(v: &Value, account: &str, groups: &bridge::SignalGroups) -> Option<Incoming> {
    let env = receive_envelope(v)?;
    let from_self = env.get("sourceNumber").and_then(Value::as_str) == Some(account)
        || env.get("source").and_then(Value::as_str) == Some(account);
    if !from_self || account.is_empty() {
        return None;
    }
    let sent = env.pointer("/syncMessage/sentMessage")?;
    let text = sent.get("message").and_then(Value::as_str).unwrap_or("").trim();
    let files: Vec<RemoteFile> = sent
        .get("attachments")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|a| {
                    let id = a.get("id").and_then(Value::as_str)?.to_string();
                    Some(RemoteFile { name: attachment_name(a, &id), id })
                })
                .take(MAX_FILES)
                .collect()
        })
        .unwrap_or_default();
    if text.is_empty() && files.is_empty() {
        return None;
    }
    let timestamp = sent.get("timestamp").and_then(Value::as_u64).unwrap_or(0);
    let group = sent.pointer("/groupInfo/groupId").and_then(Value::as_str).filter(|g| !g.is_empty())?;
    let origin = if group == groups.main {
        Origin::Council
    } else {
        Origin::Agent(groups.agents.iter().find(|(_, id)| id.as_str() == group)?.0.clone())
    };
    Some(Incoming { origin, text: text.chars().take(8000).collect(), timestamp, files, group: group.into() })
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
    // The startup check sends its report to the phone itself.
    if name == "Consilium" && body.starts_with(crate::startup_check::HEADER) {
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
    send_group_files(cfg, group, text, &[])
}

/// Sends a Consilium report to the council group when the phone link is active.
pub fn send_to_phone(text: &str) -> Result<(), String> {
    let cfg = BridgeConfig::load(&bridge::config_path(None))?;
    if !cfg.enabled || !bridge::valid_account(&cfg.account) || cfg.allowed_acis.is_empty() {
        return Err("Signal isn't set up".into());
    }
    if !crate::link::signal_on() {
        return Err("Signal isn't connected".into());
    }
    let mut msg = crate::redact::scrub(text);
    if msg.chars().count() > MAX_OUT {
        msg = msg.chars().take(MAX_OUT).collect::<String>() + notice::CUT;
    }
    send_group(&cfg, &cfg.groups.main, &msg)
}

/// Include the account itself even when the person's phone is the group's only member.
pub(crate) fn group_send_params(group: &str, text: &str, files: &[String]) -> Value {
    let mut params = json!({ "groupId": group, "notifySelf": true, "message": text });
    if !files.is_empty() {
        params["attachments"] = json!(files);
    }
    params
}

pub(crate) fn group_send_accepted(cfg: Option<&BridgeConfig>, group: &str, result: &Value) -> bool {
    result.get("results").and_then(Value::as_array).is_some_and(|recipients| {
        recipients.iter().any(|entry| {
            if entry.get("type").and_then(Value::as_str) != Some("SUCCESS")
                || entry.get("groupId").and_then(Value::as_str) != Some(group)
            {
                return false;
            }
            cfg.is_none_or(|cfg| {
                let number = entry.pointer("/recipientAddress/number").and_then(Value::as_str);
                let uuid = entry.pointer("/recipientAddress/uuid").and_then(Value::as_str);
                number == Some(cfg.account.as_str())
                    || uuid.is_some_and(|uuid| cfg.allowed_acis.iter().any(|aci| aci.eq_ignore_ascii_case(uuid)))
            })
        })
    })
}

fn send_group_files(cfg: &BridgeConfig, group: &str, text: &str, files: &[String]) -> Result<(), String> {
    if group.is_empty() {
        return Err("The Consilium Signal group has not been set up".into());
    }
    let result = call(cfg, "send", group_send_params(group, text, files))?;
    if !group_send_accepted(Some(cfg), group, &result) {
        return Err("Signal did not accept this group message for your account".into());
    }
    Ok(())
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
/// Asks signal-cli (in manual receive mode) to start passing messages to this connection.
pub const SUBSCRIBE: &str = subscriber::SUBSCRIBE_REQUEST;

/// Reads one whole line from signal-cli. A read timeout part-way through a line keeps what has
/// arrived in `buf` for the next call, so a long message (a photo, a long text) that arrives in
/// pieces isn't cut in two and lost. Returns None while the line is still incomplete.
pub fn read_whole_line(r: &mut impl BufRead, buf: &mut Vec<u8>) -> Result<Option<String>, String> {
    match r.read_until(b'\n', buf) {
        Ok(0) => return Err("signal-cli closed the connection".into()),
        Ok(_) => {}
        Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
        Err(e) => return Err(e.to_string()),
    }
    if buf.last() != Some(&b'\n') {
        if buf.len() > 16 * 1024 * 1024 {
            buf.clear(); // no line is ever this long; don't hold on to it
        }
        return Ok(None);
    }
    let line = String::from_utf8_lossy(buf).into_owned();
    buf.clear();
    Ok(Some(line))
}

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

fn same_receiver_config(now: &BridgeConfig, expected: &BridgeConfig) -> bool {
    now.enabled && now.account == expected.account && now.endpoint == expected.endpoint
        && now.groups == expected.groups && now.allowed_acis == expected.allowed_acis
        && !now.allowed_acis.is_empty()
}

fn session(engine: &Arc<Engine>, cfg_path: &std::path::Path) -> Result<(), String> {
    crate::link::set_signal_on(false);
    let cfg = BridgeConfig::load(cfg_path)?;
    if cfg.enabled && bridge::valid_account(&cfg.account) && cfg.allowed_acis.is_empty() {
        // Saved but never verified: say so once (not every 5 s), so it isn't mistaken for a dead bridge.
        static SAID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if !SAID.swap(true, std::sync::atomic::Ordering::Relaxed) {
            eprintln!("signal chat: number saved but not verified (no allowed sender yet); waiting for the Consilium group code from Settings > Signal");
        }
    }
    if !cfg.enabled || !bridge::valid_account(&cfg.account) || cfg.allowed_acis.is_empty() {
        // Not set up, switched off, or the setup's round trip hasn't been verified yet.
        return Ok(());
    }
    let members = engine.config.get().members;
    let cfg = match ensure_groups(cfg_path, &members) {
        Ok(c) => c,
        Err(e) => {
            // Usually signal-cli itself has stopped: start it again rather than waiting forever.
            crate::signal_setup::revive(&BridgeConfig::load(cfg_path)?);
            return Err(e);
        }
    };
    let expected = cfg.clone();
    let current_path = cfg_path.to_path_buf();
    let decode_cfg = cfg.clone();
    let subscription = subscriber::start(addr(&cfg)?, move || {
        BridgeConfig::load(&current_path).map(|now| same_receiver_config(&now, &expected))
    }, move |value| {
        let msg = parse_incoming(value, &decode_cfg.account, &decode_cfg.groups)?;
        // The setup code is consumed by the verifier, not sent to the council.
        let setup = crate::signal_setup::state();
        if msg.origin == Origin::Council && setup.phase == "verifying" && setup.code.as_deref() == Some(msg.text.as_str()) {
            return None;
        }
        Some(msg)
    }, crate::link::set_signal_on)?;
    let mut revive_after = Instant::now();
    let mut seen: VecDeque<u64> = VecDeque::new();
    let mut seen_set: HashSet<u64> = HashSet::new();
    let mut out = Outbox { cursors: HashMap::new(), origins: HashMap::new() };
    static ANNOUNCED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    loop {
        // Stop if Signal was switched off or re-set up; the outer loop starts again.
        let now = match BridgeConfig::load(cfg_path) {
            Ok(now) => now,
            // A short config-write gap pauses both dispatch and the socket;
            // accepted messages stay in this configuration's bounded channel.
            Err(_) => { std::thread::sleep(Duration::from_millis(250)); continue; }
        };
        if !same_receiver_config(&now, &cfg) { return Ok(()); }
        if engine.config.get().members.iter().any(|m| !cfg.groups.agents.contains_key(m)) {
            return Ok(()); // a new member needs its own group
        }
        if subscription.take_connect_failure() && Instant::now() >= revive_after {
            crate::signal_setup::revive(&cfg);
            revive_after = Instant::now() + Duration::from_secs(5);
        }
        let msg = match subscription.recv_timeout(Duration::from_millis(250)) {
            Ok(message) => message,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if subscription.is_ready() {
                    if ANNOUNCED.swap(true, std::sync::atomic::Ordering::SeqCst) {
                        forward(engine, &cfg, &mut out);
                    } else {
                        let _ = send_group(&cfg, &cfg.groups.main, notice::BACK_ONLINE);
                    }
                }
                continue;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return Err("Signal subscriber stopped".into()),
        };
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
        let group_id = msg.group.clone();
        let reply = deliver(engine, &cfg, &mut out, msg);
        if let Err(e) = send_group(&cfg, &group_id, &reply) {
            eprintln!("signal chat: couldn't acknowledge the phone message to its group: {e}");
        }
    }
}

/// Posts a phone message as the human's message in the newest conversation, and returns the
/// ready-made notice that tells the phone what happened to it.
fn deliver(engine: &Arc<Engine>, cfg: &BridgeConfig, out: &mut Outbox, msg: Incoming) -> String {
    let convs = engine.store.list();
    let Some(conv) = convs.iter().find(|c| !crate::peer_sync::is_mirror(&c.id)).or_else(|| convs.first()) else {
        return notice::NO_CONVERSATION.to_string();
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
    // Pictures and documents sent from the phone are saved into the conversation, so every AI
    // sees them like files attached in the app.
    let mut attachments = Vec::new();
    let mut failed = 0;
    for f in &msg.files {
        match fetch_file(cfg, &msg.group, f).and_then(|b| engine.attach_bytes(&conv.id, &f.name, &b)) {
            Ok(a) => attachments.push(a),
            Err(e) => {
                eprintln!("signal chat: couldn't take {} from Signal: {e}", f.name);
                failed += 1;
            }
        }
    }
    let mut text = text;
    if failed > 0 {
        text.push_str(&format!("\n\n({failed} file(s) sent from the phone couldn't be downloaded from Signal.)"));
    }
    let text = text.trim().to_string();
    if text.is_empty() && attachments.is_empty() {
        return notice::NOT_POSTED.to_string();
    }
    match engine.send_via(&conv.id, &text, attachments, targets.clone(), 1, Some("Signal")) {
        Ok(e) => {
            out.origins.insert(e.id.clone(), msg.origin);
            if out.origins.len() > 500 {
                out.origins.clear();
            }
            formatted_receipt(nobody, decision, if e.to.is_empty() { &targets } else { &e.to })
        }
        Err(e) => {
            eprintln!("signal chat: couldn't post the phone message: {e}");
            notice::NOT_POSTED.to_string()
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

/// Formats a specific verification notice for a phone message, indicating which AI is assigned or taking the task.
pub fn formatted_receipt(nobody_can_answer: bool, decision: bool, targets: &[String]) -> String {
    if nobody_can_answer {
        return receipt(true, decision).to_string();
    }
    let names: Vec<String> = targets
        .iter()
        .map(|id| crate::agents::by_id(id).map_or(id.clone(), |a| a.name.to_string()))
        .collect();
    if decision {
        if names.is_empty() {
            receipt(false, true).to_string()
        } else {
            format!("Consilium: Decision received and passed to {}.", names.join(", "))
        }
    } else if names.is_empty() {
        receipt(false, false).to_string()
    } else if names.len() == 1 {
        format!("Consilium: Message received. Assigned to {}. Please wait for the response.", names[0])
    } else {
        format!("Consilium: Message received. Assigned to the council ({}). Please wait for the response.", names.join(", "))
    }
}

/// Sends new messages (AI replies, in-app human messages, Consilium notes) across active conversations to Signal.
fn forward(engine: &Arc<Engine>, cfg: &BridgeConfig, out: &mut Outbox) {
    let convs = engine.store.list();
    if convs.is_empty() {
        return;
    }
    let answers = crate::link_answers_signal();
    for conv in convs.into_iter().filter(|c| answers || !crate::peer_sync::is_mirror(&c.id)).take(10) {
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
            // Files attached in the app (by you or an AI) go to the phone with the message; files
            // that came from the phone aren't sent back.
            let from_phone = e.via.as_deref().is_some_and(|v| v.eq_ignore_ascii_case("signal"));
            let dir = engine.store.files_dir(&conv.id);
            let files: Vec<String> = if from_phone {
                vec![]
            } else {
                e.attachments
                    .iter()
                    .filter(|a| crate::attach::belongs(&dir, a) && std::path::Path::new(&a.path).is_file())
                    .take(MAX_FILES)
                    .map(|a| a.path.clone())
                    .collect()
            };
            let text = match outbound_text(e) {
                Some(t) => t,
                None if !files.is_empty() && e.sender == "human" => format!("You: (sent {} file(s))", files.len()),
                None => continue,
            };
            // Keep replies in the group that asked. The council group is the default for
            // desktop messages and for mirrored conversations with no local origin record.
            let origin = last_human.and_then(|h| out.origins.get(&h.id));
            if let Some(group) = outbound_group(&cfg.groups, origin) {
                if let Err(err) = send_group_files(cfg, group, &text, &files) {
                    eprintln!("signal chat: couldn't send to the Signal group: {err}");
                }
            }
        }
        out.cursors.insert(conv.id, newest);
    }
}

fn outbound_group<'a>(groups: &'a bridge::SignalGroups, origin: Option<&Origin>) -> Option<&'a str> {
    let group = match origin {
        Some(Origin::Agent(agent)) => groups.agents.get(agent)?,
        _ => &groups.main,
    };
    (!group.is_empty()).then_some(group.as_str())
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
            Some(Incoming {
                origin: Origin::Council,
                text: "hi all".into(),
                timestamp: 7,
                files: vec![],
                group: "MAIN=".into()
            })
        );
        let own = sync(ACC, json!({"message": "just you", "timestamp": 8, "groupInfo": {"groupId": "CLAUDE="}}));
        assert_eq!(parse_incoming(&own, ACC, &g).unwrap().origin, Origin::Agent("claude".into()));
        let note = sync(ACC, json!({"message": "note", "destinationNumber": ACC}));
        assert!(parse_incoming(&note, ACC, &g).is_none());
    }

    #[test]
    fn takes_files_sent_from_the_phone() {
        let g = groups();
        let pic = sync(ACC, json!({"timestamp": 9, "groupInfo": {"groupId": "MAIN="},
            "attachments": [{"id": "abc123XYZ.jpg", "contentType": "image/jpeg"},
                            {"id": "q2", "contentType": "application/pdf", "filename": "plan.pdf"}]}));
        let m = parse_incoming(&pic, ACC, &g).expect("a picture with no text is still a message");
        assert_eq!(m.text, "");
        assert_eq!(m.group, "MAIN=");
        assert_eq!(m.files.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["signal-abc123XY.jpg", "plan.pdf"]);
        assert_eq!(decode_base64("aGVsbG8gd29ybGQ=").unwrap(), b"hello world");
        assert_eq!(decode_base64("aGk").unwrap(), b"hi");
        assert_eq!(decode_base64("a*b"), None);
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

    #[test]
    fn reads_messages_in_the_subscribed_shape() {
        // What signal-cli actually sends to a client that asked with `subscribeReceive`.
        let g = groups();
        let wrap = |legacy: Value| {
            let env = legacy["params"]["envelope"].clone();
            json!({"jsonrpc": "2.0", "method": "receive",
                "params": {"subscription": 0, "result": {"account": ACC, "envelope": env}}})
        };
        let main = wrap(sync(ACC, json!({"message": "hello", "groupInfo": {"groupId": "MAIN="}, "timestamp": 7})));
        let got = parse_incoming(&main, ACC, &g).expect("the council group in the subscribed shape is read");
        assert_eq!(got.text, "hello");
        assert_eq!(got.timestamp, 7);
        let stranger = wrap(sync("+15559999999", json!({"message": "x", "groupInfo": {"groupId": "MAIN="}})));
        assert_eq!(parse_incoming(&stranger, ACC, &g), None);
        assert!(receive_envelope(&json!({"jsonrpc": "2.0", "id": "x", "result": 0})).is_none());
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

    /// Hands out its chunks one per read, with a read timeout between them, like a socket.
    struct Chunky(Vec<Vec<u8>>, bool);
    impl std::io::Read for Chunky {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            self.1 = !self.1;
            if self.1 {
                return Err(std::io::ErrorKind::WouldBlock.into());
            }
            if self.0.is_empty() {
                return Ok(0);
            }
            let mut c = self.0.remove(0);
            let n = c.len().min(out.len());
            out[..n].copy_from_slice(&c[..n]);
            if n < c.len() {
                self.0.insert(0, c.split_off(n));
            }
            Ok(n)
        }
    }

    #[test]
    fn a_message_split_by_a_read_timeout_arrives_whole() {
        let msg = sync("+15551234567", json!({"message": "héllo", "destinationNumber": "+15551234567"})).to_string();
        let bytes = format!("{msg}\n").into_bytes();
        // Split inside the two-byte "é" as well, and between lines.
        let cut = msg.find('é').unwrap() + 1;
        let chunks = vec![bytes[..10].to_vec(), bytes[10..cut].to_vec(), bytes[cut..].to_vec(), b"{\"a\":1}\n".to_vec()];
        let mut r = BufReader::with_capacity(4, Chunky(chunks, false));
        let mut buf = Vec::new();
        let mut lines = Vec::new();
        loop {
            match read_whole_line(&mut r, &mut buf) {
                Ok(Some(l)) => lines.push(l),
                Ok(None) => {}
                Err(_) => break,
            }
        }
        assert_eq!(lines, vec![format!("{msg}\n"), "{\"a\":1}\n".to_string()]);
        let v: Value = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(parse_incoming(&v, "+15551234567", &groups()).unwrap().text, "héllo");
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
    fn formatted_receipt_specifies_assigned_ai_and_council() {
        assert_eq!(
            formatted_receipt(false, false, &["gemini".into()]),
            "Consilium: Message received. Assigned to Gemini. Please wait for the response."
        );
        assert_eq!(
            formatted_receipt(false, false, &["claude".into(), "gemini".into()]),
            "Consilium: Message received. Assigned to the council (Claude, Gemini). Please wait for the response."
        );
        assert_eq!(
            formatted_receipt(false, true, &["claude".into()]),
            "Consilium: Decision received and passed to Claude."
        );
        assert_eq!(
            formatted_receipt(true, false, &[]),
            notice::NO_AI
        );
    }

    #[test]
    fn replies_use_one_group_and_include_the_owner_as_a_recipient() {
        let g = groups();
        assert_eq!(outbound_group(&g, None), Some("MAIN="));
        assert_eq!(outbound_group(&g, Some(&Origin::Council)), Some("MAIN="));
        assert_eq!(outbound_group(&g, Some(&Origin::Agent("claude".into()))), Some("CLAUDE="));
        assert_eq!(outbound_group(&g, Some(&Origin::Agent("unknown".into()))), None);

        let params = group_send_params("MAIN=", "reply", &["phone-report.pdf".into()]);
        assert_eq!(params, json!({"groupId":"MAIN=", "message":"reply", "notifySelf":true, "attachments":["phone-report.pdf"]}));

        let cfg = BridgeConfig { account: ACC.into(), ..Default::default() };
        let success = |group, number| json!({"results":[{"groupId":group, "type":"SUCCESS", "recipientAddress":{"number":number}}]});
        assert!(group_send_accepted(Some(&cfg), "MAIN=", &success("MAIN=", ACC)));
        assert!(!group_send_accepted(Some(&cfg), "MAIN=", &success("MAIN=", "+15550000000")));
        assert!(!group_send_accepted(Some(&cfg), "MAIN=", &success("WRONG=", ACC)));
        assert!(!group_send_accepted(Some(&cfg), "MAIN=", &json!({"timestamp":42, "results":[]})));
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
