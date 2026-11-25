//! Conversation storage: one append-only event log per conversation.
//!
//! Each conversation is `<id>.jsonl` (one event per line) plus `<id>.json` (title and timestamps)
//! in the app's data folder. Events are only ever appended, so a crash can at worst leave one
//! half-written last line, which `load` skips.
//!
//! The app and the terminal command (`consilium`, see `cli`) can both have the same conversations
//! open. Appends lock the log file, so two processes never write the same sequence number, and a
//! conversation's AIs are run by one process at a time: the one holding its turn lock (see
//! `try_turn_lock`).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// Puts the finished temporary file `tmp` in place of `path`. A rename can fail for a moment on
/// Windows (another process reading the file, an antivirus scan), so it is retried. Some setups
/// refuse it outright (an app container, a virtualised or EFS-encrypted AppData folder, which
/// reports "not the same device"); then the contents are written over `path` in place. `path` is
/// never removed first, so a failed save leaves the old file, not none.
pub fn replace_file(tmp: &Path, path: &Path) -> std::io::Result<()> {
    for attempt in 0..5u64 {
        if fs::rename(tmp, path).is_ok() {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(15 * (attempt + 1)));
    }
    let data = fs::read(tmp)?;
    let mut f = OpenOptions::new().write(true).create(true).truncate(true).open(path)?;
    f.write_all(&data)?;
    f.sync_all()?;
    let _ = fs::remove_file(tmp);
    Ok(())
}

/// Writes `data` to a temporary file next to `path`, flushed to disk, and puts it in place. The
/// temporary name is unique to this write, so the app and the terminal saving the same file at
/// once can't take each other's temporary file away.
pub fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let tmp = path.with_file_name(format!("{name}.{}-{}.tmp", std::process::id(), &uuid::Uuid::new_v4().simple().to_string()[..8]));
    let written = (|| {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(data)?;
        // Flushed before it replaces anything: Consilium can be closed for an update straight
        // after a save, and an unflushed file could come back as zeros.
        f.sync_all()
    })();
    if let Err(e) = written.and_then(|_| replace_file(&tmp, path)) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// Provider-reported token counts for one AI turn. Cache and thinking counts stay separate so the
/// UI can explain the billable total without pretending every provider accounts for them alike.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenUsage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub cache_creation_tokens: u64,
    #[serde(default)]
    pub thinking_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
    /// Provider-measured generation rate for this reply, in thousandths of a token per second.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_rate_milli: Option<u64>,
}

impl TokenUsage {
    fn add(&mut self, other: Self) {
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
        self.cache_read_tokens = self.cache_read_tokens.saturating_add(other.cache_read_tokens);
        self.cache_creation_tokens = self.cache_creation_tokens.saturating_add(other.cache_creation_tokens);
        self.thinking_tokens = self.thinking_tokens.saturating_add(other.thinking_tokens);
        self.total_tokens = self.total_tokens.saturating_add(other.total_tokens);
    }
}

