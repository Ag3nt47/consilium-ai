//! Phone commands in a dedicated Signal group. This listener runs outside the desktop app.
use crate::bridge::{self, BridgeConfig};
use crate::signal_chat::{self, subscriber, Incoming};
use crate::watchdog_power::PowerTarget;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::ErrorKind;
use std::path::Path;
use std::thread;
use std::time::Duration;

#[path = "signal_watchdog_inbox.rs"]
pub(crate) mod inbox;

const GROUP_NAME: &str = "Consilium Watchdog";
const GROUP_DESCRIPTION: &str = "Private recovery commands for Consilium. Send restart, restart signal, or status from your primary phone.";
#[cfg(windows)]
const WELCOME: &str = "Consilium Watchdog is separate from the council. From your primary Signal phone, send restart to relaunch Consilium, restart signal to request bridge recovery, or status to check this computer.";
const ENROLLMENT_FILE: &str = "signal-watchdog.json";
const MAX_COMMAND_AGE_MS: u64 = 5 * 60_000;
const MAX_FUTURE_MS: u64 = 30_000;
const MAX_SLEEP_COMMAND_AGE_MS: u64 = 60_000;
const MAX_WAKE_COMMAND_AGE_MS: u64 = 2 * 60 * 60_000;
const RESTART_COOLDOWN_MS: u64 = 60_000;
const POWER_COOLDOWN_MS: u64 = 15_000;
const COMMAND_LIST: &str = include_str!("watchdog_commands.txt");
const MAX_RECENT_COMMANDS: usize = 128;
const VERIFIED_RECEIPT: &str = "Consilium Watchdog received and verified your command.";
const UNRECOGNIZED_RECEIPT: &str =
    "Consilium Watchdog received your message. Send a listed command as plain text to run it.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    RestartApp,
    RestartSignal,
    Status,
    Wake(PowerTarget),
    Sleep(PowerTarget),
    PowerStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    Desktop,
    PowerRelay,
}

impl Mode {
    fn accepts(self, action: Action) -> bool {
        matches!(
            (self, action),
            (
                Self::Desktop,
                Action::RestartApp | Action::RestartSignal | Action::Status
            ) | (
                Self::PowerRelay,
                Action::Wake(_) | Action::Sleep(_) | Action::PowerStatus
            )
        )
    }

