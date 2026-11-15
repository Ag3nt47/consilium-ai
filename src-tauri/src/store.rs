//! Conversation storage: one append-only event log per conversation.
//!
//! Each conversation is `<id>.jsonl` (one event per line) plus `<id>.json` (title and timestamps)
//! in the app's data folder. Events are only ever appended, so a crash can at worst leave one
//! half-written last line, which `load` skips.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

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
        self.cache_read_tokens = self
            .cache_read_tokens
            .saturating_add(other.cache_read_tokens);
        self.cache_creation_tokens = self
            .cache_creation_tokens
            .saturating_add(other.cache_creation_tokens);
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
    /// Token counts reported by the provider for the turn that produced this event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<TokenUsage>,
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
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
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
            let w = word
                .trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase();
            if let Some(a) = crate::agents::AGENTS
                .iter()
                .find(|a| a.id == w || a.name.to_lowercase() == w)
            {
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
    /// Serializes appends and remembers each conversation's last sequence number.
    seqs: Mutex<HashMap<String, u64>>,
}

impl Store {
    pub fn new(dir: PathBuf) -> std::io::Result<Self> {
        fs::create_dir_all(&dir)?;
        Ok(Store {
            dir,
            seqs: Mutex::new(HashMap::new()),
        })
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
        fs::write(
            &tmp,
            serde_json::to_vec_pretty(meta).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        fs::rename(&tmp, self.meta_path(&meta.id)).map_err(|e| e.to_string())
    }

    pub fn meta(&self, id: &str) -> Result<ConversationMeta, String> {
        Self::check(id)?;
        let s = fs::read_to_string(self.meta_path(id))
            .map_err(|_| "no such conversation".to_string())?;
        serde_json::from_str(&s).map_err(|e| e.to_string())
    }

    pub fn rename(&self, id: &str, title: &str) -> Result<ConversationMeta, String> {
        let mut meta = self.meta(id)?;
        meta.title = clean_title(title);
        self.write_meta(&meta)?;
        Ok(meta)
    }

    /// Sets or clears the project folder. Only existing folders are accepted.
    pub fn set_workspace(
        &self,
        id: &str,
        folder: Option<&str>,
    ) -> Result<ConversationMeta, String> {
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
        self.seqs.lock().unwrap().remove(id);
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

    pub fn append_with_usage(
        &self,
        id: &str,
        sender: &str,
        kind: &str,
        to: Vec<String>,
        text: &str,
        usage: Option<TokenUsage>,
    ) -> Result<Event, String> {
        Self::check(id)?;
        let mut seqs = self.seqs.lock().unwrap();
        let last = match seqs.get(id) {
            Some(n) => *n,
            None => read_events(&self.log_path(id)).last().map_or(0, |e| e.seq),
        };
        let event = Event {
            id: uuid::Uuid::new_v4().to_string(),
            seq: last + 1,
            ts: now_ms(),
            conversation: id.to_string(),
            sender: sender.to_string(),
            kind: kind.to_string(),
            to,
            text: text.to_string(),
            requests: if kind == "message" {
                requests_in(text)
            } else {
                vec![]
            },
            add_ai: if kind == "message" && sender != "human" {
                add_ai_in(text)
            } else {
                vec![]
            },
            usage,
        };
        let mut line = serde_json::to_string(&event).map_err(|e| e.to_string())?;
        line.push('\n');
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.log_path(id))
            .map_err(|e| e.to_string())?;
        f.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
        f.sync_data().map_err(|e| e.to_string())?;
        seqs.insert(id.to_string(), event.seq);
        drop(seqs);
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
    let Ok(data) = fs::read_to_string(path) else {
        return vec![];
    };
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

    fn temp_store() -> (Store, PathBuf) {
        let dir = std::env::temp_dir().join(format!("consilium-test-{}", uuid::Uuid::new_v4()));
        (Store::new(dir.clone()).unwrap(), dir)
    }

    #[test]
    fn append_and_load_round_trip_with_sequence_numbers() {
        let (s, dir) = temp_store();
        let c = s.create("New conversation").unwrap();
        s.append(
            &c.id,
            "human",
            "message",
            vec!["gpt".into()],
            "Plan the app\nREQUEST: no",
        )
        .unwrap();
        let e2 = s
            .append(
                &c.id,
                "gpt",
                "message",
                vec![],
                "Sure.\nREQUEST: pick a name",
            )
            .unwrap();
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
        s.append(&c.id, "human", "message", vec![], "hello")
            .unwrap();
        let mut f = OpenOptions::new()
            .append(true)
            .open(s.log_path(&c.id))
            .unwrap();
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
        assert_eq!(
            add_ai_in("Let's bring in help.\nADD_AI: Gemini\nadd_ai: claude, nobody"),
            vec!["gemini", "claude"]
        );
        assert!(
            add_ai_in("we could ADD_AI: gemini later").is_empty(),
            "only at the start of a line"
        );
    }

    #[test]
    fn request_lines_are_parsed() {
        assert_eq!(
            requests_in("a\n  request: one\nREQUEST:\nREQUEST: two "),
            vec!["one".to_string(), "two".to_string()]
        );
    }
}