/// Totals for the open conversation and for every conversation since the caller's local-day
/// boundary. Keys are agent ids; providers that did not report token counts are absent.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct TokenUsageReport {
    pub conversation: HashMap<String, TokenUsage>,
    pub today: HashMap<String, TokenUsage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Event {
    pub id: String,
    pub seq: u64,
    /// Milliseconds since the Unix epoch.
    pub ts: u64,
    pub conversation: String,
    /// "human", or an agent id such as "gpt" or "claude".
    pub sender: String,
    /// "message", "system" (status notes), or "error".
    pub kind: String,
    /// Who the message was addressed to (agent ids). Empty means nobody in particular.
    #[serde(default)]
    pub to: Vec<String>,
    pub text: String,
    /// Questions or actions for the human, from `REQUEST:` lines.
    #[serde(default)]
    pub requests: Vec<String>,
    /// AIs this message proposes adding to the council, from `ADD_AI:` lines (agent ids).
    #[serde(default)]
    pub add_ai: Vec<String>,
    /// Message ids handed to this event's sender. Present only when `kind` is `seen`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub seen: Vec<String>,
    /// Token counts reported by the provider for the turn that produced this event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<TokenUsage>,
    /// Files the human attached to this message (see `attach`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<crate::attach::Attachment>,
    /// Transport or bridge the message arrived through (e.g. "Signal"), if not sent directly in-app.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    /// Source node and event identity for a message imported from a linked computer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_event: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConversationMeta {
    pub id: String,
    pub title: String,
    pub created: u64,
    pub updated: u64,
    /// Project folder the AIs may work in (subject to each AI's access level), if any.
    #[serde(default)]
    pub workspace: Option<String>,
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Conversation ids are UUIDs we generate; anything else is refused so an id can never name a path.
pub fn valid_id(id: &str) -> bool {
    id.len() == 36
        && id.chars().enumerate().all(|(i, c)| {
            if [8, 13, 18, 23].contains(&i) {
                c == '-'
            } else {
                c.is_ascii_hexdigit()
            }
        })
}

/// Lines starting with `REQUEST:` (case-insensitive), which ask the human for a decision or action.
pub fn requests_in(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| {
            let t = line.trim_start();
            if t.len() >= 8 && t[..8].eq_ignore_ascii_case("REQUEST:") {
                let rest = t[8..].trim();
                (!rest.is_empty()).then(|| rest.to_string())
            } else {
                None
            }
        })
        .collect()
}

/// Agent ids named on `ADD_AI:` lines (by id or name, case-insensitive), e.g. `ADD_AI: Gemini`.
pub fn add_ai_in(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        let t = line.trim_start();
        if t.len() < 7 || !t[..7].eq_ignore_ascii_case("ADD_AI:") {
            continue;
        }
        for word in t[7..].split(|c: char| c == ',' || c.is_whitespace()) {
            let w = word.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase();
            if let Some(a) = crate::agents::all().into_iter().find(|a| a.id == w || a.name.to_lowercase() == w) {
                if !out.iter().any(|x| x == a.id) {
                    out.push(a.id.to_string());
                }
            }
        }
    }
    out
}

pub struct Store {
    dir: PathBuf,
    /// Serializes this process's appends; the file lock serializes them with other processes.
    appending: Mutex<()>,
}

enum TurnProbe {
    Free(fs::File),
    Held,
    Unknown,
}

/// Held while this process runs a conversation's AIs. The operating system releases it when the
/// file is closed, so a quit or crash can never leave a conversation locked.
pub struct TurnLock {
    _file: fs::File,
}

impl Store {
    pub fn new(dir: PathBuf) -> std::io::Result<Self> {
        fs::create_dir_all(&dir)?;
        Ok(Store { dir, appending: Mutex::new(()) })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Locked while a process appends to the conversation. A file of its own, not the log itself:
    /// Windows locks are mandatory, so a locked log couldn't be read meanwhile.
    fn append_lock_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.append.lock"))
    }