    fn lock_file(self) -> &'static str {
        match self {
            Self::Desktop => "signal-watchdog.lock",
            Self::PowerRelay => "signal-watchdog-power.lock",
        }
    }

    fn enrollment_file(self) -> &'static str {
        match self {
            Self::Desktop => ENROLLMENT_FILE,
            Self::PowerRelay => "signal-watchdog-power.json",
        }
    }

    fn replay_file(self) -> &'static str {
        match self {
            Self::Desktop => "signal-watchdog-commands.json",
            Self::PowerRelay => "signal-watchdog-power-commands.json",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Command {
    action: Action,
    timestamp: u64,
}

#[derive(Deserialize, Serialize, PartialEq, Eq)]
struct Enrollment {
    identity: String,
    group: Option<String>,
    #[serde(default)]
    announced: bool,
}

#[derive(Default, Deserialize, Serialize)]
struct Replay {
    timestamps: Vec<u64>,
    #[serde(default)]
    wake_timestamps: Vec<u64>,
    app_restart: Option<u64>,
    signal_restart: Option<u64>,
    #[serde(default)]
    power: [Option<u64>; 3],
}

#[derive(Debug, PartialEq, Eq)]
enum Admission {
    Ignored,
    CoolingDown,
    Accepted,
}

impl Replay {
    fn admit(&mut self, command: Command, now: u64) -> Admission {
        let (max_age, retain_age, timestamps) = match command.action {
            Action::Wake(_) => (
                MAX_WAKE_COMMAND_AGE_MS,
                MAX_WAKE_COMMAND_AGE_MS,
                &mut self.wake_timestamps,
            ),
            Action::Sleep(_) => (
                MAX_SLEEP_COMMAND_AGE_MS,
                MAX_COMMAND_AGE_MS,
                &mut self.timestamps,
            ),
            _ => (MAX_COMMAND_AGE_MS, MAX_COMMAND_AGE_MS, &mut self.timestamps),
        };
        if !remember(timestamps, command.timestamp, now, max_age, retain_age) {
            return Admission::Ignored;
        }
        let last = match command.action {
            Action::RestartApp => &mut self.app_restart,
            Action::RestartSignal => &mut self.signal_restart,
            Action::Status | Action::PowerStatus => return Admission::Accepted,
            Action::Wake(target) | Action::Sleep(target) => return self.admit_power(target, now),
        };
        if last.is_some_and(|previous| now.saturating_sub(previous) < RESTART_COOLDOWN_MS) {
            return Admission::CoolingDown;
        }
        *last = Some(now);
        Admission::Accepted
    }

    fn admit_unrecognized(&mut self, timestamp: u64, now: u64) -> Admission {
        if remember(
            &mut self.timestamps,
            timestamp,
            now,
            MAX_COMMAND_AGE_MS,
            MAX_COMMAND_AGE_MS,
        ) {
            Admission::Accepted
        } else {
            Admission::Ignored
        }
    }

    fn admit_power(&mut self, target: PowerTarget, now: u64) -> Admission {
        let affected: &[usize] = match target {
            PowerTarget::Windows => &[0],
            PowerTarget::Mac => &[1],
            PowerTarget::Linux => &[2],
            PowerTarget::Both => &[0, 1, 2],
        };
        if affected.iter().any(|&slot| {
            self.power[slot]
                .is_some_and(|previous| now.saturating_sub(previous) < POWER_COOLDOWN_MS)
        }) {
            return Admission::CoolingDown;
        }
        for &slot in affected {
            self.power[slot] = Some(now);
        }
        Admission::Accepted
    }
}

fn remember(
    timestamps: &mut Vec<u64>,
    timestamp: u64,
    now: u64,
    max_age: u64,
    retain_age: u64,
) -> bool {
    timestamps.retain(|timestamp| now.saturating_sub(*timestamp) <= retain_age);
    if timestamp == 0
        || now.saturating_sub(timestamp) > max_age
        || timestamp.saturating_sub(now) > MAX_FUTURE_MS
        || timestamps.len() >= MAX_RECENT_COMMANDS
        || timestamps.contains(&timestamp)
    {
        return false;
    }
    timestamps.push(timestamp);
    true
}

#[cfg(windows)]
pub(crate) fn worker_running(data: &Path) -> Result<bool, String> {
    try_profile_lock(data, Mode::Desktop).map(|lock| lock.is_none())
}

fn profile_lock(data: &Path, mode: Mode) -> Result<File, String> {
    try_profile_lock(data, mode)?
        .ok_or_else(|| "Consilium's Signal watchdog is already running for this profile.".into())
}

fn try_profile_lock(data: &Path, mode: Mode) -> Result<Option<File>, String> {
    fs::create_dir_all(data)
        .map_err(|e| format!("Couldn't open the Signal watchdog data folder: {e}"))?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(data.join(mode.lock_file()))
        .map_err(|e| format!("Couldn't open the Signal watchdog lock: {e}"))?;
    match lock.try_lock() {
        Ok(()) => Ok(Some(lock)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(e)) => {
            Err(format!("Couldn't lock the Signal watchdog profile: {e}"))
        }
    }
}

fn load<T: serde::de::DeserializeOwned>(file: &Path) -> Result<Option<T>, String> {
    match fs::read(file) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| "A local Signal watchdog file is invalid.".into()),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("Couldn't read local Signal watchdog data: {e}")),
    }
}

fn save<T: Serialize>(file: &Path, data: &T) -> Result<(), String> {
    let bytes =
        serde_json::to_vec(data).map_err(|_| "Couldn't encode local Signal watchdog data.")?;
    crate::link::write_private(file, &bytes)
}

fn identity(cfg: &BridgeConfig) -> Option<String> {
    if !cfg.enabled || !bridge::valid_account(&cfg.account) {
        return None;
    }
    let mut allowed: Vec<uuid::Uuid> = cfg
        .allowed_acis
        .iter()
        .filter_map(|aci| uuid::Uuid::parse_str(aci.trim()).ok())
        .collect();
    allowed.sort_unstable();
    allowed.dedup();
    if allowed.is_empty() {
        return None;
    }
    let mut digest = Sha256::new();
    digest.update(cfg.account.as_bytes());
    for aci in allowed {
        digest.update(aci.as_bytes());
    }
    Some(format!("{:x}", digest.finalize()))
}

fn configured_group<'a>(cfg: &BridgeConfig, id: &'a str) -> Option<&'a str> {
    if id.is_empty() || id == cfg.groups.main || cfg.groups.agents.values().any(|other| other == id)
    {
        return None;
    }
    Some(id)
}

