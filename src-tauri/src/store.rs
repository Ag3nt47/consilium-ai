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

/// Puts the finished temporary file `tmp` in place of `path`. Some Windows setups (an app container
/// or a virtualised AppData folder) refuse to rename over an existing file; then the contents are
/// copied over instead, so the save still lands.
pub fn replace_file(tmp: &Path, path: &Path) -> std::io::Result<()> {
    if fs::rename(tmp, path).is_ok() {
        return Ok(());
    }
    fs::copy(tmp, path)?;
    let _ = fs::remove_file(tmp);
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

    fn stop_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.stop"))
    }

    /// Takes the right to run this conversation's AIs, unless a process (this one or another)
    /// already has it.
    pub fn try_turn_lock(&self, id: &str) -> Option<TurnLock> {
        Self::check(id).ok()?;
        let file = OpenOptions::new().create(true).truncate(false).write(true).open(self.turn_lock_path(id)).ok()?;
        file.try_lock().ok()?;
        Some(TurnLock { _file: file })
    }

    /// Whether some process is running this conversation's AIs. (A process that holds the lock
    /// itself knows that already: this probe reports its own lock as held too.)
    pub fn turn_locked(&self, id: &str) -> bool {
        // No lock file yet means no turn has ever run (and the probe doesn't create one).
        valid_id(id) && self.turn_lock_path(id).exists() && self.try_turn_lock(id).is_none()
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

    pub fn list(&self) -> Vec<ConversationMeta> {
        let mut out: Vec<ConversationMeta> = fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .filter_map(|e| fs::read_to_string(e.path()).ok())
            .filter_map(|s| serde_json::from_str::<ConversationMeta>(&s).ok())
            .filter(|m| valid_id(&m.id))
            .collect();
        out.sort_by_key(|m| std::cmp::Reverse(m.updated));
        out
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
        let tmp = self.dir.join(format!("{}.json.tmp", meta.id));
        fs::write(&tmp, serde_json::to_vec_pretty(meta).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        replace_file(&tmp, &self.meta_path(&meta.id)).map_err(|e| e.to_string())
    }

    pub fn meta(&self, id: &str) -> Result<ConversationMeta, String> {
        Self::check(id)?;
        let s = fs::read_to_string(self.meta_path(id)).map_err(|_| "no such conversation".to_string())?;
        serde_json::from_str(&s).map_err(|e| e.to_string())
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
        Ok(())
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
        self.append_full(id, "human", "message", to, text, None, attachments, vec![], via.map(String::from), false)
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
        self.append_full(id, sender, kind, to, text, usage, vec![], vec![], None, false)
    }

    /// A message copied from a linked computer. Its REQUEST lines get no buttons here: the
    /// computer it came from owns that turn.
    pub fn append_imported(
        &self,
        id: &str,
        sender: &str,
        kind: &str,
        to: Vec<String>,
        text: &str,
        via: Option<String>,
    ) -> Result<Event, String> {
        self.append_full(id, sender, kind, to, text, None, vec![], vec![], via, true)
    }

    /// Records the messages that were handed to an AI in one compact, hidden receipt event.
    pub fn append_seen(&self, id: &str, sender: &str, seen: Vec<String>) -> Result<Event, String> {
        self.append_full(id, sender, "seen", vec![], "", None, vec![], seen, None, false)
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