    fn turn_lock_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.turn.lock"))
    }

    /// Where this conversation's attached files are kept.
    pub fn files_dir(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.files"))
    }

    pub fn handoff_path(&self, id: &str) -> Option<PathBuf> {
        valid_id(id).then(|| self.dir.join(format!("{id}.HANDOFF.md")))
    }

    fn stop_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.stop"))
    }

    /// Takes the right to run this conversation's AIs, unless a process (this one or another)
    /// already has it.
    pub fn try_turn_lock(&self, id: &str) -> Option<TurnLock> {
        Self::check(id).ok()?;
        match self.probe_turn_lock(id) {
            TurnProbe::Free(file) => Some(TurnLock { _file: file }),
            TurnProbe::Held | TurnProbe::Unknown => None,
        }
    }

    /// Tries to take the turn lock, telling "another holder" apart from a passing error (such as
    /// running short of file handles), which is retried a few times before giving up.
    fn probe_turn_lock(&self, id: &str) -> TurnProbe {
        let path = self.turn_lock_path(id);
        for attempt in 0..5 {
            if attempt > 0 {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            let Ok(file) = OpenOptions::new().create(true).truncate(false).write(true).open(&path) else {
                continue;
            };
            match file.try_lock() {
                Ok(()) => return TurnProbe::Free(file),
                Err(std::fs::TryLockError::WouldBlock) => return TurnProbe::Held,
                Err(std::fs::TryLockError::Error(_)) => continue,
            }
        }
        TurnProbe::Unknown
    }

    /// Whether some process is running this conversation's AIs. (A process that holds the lock
    /// itself knows that already: this probe reports its own lock as held too.) Only a lock that is
    /// really held counts: an error while checking doesn't make the conversation look busy.
    pub fn turn_locked(&self, id: &str) -> bool {
        // No lock file yet means no turn has ever run (and the probe doesn't create one).
        valid_id(id) && self.turn_lock_path(id).exists() && matches!(self.probe_turn_lock(id), TurnProbe::Held)
    }

    /// Asks whichever process is running this conversation's AIs to stop them (see `take_stop_request`).
    pub fn request_stop(&self, id: &str) -> Result<(), String> {
        Self::check(id)?;
        fs::write(self.stop_path(id), now_ms().to_string()).map_err(|e| e.to_string())
    }

    /// Whether a stop was asked for since `since` (ms). The request is used up either way, so an old
    /// one left behind by a process that has gone can't stop a later turn.
    pub fn take_stop_request(&self, id: &str, since: u64) -> bool {
        let path = self.stop_path(id);
        let Ok(text) = fs::read_to_string(&path) else { return false };
        let _ = fs::remove_file(&path);
        text.trim().parse::<u64>().is_ok_and(|t| t >= since)
    }

    /// Size of the conversation's log, to notice cheaply when another process has added to it.
    pub fn log_len(&self, id: &str) -> u64 {
        fs::metadata(self.log_path(id)).map(|m| m.len()).unwrap_or(0)
    }

    fn log_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.jsonl"))
    }

    fn meta_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    fn check(id: &str) -> Result<(), String> {
        if valid_id(id) {
            Ok(())
        } else {
            Err("invalid conversation id".into())
        }
    }

    /// Every conversation, newest first. One whose title file is missing or damaged is still
    /// listed, rebuilt from its own message log (see `meta`), so it can never silently disappear.
    pub fn list(&self) -> Vec<ConversationMeta> {
        let mut ids: Vec<String> = fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let path = e.path();
                let ext = path.extension()?.to_str()?;
                (ext == "json" || ext == "jsonl").then(|| path.file_stem()?.to_str().map(str::to_string)).flatten()
            })
            .filter(|id| valid_id(id))
            .collect();
        ids.sort();
        ids.dedup();
        let mut out: Vec<ConversationMeta> = ids.iter().filter_map(|id| self.meta(id).ok()).filter(|m| valid_id(&m.id)).collect();
        out.sort_by_key(|m| std::cmp::Reverse(m.updated));
        out
    }

    /// Rebuilds the title file of a conversation whose `<id>.json` is missing or unreadable: from a
    /// temporary copy a save left behind, else from its message log (title from the first thing the
    /// human wrote, times from the first and last events). Saved again, so it is done once. None if
    /// there is no log, so a conversation is only ever recovered, never invented.
    fn recover_meta(&self, id: &str) -> Option<ConversationMeta> {
        let log = self.log_path(id);
        if !log.is_file() {
            return None;
        }
        let events = read_events(&log);
        let left_behind = fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.file_name().to_str().is_some_and(|n| n.starts_with(&format!("{id}.json")) && n.ends_with(".tmp")))
            .filter_map(|e| serde_json::from_slice::<ConversationMeta>(&fs::read(e.path()).ok()?).ok())
            .filter(|m| m.id == id)
            .max_by_key(|m| m.updated);
        let file_time = || {
            fs::metadata(&log).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_millis() as u64)
        };
        let last = events.last().map(|e| e.ts).unwrap_or_else(file_time);
        let meta = match left_behind {
            Some(mut m) => {
                m.updated = m.updated.max(last);
                m
            }
            None => ConversationMeta {
                id: id.to_string(),
                title: events
                    .iter()
                    .find(|e| e.sender == "human" && e.kind == "message")
                    .map(|e| clean_title(&e.text))
                    .unwrap_or_else(|| "Recovered conversation".into()),
                created: events.first().map(|e| e.ts).unwrap_or_else(file_time),
                updated: last,
                workspace: None,
            },
        };
        let _ = self.write_meta(&meta);
        Some(meta)
    }

    pub fn create(&self, title: &str) -> Result<ConversationMeta, String> {
        let t = now_ms();
        let meta = ConversationMeta {
            id: uuid::Uuid::new_v4().to_string(),
            title: clean_title(title),
            created: t,
            updated: t,
            workspace: None,
        };
        self.write_meta(&meta)?;
        fs::File::create(self.log_path(&meta.id)).map_err(|e| e.to_string())?;
        Ok(meta)
    }

    fn write_meta(&self, meta: &ConversationMeta) -> Result<(), String> {
        let data = serde_json::to_vec_pretty(meta).map_err(|e| e.to_string())?;
        write_atomic(&self.meta_path(&meta.id), &data).map_err(|e| e.to_string())
    }

    pub fn meta(&self, id: &str) -> Result<ConversationMeta, String> {
        Self::check(id)?;
        let read = fs::read_to_string(self.meta_path(id)).ok().and_then(|s| serde_json::from_str::<ConversationMeta>(&s).ok());
        match read {
            Some(m) => Ok(m),
            // Missing or damaged (a save cut off by a restart): rebuilt from the conversation's log.
            None => self.recover_meta(id).ok_or_else(|| "no such conversation".to_string()),
        }
    }

    pub fn rename(&self, id: &str, title: &str) -> Result<ConversationMeta, String> {
        let mut meta = self.meta(id)?;
        meta.title = clean_title(title);
        self.write_meta(&meta)?;
        Ok(meta)
    }

    /// Sets or clears the project folder. Only existing folders are accepted.
    pub fn set_workspace(&self, id: &str, folder: Option<&str>) -> Result<ConversationMeta, String> {
        let mut meta = self.meta(id)?;
        meta.workspace = match folder {
            None => None,
            Some(f) => {
                let p = std::fs::canonicalize(f).map_err(|_| format!("{f} doesn't exist"))?;
                if !p.is_dir() {
                    return Err(format!("{f} isn't a folder"));
                }
                Some(p.display().to_string())
            }
        };
        self.write_meta(&meta)?;
        Ok(meta)
    }

    pub fn delete(&self, id: &str) -> Result<(), String> {
        Self::check(id)?;
        let _ = fs::remove_file(self.log_path(id));
        fs::remove_file(self.meta_path(id)).map_err(|e| e.to_string())?;
        let _ = fs::remove_file(self.turn_lock_path(id));
        let _ = fs::remove_file(self.append_lock_path(id));
        let _ = fs::remove_file(self.stop_path(id));
        let _ = fs::remove_dir_all(self.files_dir(id));
        let _ = fs::remove_file(self.dir.join(format!("{id}.tasks.json")));
        if let Some(path) = self.handoff_path(id) {
            let _ = fs::remove_file(path);
        }
        Ok(())
    }

    /// Deletes a group of conversations only when none of them is running. Checking every turn
    /// lock before removing the first file prevents a bulk cleanup from stopping halfway because
    /// one of the conversations is still being answered in the app or terminal.
    pub fn delete_many(&self, ids: &[String]) -> Result<usize, String> {
        for id in ids {
            Self::check(id)?;
        }
        if ids.iter().any(|id| self.turn_locked(id)) {
            return Err("stop every running conversation before deleting them all".into());
        }
        for id in ids {
            self.delete(id)?;
        }
        Ok(ids.len())
    }

    pub fn load(&self, id: &str) -> Result<Vec<Event>, String> {
        Self::check(id)?;
        Ok(read_events(&self.log_path(id)))
    }

    pub fn token_usage_report(&self, id: &str, day_start: u64) -> Result<TokenUsageReport, String> {
        Self::check(id)?;
        let mut report = TokenUsageReport::default();
        add_usage(&mut report.conversation, self.load(id)?.into_iter());
        for conversation in self.list() {
            let events = self
                .load(&conversation.id)?
                .into_iter()
                .filter(|event| event.ts >= day_start);
            add_usage(&mut report.today, events);
        }
        Ok(report)
    }

    pub fn append(
        &self,
        id: &str,
        sender: &str,
        kind: &str,
        to: Vec<String>,
        text: &str,
    ) -> Result<Event, String> {
        self.append_with_usage(id, sender, kind, to, text, None)
    }

    /// A message from the human with attached files (already saved in `files_dir`).
    pub fn append_with_attachments(
        &self,
        id: &str,
        to: Vec<String>,
        text: &str,
        attachments: Vec<crate::attach::Attachment>,
    ) -> Result<Event, String> {
        self.append_with_attachments_via(id, to, text, attachments, None)
    }

    /// A message from the human with attached files and an optional delivery transport (e.g. "Signal").
    pub fn append_with_attachments_via(
        &self,
        id: &str,
        to: Vec<String>,
        text: &str,
        attachments: Vec<crate::attach::Attachment>,
        via: Option<&str>,
    ) -> Result<Event, String> {
        self.append_full(id, "human", "message", to, text, None, attachments, vec![], via.map(String::from), false, None)
    }

    pub fn append_with_usage(
        &self,
        id: &str,
        sender: &str,
        kind: &str,
        to: Vec<String>,
        text: &str,
        usage: Option<TokenUsage>,
    ) -> Result<Event, String> {
        self.append_full(id, sender, kind, to, text, usage, vec![], vec![], None, false, None)
    }

    /// A message copied from a linked computer. Its REQUEST lines get no buttons here: the
    /// computer it came from owns that turn.
    #[allow(clippy::too_many_arguments)]
    pub fn append_imported(
        &self,
        id: &str,
        sender: &str,
        kind: &str,
        to: Vec<String>,
        text: &str,
        via: Option<String>,
        source_event: Option<String>,
    ) -> Result<Event, String> {
        self.append_full(id, sender, kind, to, text, None, vec![], vec![], via, true, source_event)
    }

    /// Records the messages that were handed to an AI in one compact, hidden receipt event.
    pub fn append_seen(&self, id: &str, sender: &str, seen: Vec<String>) -> Result<Event, String> {
        self.append_full(id, sender, "seen", vec![], "", None, vec![], seen, None, false, None)
    }

    #[allow(clippy::too_many_arguments)]
    fn append_full(
        &self,
        id: &str,
        sender: &str,
        kind: &str,
        to: Vec<String>,
        text: &str,
        usage: Option<TokenUsage>,
        attachments: Vec<crate::attach::Attachment>,
        seen: Vec<String>,
        via: Option<String>,
        foreign: bool,
        source_event: Option<String>,
    ) -> Result<Event, String> {
        Self::check(id)?;
        // Everything in a conversation is sent to every AI's provider, so secrets are masked first.
        let text = &crate::redact::scrub(text);
        let appending = self.appending.lock().unwrap();
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.append_lock_path(id))
            .map_err(|e| e.to_string())?;
        lock.lock().map_err(|e| e.to_string())?;
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.log_path(id))
            .map_err(|e| e.to_string())?;
        // Another process may have appended since this one last looked, so the last sequence
        // number is read under the lock, from the file itself.
        let last = read_events(&self.log_path(id)).last().map_or(0, |e| e.seq);
        let event = Event {
            id: uuid::Uuid::new_v4().to_string(),
            seq: last + 1,
            ts: now_ms(),
            conversation: id.to_string(),
            sender: sender.to_string(),
            kind: kind.to_string(),
            to,
            text: text.to_string(),
            // An AI's blocked turn ("error") can carry a REQUEST, e.g. an elevated command awaiting approval.
            // Kept on copies from another computer too: answering one there sends the answer back.
            requests: if kind == "message" || (kind == "error" && sender != "human") { requests_in(text) } else { vec![] },
            add_ai: if kind == "message" && sender != "human" && !foreign { add_ai_in(text) } else { vec![] },
            seen,
            usage,
            attachments,
            via,
            source_event,
        };
        let mut line = serde_json::to_string(&event).map_err(|e| e.to_string())?;
        line.push('\n');
        f.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
        f.sync_data().map_err(|e| e.to_string())?;
        drop(f);
        drop(lock);
        drop(appending);
        if let Ok(mut meta) = self.meta(id) {
            meta.updated = event.ts;
            if meta.title == "New conversation" && sender == "human" {
                meta.title = clean_title(text);
            }
            let _ = self.write_meta(&meta);
        }
        Ok(event)
    }
}