fn verified_envelope<'a>(value: &'a Value, cfg: &BridgeConfig) -> Option<&'a Value> {
    let env = signal_chat::receive_envelope(value)?;
    let source = env
        .get("sourceNumber")
        .and_then(Value::as_str)
        .or_else(|| env.get("source").and_then(Value::as_str));
    let aci = env
        .get("sourceUuid")
        .and_then(Value::as_str)
        .or_else(|| env.get("sourceAci").and_then(Value::as_str))?;
    (source == Some(cfg.account.as_str()) && cfg.is_sender_allowed(aci)).then_some(env)
}

struct WatchdogMessage {
    timestamp: u64,
    action: Option<Action>,
}

fn watchdog_message(value: &Value, cfg: &BridgeConfig, group: &str) -> Option<WatchdogMessage> {
    let env = verified_envelope(value, cfg)?;
    // Linked desktops and daemon acknowledgments are never phone requests.
    if env.get("sourceDevice").and_then(Value::as_u64) != Some(1) {
        return None;
    }
    let sent = env.pointer("/syncMessage/sentMessage")?;
    if sent.pointer("/groupInfo/groupId").and_then(Value::as_str) != Some(group) {
        return None;
    }
    let text = sent
        .get("message")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty());
    let has_attachments = match sent.get("attachments") {
        None | Some(Value::Null) => false,
        Some(files) => !files.as_array()?.is_empty(),
    };
    // Read receipts, reactions and group updates without a chat message do not get a reply.
    if text.is_none() && !has_attachments {
        return None;
    }
    Some(WatchdogMessage {
        action: if has_attachments {
            None
        } else {
            text.and_then(parse_action)
        },
        timestamp: sent.get("timestamp")?.as_u64()?,
    })
}

#[cfg(test)]
fn command(value: &Value, cfg: &BridgeConfig, group: &str) -> Option<Command> {
    let message = watchdog_message(value, cfg, group)?;
    Some(Command {
        action: message.action?,
        timestamp: message.timestamp,
    })
}

fn parse_action(text: &str) -> Option<Action> {
    let command = text.trim().to_ascii_lowercase();
    match command.as_str() {
        "restart" | "restart app" => Some(Action::RestartApp),
        "restart signal" => Some(Action::RestartSignal),
        "status" => Some(Action::Status),
        "power status" => Some(Action::PowerStatus),
        text => {
            let (verb, name) = text.split_once(' ')?;
            let target = match name {
                "windows" => PowerTarget::Windows,
                "mac" => PowerTarget::Mac,
                "linux" => PowerTarget::Linux,
                "both" => PowerTarget::Both,
                _ => return None,
            };
            match verb {
                "wake" => Some(Action::Wake(target)),
                "sleep" => Some(Action::Sleep(target)),
                _ => None,
            }
        }
    }
}

enum Received {
    Command(Command),
    Unrecognized(u64),
    Council(Incoming),
}

fn received(value: &Value, cfg: &BridgeConfig, group: &str, mode: Mode) -> Option<Received> {
    if let Some(message) = watchdog_message(value, cfg, group) {
        return match message.action {
            Some(action) => mode.accepts(action).then_some(Received::Command(Command {
                action,
                timestamp: message.timestamp,
            })),
            None => (mode == Mode::Desktop).then_some(Received::Unrecognized(message.timestamp)),
        };
    }
    if mode == Mode::PowerRelay {
        return None;
    }
    verified_envelope(value, cfg)?;
    let message = signal_chat::verified_incoming(value, cfg)?;
    bufferable_council_text(&message.text).then_some(Received::Council(message))
}

pub(crate) fn bufferable_council_text(text: &str) -> bool {
    let text = text.trim();
    // These need the live request or verification context, which may have changed before replay.
    !(text.len() == 6 && text.bytes().all(|b| b.is_ascii_digit()))
        && !text.starts_with("Approved:")
        && !text.starts_with("Denied:")
        && matches!(
            bridge::resolve_incoming_message(text, &["pending approval".into()], true),
            bridge::ResolvedSignalAction::Prompt { .. }
        )
}

fn private_group(cfg: &BridgeConfig, group: &Value) -> bool {
    group.get("name").and_then(Value::as_str) == Some(GROUP_NAME)
        && group.get("description").and_then(Value::as_str) == Some(GROUP_DESCRIPTION)
        && group.get("isMember").and_then(Value::as_bool) == Some(true)
        && group.get("isBlocked").and_then(Value::as_bool) == Some(false)
        && group.get("isTerminated").and_then(Value::as_bool) != Some(true)
        && group.get("groupInviteLink").is_none_or(Value::is_null)
        && ["pendingMembers", "requestingMembers"].iter().all(|key| {
            group
                .get(key)
                .and_then(Value::as_array)
                .is_some_and(Vec::is_empty)
        })
        && group
            .get("members")
            .and_then(Value::as_array)
            .is_some_and(|members| {
                matches!(members.as_slice(), [member] if member.get("uuid").and_then(Value::as_str)
                .is_some_and(|aci| cfg.is_sender_allowed(aci)))
            })
}

fn find_group(cfg: &BridgeConfig, groups: &Value) -> Result<Option<String>, String> {
    let groups = groups
        .as_array()
        .ok_or("Signal returned an unexpected group list.")?;
    let mut matches = groups
        .iter()
        .filter(|group| group.get("name").and_then(Value::as_str) == Some(GROUP_NAME));
    let Some(group) = matches.next() else {
        return Ok(None);
    };
    if matches.next().is_some() || !private_group(cfg, group) {
        return Err("A Consilium Watchdog group already exists, but its membership or setup could not be verified.".into());
    }
    group
        .get("id")
        .and_then(Value::as_str)
        .and_then(|id| configured_group(cfg, id))
        .map(str::to_owned)
        .map(Some)
        .ok_or_else(|| "The Signal watchdog group ID is invalid.".into())
}

fn receiver_config(cfg: &BridgeConfig, expected: &str, group: &str) -> bool {
    identity(cfg).as_deref() == Some(expected) && configured_group(cfg, group).is_some()
}

#[cfg(windows)]
pub(crate) fn configured(data: &Path) -> bool {
    let Ok(Some(enrollment)) = load::<Enrollment>(&data.join(ENROLLMENT_FILE)) else {
        return false;
    };
    let Some(group) = enrollment.group.filter(|_| enrollment.announced) else {
        return false;
    };
    BridgeConfig::load(&bridge::config_path(None)).is_ok_and(|cfg| {
        receiver_config(&cfg, &enrollment.identity, &group) && signal_chat::addr(&cfg).is_ok()
    })
}

#[cfg(windows)]
pub(crate) fn enroll(data: &Path) -> Result<(), String> {
    let _lock = profile_lock(data, Mode::Desktop)?;
    let cfg = BridgeConfig::load(&bridge::config_path(None))?;
    let expected = identity(&cfg).ok_or(
        "Verify your linked Signal account in Consilium before setting up its watchdog group.",
    )?;
    signal_chat::addr(&cfg)?;
    let path = data.join(ENROLLMENT_FILE);
    let previous: Option<Enrollment> = load(&path)?;
    if previous
        .as_ref()
        .is_some_and(|entry| entry.identity != expected)
    {
        return Err("The watchdog was set up for a different verified Signal account.".into());
    }
    let groups = signal_chat::call(&cfg, "listGroups", json!({}))?;
    let id = match (previous.as_ref(), find_group(&cfg, &groups)?) {
        (Some(old), Some(found)) if old.group.as_ref().is_none_or(|id| *id == found) => found,
        (None, Some(found)) => found,
        (Some(_), _) => return Err("An earlier watchdog setup could not be reconciled. Check for the existing Consilium Watchdog group before retrying.".into()),
        (None, None) => {
            // Record the attempt before talking to Signal. A timeout or crash must never cause
            // a second create call when the first one may have succeeded.
            save(&path, &Enrollment { identity: expected.clone(), group: None, announced: false })?;
            let created = signal_chat::call(&cfg, "updateGroup", json!({
                "name": GROUP_NAME,
                "description": GROUP_DESCRIPTION,
                "link": "disabled",
                "setPermissionAddMember": "only-admins",
                "setPermissionEditDetails": "only-admins",
            }))?;
            created.get("groupId").and_then(Value::as_str).and_then(|id| configured_group(&cfg, id))
                .ok_or("Signal did not return the new watchdog group ID.")?.to_owned()
        }
    };
    let mut entry = Enrollment {
        identity: expected,
        group: Some(id.clone()),
        announced: previous.is_some_and(|old| old.announced),
    };
    save(&path, &entry)?;
    if !entry.announced {
        signal_chat::send_group(&cfg, &id, WELCOME)?;
        entry.announced = true;
        save(&path, &entry)?;
    }
    Ok(())
}