fn add_usage(events: &mut HashMap<String, TokenUsage>, source: impl Iterator<Item = Event>) {
    for event in source {
        if let Some(usage) = event.usage {
            events.entry(event.sender).or_default().add(usage);
        }
    }
}

fn clean_title(text: &str) -> String {
    let first = text.lines().next().unwrap_or("").trim();
    let t: String = first.chars().filter(|c| !c.is_control()).take(60).collect();
    if t.is_empty() {
        "New conversation".into()
    } else {
        t
    }
}

fn read_events(path: &Path) -> Vec<Event> {
    let Ok(data) = fs::read_to_string(path) else { return vec![] };
    // Only newline-terminated lines are complete; a trailing partial line is ignored.
    let complete = match data.rfind('\n') {
        Some(i) => &data[..=i],
        None => "",
    };
    complete
        .lines()
        .filter_map(|l| serde_json::from_str::<Event>(l).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replace_file_overwrites_and_removes_the_temporary_file() {
        let dir = std::env::temp_dir().join(format!("consilium-replace-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let (tmp, path) = (dir.join("a.json.tmp"), dir.join("a.json"));
        fs::write(&path, "old").unwrap();
        fs::write(&tmp, "new").unwrap();
        replace_file(&tmp, &path).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "new");
        assert!(!tmp.exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_conversation_with_a_missing_or_zeroed_title_file_is_recovered_not_lost() {
        let (s, dir) = temp_store();
        let m = s.create("Plans").unwrap();
        s.append(&m.id, "human", "message", vec![], "Please install the tools").unwrap();
        s.append(&m.id, "claude", "message", vec![], "on it").unwrap();
        // A save cut off by a restart: the title file is all zeros.
        fs::write(s.meta_path(&m.id), vec![0u8; 202]).unwrap();
        let listed = s.list();
        assert_eq!(listed.len(), 1, "still listed");
        assert_eq!(listed[0].id, m.id);
        assert_eq!(listed[0].title, "Please install the tools");
        assert!(serde_json::from_str::<ConversationMeta>(&fs::read_to_string(s.meta_path(&m.id)).unwrap()).is_ok(), "rewritten once");
        // Missing, with the copy a failed save left behind: that copy wins.
        let mut renamed = s.meta(&m.id).unwrap();
        renamed.title = "Named by the human".into();
        fs::write(dir.join(format!("{}.json.tmp", m.id)), serde_json::to_vec(&renamed).unwrap()).unwrap();
        fs::remove_file(s.meta_path(&m.id)).unwrap();
        assert_eq!(s.meta(&m.id).unwrap().title, "Named by the human");
        // A title file with no log is not invented into a conversation.
        assert!(s.meta("0b1c2d3e-0000-4000-8000-000000000000").is_err());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn saves_from_two_processes_at_once_never_lose_the_file() {
        let (s, dir) = temp_store();
        let m = s.create("Plans").unwrap();
        let s = std::sync::Arc::new(s);
        let threads: Vec<_> = (0..8)
            .map(|i| {
                let (s, id) = (s.clone(), m.id.clone());
                std::thread::spawn(move || {
                    for j in 0..20 {
                        let _ = s.rename(&id, &format!("title {i}-{j}"));
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert!(s.meta(&m.id).unwrap().title.starts_with("title "));
        let leftovers = fs::read_dir(&dir).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().ends_with(".tmp")).count();
        assert_eq!(leftovers, 0, "no temporary files left behind");
        let _ = fs::remove_dir_all(dir);
    }

    fn temp_store() -> (Store, PathBuf) {
        let dir = std::env::temp_dir().join(format!("consilium-test-{}", uuid::Uuid::new_v4()));
        (Store::new(dir.clone()).unwrap(), dir)
    }

    #[test]
    fn a_blocked_ai_turn_keeps_its_request_so_it_can_be_approved() {
        let (store, dir) = temp_store();
        let c = store.create("x").unwrap();
        let blocked = store
            .append(&c.id, "gpt", "error", vec![], "GPT couldn't answer: approval required.\nREQUEST: Run it? ELEVATED_COMMAND_JSON: \"sudo true\"")
            .unwrap();
        assert_eq!(blocked.requests, vec![r#"Run it? ELEVATED_COMMAND_JSON: "sudo true""#.to_string()]);
        let mine = store.append(&c.id, "human", "error", vec![], "REQUEST: not an AI").unwrap();
        assert!(mine.requests.is_empty());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn two_processes_appending_never_share_a_sequence_number() {
        // Two stores on one folder stand for the app and the terminal command.
        let (a, dir) = temp_store();
        let b = Store::new(dir.clone()).unwrap();
        let c = a.create("x").unwrap();
        let (a, b) = (std::sync::Arc::new(a), std::sync::Arc::new(b));
        let threads: Vec<_> = [a.clone(), b.clone()]
            .into_iter()
            .map(|s| {
                let id = c.id.clone();
                std::thread::spawn(move || {
                    for i in 0..25 {
                        s.append(&id, "human", "message", vec![], &format!("m{i}")).unwrap();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let seqs: Vec<u64> = a.load(&c.id).unwrap().iter().map(|e| e.seq).collect();
        assert_eq!(seqs, (1..=50).collect::<Vec<u64>>());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn one_process_at_a_time_runs_a_conversation() {
        let (a, dir) = temp_store();
        let b = Store::new(dir.clone()).unwrap();
        let c = a.create("x").unwrap();
        let lock = a.try_turn_lock(&c.id).expect("free at first");
        assert!(b.try_turn_lock(&c.id).is_none());
        assert!(b.turn_locked(&c.id));
        drop(lock);
        assert!(!b.turn_locked(&c.id));
        assert!(b.try_turn_lock(&c.id).is_some());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn bulk_delete_is_all_or_nothing_while_a_conversation_is_running() {
        let (store, dir) = temp_store();
        let one = store.create("one").unwrap();
        let two = store.create("two").unwrap();
        store.append(&one.id, "human", "message", vec![], "first").unwrap();
        store.append(&two.id, "human", "message", vec![], "second").unwrap();
        fs::create_dir_all(store.files_dir(&one.id)).unwrap();
        fs::write(store.files_dir(&one.id).join("attachment.txt"), "kept until confirmed").unwrap();
        fs::write(store.dir.join(format!("{}.tasks.json", one.id)), "{}").unwrap();
        fs::write(store.handoff_path(&one.id).unwrap(), "handoff").unwrap();
        let ids = vec![one.id.clone(), two.id.clone()];

        let running = store.try_turn_lock(&two.id).unwrap();
        assert!(store.delete_many(&ids).is_err());
        assert_eq!(store.list().len(), 2, "a busy conversation leaves every conversation intact");
        assert!(store.files_dir(&one.id).is_dir());

        drop(running);
        assert_eq!(store.delete_many(&ids).unwrap(), 2);
        assert!(store.list().is_empty());
        assert!(!store.files_dir(&one.id).exists());
        assert!(!store.dir.join(format!("{}.tasks.json", one.id)).exists());
        assert!(!store.handoff_path(&one.id).unwrap().exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_stop_request_counts_once_and_only_for_a_later_turn() {
        let (s, dir) = temp_store();
        let c = s.create("x").unwrap();
        s.request_stop(&c.id).unwrap();
        assert!(!s.take_stop_request(&c.id, now_ms() + 60_000), "older than the turn: ignored");
        assert!(!s.take_stop_request(&c.id, 0), "and used up");
        s.request_stop(&c.id).unwrap();
        assert!(s.take_stop_request(&c.id, 0));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn secrets_are_masked_before_they_are_saved_or_shared() {
        let (s, dir) = temp_store();
        let c = s.create("Keys").unwrap();
        let key = format!("ghp_{}", "A1b2".repeat(10));
        let e = s
            .append(
                &c.id,
                "gpt",
                "message",
                vec![],
                &format!("Found {key} in .env"),
            )
            .unwrap();
        assert_eq!(e.text, "Found [REDACTED GitHub token] in .env");
        let raw = fs::read_to_string(s.log_path(&c.id)).unwrap();
        assert!(!raw.contains(&key));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn append_and_load_round_trip_with_sequence_numbers() {
        let (s, dir) = temp_store();
        let c = s.create("New conversation").unwrap();
        s.append(&c.id, "human", "message", vec!["gpt".into()], "Plan the app\nREQUEST: no").unwrap();
        let e2 = s.append(&c.id, "gpt", "message", vec![], "Sure.\nREQUEST: pick a name").unwrap();
        let events = s.load(&c.id).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].seq, 1);
        assert_eq!(e2.seq, 2);
        assert_eq!(events[1].requests, vec!["pick a name".to_string()]);
        assert_eq!(s.meta(&c.id).unwrap().title, "Plan the app");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn token_usage_round_trips_and_aggregates_by_agent() {
        let (s, dir) = temp_store();
        let one = s.create("one").unwrap();
        let two = s.create("two").unwrap();
        let usage = TokenUsage {
            input_tokens: 10,
            output_tokens: 5,
            cache_read_tokens: 3,
            total_tokens: 15,
            output_rate_milli: Some(48_200),
            ..TokenUsage::default()
        };
        s.append_with_usage(&one.id, "gpt", "message", vec![], "a", Some(usage))
            .unwrap();
        s.append_with_usage(&one.id, "gpt", "message", vec![], "b", Some(usage))
            .unwrap();
        s.append_with_usage(&two.id, "claude", "message", vec![], "c", Some(usage))
            .unwrap();

        let report = s.token_usage_report(&one.id, 0).unwrap();
        assert_eq!(report.conversation["gpt"].total_tokens, 30);
        assert_eq!(report.conversation["gpt"].output_rate_milli, None);
        assert_eq!(report.conversation["gpt"].cache_read_tokens, 6);
        assert_eq!(report.today["gpt"].total_tokens, 30);
        assert_eq!(report.today["claude"].total_tokens, 15);
        assert_eq!(s.load(&one.id).unwrap()[0].usage, Some(usage));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn old_events_without_usage_still_load() {
        let line = r#"{"id":"x","seq":1,"ts":0,"conversation":"c","sender":"human","kind":"message","to":[],"text":"hi","requests":[],"add_ai":[]}"#;
        let event: Event = serde_json::from_str(line).unwrap();
        assert_eq!(event.usage, None);
    }

    #[test]
    fn partial_last_line_is_ignored() {
        let (s, dir) = temp_store();
        let c = s.create("x").unwrap();
        s.append(&c.id, "human", "message", vec![], "hello").unwrap();
        let mut f = OpenOptions::new().append(true).open(s.log_path(&c.id)).unwrap();
        f.write_all(b"{\"id\":\"half-writ").unwrap();
        assert_eq!(s.load(&c.id).unwrap().len(), 1);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn ids_that_could_name_paths_are_refused() {
        let (s, dir) = temp_store();
        assert!(s.load("../../etc/passwd").is_err());
        assert!(s.append("..", "human", "message", vec![], "x").is_err());
        assert!(valid_id("123e4567-e89b-12d3-a456-426614174000"));
        assert!(!valid_id("123e4567-e89b-12d3-a456-42661417400/"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn add_ai_lines_are_parsed_by_id_or_name() {
        assert_eq!(add_ai_in("Let's bring in help.\nADD_AI: Gemini\nadd_ai: claude, nobody"), vec!["gemini", "claude"]);
        assert!(add_ai_in("we could ADD_AI: gemini later").is_empty(), "only at the start of a line");
    }

    #[test]
    fn request_lines_are_parsed() {
        assert_eq!(
            requests_in("a\n  request: one\nREQUEST:\nREQUEST: two "),
            vec!["one".to_string(), "two".to_string()]
        );
    }
}