/// Adopts the existing private group using this device's verified Signal link.
/// This never changes group details or creates a group from the relay.
pub(crate) fn enroll_existing_relay(data: &Path) -> Result<(), String> {
    let _lock = profile_lock(data, Mode::PowerRelay)?;
    let cfg = BridgeConfig::load(&bridge::config_path(None))?;
    let identity = identity(&cfg)
        .ok_or("Verify Signal on the relay before enrolling it in the watchdog group.")?;
    signal_chat::addr(&cfg)?;
    let groups = signal_chat::call(&cfg, "listGroups", json!({}))?;
    let group = find_group(&cfg, &groups)?
        .ok_or("The existing private Consilium Watchdog group has not synced to this device.")?;
    let path = data.join(Mode::PowerRelay.enrollment_file());
    let entry = Enrollment {
        identity,
        group: Some(group),
        announced: true,
    };
    if let Some(previous) = load::<Enrollment>(&path)? {
        if previous != entry {
            return Err(
                "The power relay was enrolled for a different Watchdog group or Signal account."
                    .into(),
            );
        }
        return Ok(());
    }
    save(&path, &entry)
}

pub(crate) fn post_commands(data: &Path) -> Result<(), String> {
    let desktop = load::<Enrollment>(&data.join(ENROLLMENT_FILE))?;
    let enrollment = match desktop {
        Some(enrollment) => enrollment,
        None => load::<Enrollment>(&data.join(Mode::PowerRelay.enrollment_file()))?
            .ok_or("Set up the Consilium Watchdog Signal group first.")?,
    };
    let group = enrollment
        .group
        .ok_or("Complete the Consilium Watchdog Signal group setup first.")?;
    let cfg = BridgeConfig::load(&bridge::config_path(None))?;
    if !receiver_config(&cfg, &enrollment.identity, &group) || !group_still_private(&cfg, &group) {
        return Err("Couldn't verify the private Consilium Watchdog group.".into());
    }
    signal_chat::send_group(&cfg, &group, COMMAND_LIST.trim_end())
}

#[cfg(windows)]
pub(crate) fn run(data: &Path, handle: impl FnMut(Action) -> String) -> Result<(), String> {
    run_with_mode(data, Mode::Desktop, handle)
}

pub(crate) fn run_with_mode(
    data: &Path,
    mode: Mode,
    mut handle: impl FnMut(Action) -> String,
) -> Result<(), String> {
    let _lock = profile_lock(data, mode)?;
    let enrollment = load::<Enrollment>(&data.join(mode.enrollment_file()))?
        .ok_or("Set up the Consilium Watchdog Signal group first.")?;
    let group = enrollment
        .group
        .ok_or("Complete the Consilium Watchdog Signal group setup first.")?;
    let config_path = bridge::config_path(None);
    let replay_path = data.join(mode.replay_file());
    let mut replay: Replay = load(&replay_path)?.unwrap_or_default();
    let council_inbox = (mode == Mode::Desktop)
        .then(|| inbox::CouncilInbox::for_watchdog(data, &enrollment.identity));
    loop {
        let Ok(cfg) = BridgeConfig::load(&config_path) else {
            thread::sleep(Duration::from_secs(5));
            continue;
        };
        let Some(endpoint) = signal_chat::addr(&cfg)
            .ok()
            .filter(|_| receiver_config(&cfg, &enrollment.identity, &group))
        else {
            thread::sleep(Duration::from_secs(5));
            continue;
        };
        let current_path = config_path.clone();
        let expected = enrollment.identity.clone();
        let current_group = group.clone();
        let current_endpoint = cfg.endpoint.clone();
        let current_groups = cfg.groups.clone();
        let matches_config = move || {
            BridgeConfig::load(&current_path).map(|now| {
                now.endpoint == current_endpoint
                    && now.groups == current_groups
                    && receiver_config(&now, &expected, &current_group)
            })
        };
        let decode_cfg = cfg.clone();
        let decode_group = group.clone();
        let sub = subscriber::start(
            endpoint,
            matches_config.clone(),
            move |value| received(value, &decode_cfg, &decode_group, mode),
            |_| {},
        )?;
        loop {
            if sub.take_daemon_unresponsive() {
                crate::signal_setup::revive(&config_path);
            }
            let event = match sub.recv_timeout(Duration::from_millis(500)) {
                Ok(event) => event,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            };
            if !matches_config().unwrap_or(false) {
                continue;
            }
            let now = crate::store::now_ms();
            let (action, admission) = match event {
                Received::Council(message) => {
                    if let Some(inbox) = &council_inbox {
                        let _ = inbox.push(&cfg, &message);
                    }
                    continue;
                }
                Received::Command(command) => (Some(command.action), replay.admit(command, now)),
                Received::Unrecognized(timestamp) => {
                    (None, replay.admit_unrecognized(timestamp, now))
                }
            };
            if admission == Admission::Ignored {
                continue;
            }
            // Persist admission before replying or acting, so a restart cannot replay the command.
            save(&replay_path, &replay)?;
            if !group_still_private(&cfg, &group) || !matches_config().unwrap_or(false) {
                continue;
            }
            let is_action_result = admission == Admission::Accepted && action.is_some();
            let reply = match (admission, action) {
                (Admission::CoolingDown, _) => match mode {
                    Mode::Desktop => "Received and verified. Consilium Watchdog already handled a restart request recently. Try again in a minute.".into(),
                    Mode::PowerRelay => "Received and verified. The power relay already handled a request for that computer recently. Try again in a few seconds.".into(),
                },
                (Admission::Accepted, Some(action)) => {
                    let _ = signal_chat::send_group_receipt(&cfg, &group, VERIFIED_RECEIPT);
                    if !matches_config().unwrap_or(false) {
                        continue;
                    }
                    handle(action)
                }
                (Admission::Accepted, None) => UNRECOGNIZED_RECEIPT.into(),
                (Admission::Ignored, _) => continue,
            };
            if is_action_result {
                if matches_config().unwrap_or(false)
                    && group_still_private(&cfg, &group)
                    && matches_config().unwrap_or(false)
                {
                    let _ = signal_chat::send_group(&cfg, &group, &reply);
                }
            } else if matches_config().unwrap_or(false) {
                let _ = signal_chat::send_group_receipt(&cfg, &group, &reply);
            }
        }
    }
}

fn group_still_private(cfg: &BridgeConfig, id: &str) -> bool {
    signal_chat::call(cfg, "listGroups", json!({"groupId": [id]}))
        .ok()
        .and_then(|groups| find_group(cfg, &groups).ok().flatten())
        .as_deref()
        == Some(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PHONE: &str = "+15551234567";
    const ACI: &str = "00000000-0000-4000-8000-000000000001";
    const GROUP: &str = "watchdog-group";

    fn config() -> BridgeConfig {
        BridgeConfig {
            enabled: true,
            account: PHONE.into(),
            allowed_acis: vec![ACI.into()],
            groups: bridge::SignalGroups {
                main: "council-group".into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn receive(group: &str, sender: &str, aci: &str, device: u32, text: &str) -> Value {
        json!({"method": "receive", "params": {"result": {"envelope": {
            "sourceNumber": sender, "sourceUuid": aci, "sourceDevice": device,
            "syncMessage": {"sentMessage": {
                "groupInfo": {"groupId": group}, "timestamp": 1234, "message": text
            }}
        }}}})
    }

    #[test]
    fn accepts_only_exact_commands_from_the_verified_phone_in_the_watchdog_group() {
        let cfg = config();
        for (text, action) in [
            ("restart", Action::RestartApp),
            ("Restart App", Action::RestartApp),
            ("restart Signal", Action::RestartSignal),
            (" status ", Action::Status),
        ] {
            assert_eq!(
                command(&receive(GROUP, PHONE, ACI, 1, text), &cfg, GROUP),
                Some(Command {
                    action,
                    timestamp: 1234
                })
            );
        }
        for message in [
            receive("council-group", PHONE, ACI, 1, "restart"),
            receive(GROUP, PHONE, ACI, 2, "restart"),
            receive(GROUP, PHONE, "different", 1, "restart"),
            receive(GROUP, "+15551230000", ACI, 1, "restart"),
            receive(GROUP, PHONE, ACI, 1, "restart; stop anything"),
        ] {
            assert_eq!(command(&message, &cfg, GROUP), None);
        }
    }

    #[test]
    fn watchdog_commands_never_enter_council_buffer_and_offline_decisions_are_filtered() {
        let cfg = config();
        assert!(matches!(
            received(
                &receive(GROUP, PHONE, ACI, 1, "restart"),
                &cfg,
                GROUP,
                Mode::Desktop
            ),
            Some(Received::Command(Command {
                action: Action::RestartApp,
                ..
            }))
        ));
        assert!(matches!(
            received(
                &receive("council-group", PHONE, ACI, 1, "restart"),
                &cfg,
                GROUP,
                Mode::Desktop,
            ),
            Some(Received::Council(_))
        ));
        for text in ["123456", "Approve", "Denied: old request", "launch vscode"] {
            assert!(received(
                &receive("council-group", PHONE, ACI, 1, text),
                &cfg,
                GROUP,
                Mode::Desktop
            )
            .is_none());
        }
        assert!(matches!(
            received(
                &receive(GROUP, PHONE, ACI, 1, "other message"),
                &cfg,
                GROUP,
                Mode::Desktop,
            ),
            Some(Received::Unrecognized(1234))
        ));
    }

    #[test]
    fn one_listener_receipts_unknown_messages_and_attachments_without_executing_them() {
        let cfg = config();
        let unknown = receive(GROUP, PHONE, ACI, 1, "hello");
        assert!(matches!(
            received(&unknown, &cfg, GROUP, Mode::Desktop),
            Some(Received::Unrecognized(1234))
        ));
        assert!(received(&unknown, &cfg, GROUP, Mode::PowerRelay).is_none());

        let mut attachment = receive(GROUP, PHONE, ACI, 1, "restart");
        let sent = attachment
            .pointer_mut("/params/result/envelope/syncMessage/sentMessage")
            .unwrap();
        sent["attachments"] = json!([{"id": "fixture"}]);
        assert!(matches!(
            received(&attachment, &cfg, GROUP, Mode::Desktop),
            Some(Received::Unrecognized(1234))
        ));
        assert!(received(&attachment, &cfg, GROUP, Mode::PowerRelay).is_none());
        assert!(received(
            &receive(GROUP, PHONE, ACI, 2, "hello"),
            &cfg,
            GROUP,
            Mode::Desktop
        )
        .is_none());
        assert!(received(
            &receive(GROUP, PHONE, ACI, 1, ""),
            &cfg,
            GROUP,
            Mode::Desktop
        )
        .is_none());

        let mut replay = Replay::default();
        assert_eq!(replay.admit_unrecognized(1234, 1234), Admission::Accepted);
        assert_eq!(replay.admit_unrecognized(1234, 1235), Admission::Ignored);
        assert_eq!(
            replay.admit_unrecognized(1235, 1236 + MAX_COMMAND_AGE_MS),
            Admission::Ignored
        );
    }

    #[test]
    fn separates_desktop_and_power_commands_before_replay() {
        let cfg = config();
        for (text, action) in [
            ("wake windows", Action::Wake(PowerTarget::Windows)),
            (" Wake Mac ", Action::Wake(PowerTarget::Mac)),
            ("sleep linux", Action::Sleep(PowerTarget::Linux)),
            ("sleep both", Action::Sleep(PowerTarget::Both)),
            ("power status", Action::PowerStatus),
        ] {
            let event = receive(GROUP, PHONE, ACI, 1, text);
            assert!(received(&event, &cfg, GROUP, Mode::Desktop).is_none());
            assert!(matches!(
                received(&event, &cfg, GROUP, Mode::PowerRelay),
                Some(Received::Command(Command { action: actual, .. })) if actual == action
            ));
        }
        for (group, device, text) in [
            (GROUP, 1, "restart"),
            (GROUP, 1, "status"),
            (GROUP, 2, "wake windows"),
            (GROUP, 1, "sleep unknown"),
            (GROUP, 1, "sleep mac and linux"),
            (GROUP, 1, "wake  windows"),
            ("council-group", 1, "wake windows"),
        ] {
            assert!(received(
                &receive(group, PHONE, ACI, device, text),
                &cfg,
                GROUP,
                Mode::PowerRelay
            )
            .is_none());
        }
        assert_ne!(
            Mode::Desktop.enrollment_file(),
            Mode::PowerRelay.enrollment_file()
        );
        assert_ne!(Mode::Desktop.lock_file(), Mode::PowerRelay.lock_file());
        assert_ne!(Mode::Desktop.replay_file(), Mode::PowerRelay.replay_file());
    }

    #[test]
    fn applies_power_cooldown_per_target_and_reads_older_replay_state() {
        let start = 900_000;
        let mut replay: Replay =
            serde_json::from_str(r#"{"timestamps":[],"app_restart":null,"signal_restart":null}"#)
                .unwrap();
        let mut request = |action, delta| {
            replay.admit(
                Command {
                    action,
                    timestamp: start + delta,
                },
                start + delta,
            )
        };
        assert_eq!(
            request(Action::Wake(PowerTarget::Mac), 0),
            Admission::Accepted
        );
        assert_eq!(
            request(Action::Sleep(PowerTarget::Linux), 1),
            Admission::Accepted
        );
        assert_eq!(
            request(Action::Sleep(PowerTarget::Both), 2),
            Admission::CoolingDown
        );
        assert_eq!(
            request(Action::Sleep(PowerTarget::Mac), 3),
            Admission::CoolingDown
        );
        assert_eq!(
            request(Action::Sleep(PowerTarget::Both), POWER_COOLDOWN_MS + 1),
            Admission::Accepted
        );
        assert_eq!(
            request(Action::Wake(PowerTarget::Windows), POWER_COOLDOWN_MS + 2),
            Admission::CoolingDown
        );

        assert_eq!(
            replay.admit(
                Command {
                    action: Action::Sleep(PowerTarget::Mac),
                    timestamp: start + 4
                },
                start + 5 + MAX_SLEEP_COMMAND_AGE_MS
            ),
            Admission::Ignored
        );
    }

    #[test]
    fn catches_hourly_wake_requests_without_replaying_them_or_delayed_sleep() {
        let sent = 500_000;
        let first_check = sent + 60 * 60_000;
        let second_check = first_check + MAX_COMMAND_AGE_MS + 1;
        let wake = Command {
            action: Action::Wake(PowerTarget::Mac),
            timestamp: sent,
        };
        let sleep = Command {
            action: Action::Sleep(PowerTarget::Mac),
            timestamp: sent + 1,
        };
        let status = Command {
            action: Action::PowerStatus,
            timestamp: sent + 2,
        };
        let mut replay = Replay::default();
        assert_eq!(replay.admit(wake, first_check), Admission::Accepted);
        assert_eq!(replay.admit(sleep, first_check), Admission::Ignored);
        assert_eq!(replay.admit(status, first_check), Admission::Ignored);
        let mut replay: Replay =
            serde_json::from_slice(&serde_json::to_vec(&replay).unwrap()).unwrap();
        assert_eq!(replay.admit(wake, second_check), Admission::Ignored);

        let mut clean_replay = Replay::default();
        assert_eq!(
            clean_replay.admit(wake, sent + MAX_WAKE_COMMAND_AGE_MS + 1),
            Admission::Ignored
        );
    }

    #[test]
    fn blocks_replays_and_rate_limits_each_restart_across_processes() {
        let start = 900_000;
        let app = Command {
            timestamp: start,
            action: Action::RestartApp,
        };
        let mut replay = Replay::default();
        assert_eq!(replay.admit(app, start), Admission::Accepted);
        let mut replay: Replay =
            serde_json::from_slice(&serde_json::to_vec(&replay).unwrap()).unwrap();
        assert_eq!(replay.admit(app, start + 1), Admission::Ignored);
        assert_eq!(
            replay.admit(
                Command {
                    timestamp: start + 1,
                    ..app
                },
                start + 1
            ),
            Admission::CoolingDown
        );
        assert_eq!(
            replay.admit(
                Command {
                    timestamp: start + 2,
                    action: Action::RestartSignal
                },
                start + 2
            ),
            Admission::Accepted
        );
        assert_eq!(
            replay.admit(
                Command {
                    timestamp: start + MAX_FUTURE_MS + 3,
                    ..app
                },
                start + 2
            ),
            Admission::Ignored
        );
        assert_eq!(
            replay.admit(app, start + MAX_COMMAND_AGE_MS + 1),
            Admission::Ignored
        );
    }

    #[test]
    fn reconciles_only_one_self_only_watchdog_group() {
        let cfg = config();
        let mut group = json!({"name": GROUP_NAME, "description": GROUP_DESCRIPTION,
            "isMember": true, "isBlocked": false, "id": GROUP,
            "members": [{"uuid": ACI}], "pendingMembers": [], "requestingMembers": []});
        assert_eq!(
            find_group(&cfg, &json!([group.clone()]))
                .unwrap()
                .as_deref(),
            Some(GROUP)
        );
        assert!(find_group(&cfg, &json!([group.clone(), group.clone()])).is_err());
        group["members"] = json!([{"uuid": ACI}, {"uuid":"second"}]);
        assert!(find_group(&cfg, &json!([group])).is_err());
    }
}
