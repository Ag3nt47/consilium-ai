//! Shared memory for the whole council.
//!
//! - Council memory (`memory.md` in the app's data folder): notes every AI receives in every
//!   conversation. AIs add to it with `REMEMBER:` lines; the person can edit or clear it in Settings.
//! - Project notes (`CONSILIUM.md` in a conversation's project folder): shared instructions for
//!   that project (conventions, how to build and test, playbooks), given to every AI whatever its
//!   access level, and kept with the project in git.
//!
//! Both are capped in size so they can't crowd out the conversation.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Characters of council memory and of project notes given to each AI.
pub const MAX_CHARS: usize = 8_000;
pub const PROJECT_NOTES: &str = "CONSILIUM.md";

pub struct Memory {
    path: PathBuf,
    lock: Mutex<()>,
}

impl Memory {
    pub fn new(path: PathBuf) -> Self {
        Memory { path, lock: Mutex::new(()) }
    }

    pub fn read(&self) -> String {
        std::fs::read_to_string(&self.path).unwrap_or_default()
    }

    pub fn write(&self, text: &str) -> Result<(), String> {
        let _g = self.lock.lock().unwrap();
        crate::store::write_atomic(&self.path, text.as_bytes()).map_err(|e| e.to_string())
    }

    /// Adds facts an AI asked to remember, one bullet each.
    /// Supports `[replaces: <topic>]` tags to replace outdated notes matching `<topic>`.
    /// Preserves verification status tags like `[verified]`. Skips exact duplicates.
    pub fn remember(&self, who: &str, date: &str, facts: &[String]) -> Result<Vec<String>, String> {
        let _g = self.lock.lock().unwrap();
        let mut text = std::fs::read_to_string(&self.path).unwrap_or_default();
        let mut added = Vec::new();
        for f in facts {
            let f = &crate::redact::scrub(f);
            let (needle_opt, clean_fact) = extract_replaces(f);
            if let Some(needle) = needle_opt {
                let needle_lower = needle.to_lowercase();
                let remaining_lines: Vec<&str> = text
                    .lines()
                    .filter(|line| {
                        let line_lower = line.to_lowercase();
                        !(line.trim_start().starts_with('-') && line_lower.contains(&needle_lower))
                    })
                    .collect();
                text = remaining_lines.join("\n");
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&format!("- {clean_fact} ({who}, {date})\n"));
                added.push(clean_fact);
            } else {
                if text.contains(clean_fact.as_str()) {
                    continue;
                }
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push_str(&format!("- {clean_fact} ({who}, {date})\n"));
                added.push(clean_fact);
            }
        }
        if !added.is_empty() {
            // Written to a temporary file and swapped in, so a crash can't leave half a memory file.
            let tmp = self.path.with_extension("md.tmp");
            std::fs::write(&tmp, &text).map_err(|e| e.to_string())?;
            crate::store::replace_file(&tmp, &self.path).map_err(|e| e.to_string())?;
        }
        Ok(added)
    }

    /// The memory as given to the AIs: the most recent part if it is too long.
    pub fn for_prompt(&self) -> Option<String> {
        clip(&without_repeats(&self.read()))
    }
}

/// The project's CONSILIUM.md, if the folder has one.
pub fn project_notes(folder: &Path) -> Option<String> {
    clip(&std::fs::read_to_string(folder.join(PROJECT_NOTES)).ok()?)
}

/// Keeps the last MAX_CHARS characters (newest notes are at the end), trimmed to whole lines.
fn clip(text: &str) -> Option<String> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    if t.chars().count() <= MAX_CHARS {
        return Some(t.to_string());
    }
    let skip = t.chars().count() - MAX_CHARS;
    let tail: String = t.chars().skip(skip).collect();
    let tail = tail.split_once('\n').map(|(_, rest)| rest.to_string()).unwrap_or(tail);
    Some(format!("(earlier notes omitted)\n{tail}"))
}

/// Keeps the newest copy of each repeated note in the prompt. Match the exact
/// note text apart from the recorded author and date. Similar notes can carry
/// different decisions, so keep both unless the text matches.
fn without_repeats(text: &str) -> String {
    let mut seen = std::collections::HashSet::new();
    let mut lines = Vec::new();
    for line in text.lines().rev() {
        if let Some(note) = line.trim_start().strip_prefix("- ") {
            if !seen.insert(without_stamp(note.trim_end())) {
                continue;
            }
        }
        lines.push(line);
    }
    lines.reverse();
    lines.join("\n")
}

fn without_stamp(note: &str) -> &str {
    let Some((body, stamp)) = note.strip_suffix(')').and_then(|s| s.rsplit_once(" (")) else {
        return note;
    };
    let Some((author, date)) = stamp.rsplit_once(", ") else {
        return note;
    };
    let is_date = date.len() == 10 && date.bytes().enumerate().all(|(i, b)| {
        if i == 4 || i == 7 { b == b'-' } else { b.is_ascii_digit() }
    });
    if !author.is_empty() && is_date { body } else { note }
}

/// Extracts an optional replacement target tag like `[replaces: needle]` or `[replace: needle]`
/// from a memory fact line. Returns `(Some(needle), cleaned_fact)`.
pub fn extract_replaces(fact: &str) -> (Option<String>, String) {
    let trimmed = fact.trim();
    let lower = trimmed.to_lowercase();
    for prefix in &["[replaces:", "[replace:"] {
        if lower.starts_with(prefix) {
            if let Some(end) = trimmed.find(']') {
                let needle = trimmed[prefix.len()..end].trim().trim_matches('"').trim_matches('\'').trim();
                let rest = trimmed[end + 1..].trim();
                if !needle.is_empty() && !rest.is_empty() {
                    return (Some(needle.to_string()), rest.to_string());
                }
            }
        }
    }
    (None, trimmed.to_string())
}

/// Checks if a fact note has been tagged as verified.
#[allow(dead_code)]
pub fn is_verified(fact: &str) -> bool {
    let lower = fact.to_lowercase();
    lower.contains("[verified]") || lower.contains("verified:") || lower.contains("(verified)")
}

/// Facts on `REMEMBER:` lines. Lines that look like they hold a secret are dropped.
pub fn remember_lines(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| {
            let t = line.trim_start();
            if t.len() < 9 || !t[..9].eq_ignore_ascii_case("REMEMBER:") {
                return None;
            }
            let fact = t[9..].trim();
            (!fact.is_empty() && !looks_secret(fact)).then(|| fact.chars().take(300).collect())
        })
        .collect()
}

fn looks_secret(s: &str) -> bool {
    let lower = s.to_lowercase();
    ["password", "api key", "apikey", "secret", "token", "sk-", "ghp_", "-----begin"].iter().any(|w| lower.contains(w))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_notes_are_left_out_of_the_prompt_newest_kept() {
        let text = concat!(
            "## Council memory\n",
            "- Keep restart disabled on this host (Claude, 2026-11-14)\n",
            "- Build installers on the Mac (Gemini, 2026-11-15)\n",
            "- Keep restart enabled on this host (Gemini, 2026-11-18)\n",
            "- Build installers on the Mac (Claude, 2026-11-19)\n",
            "- Use local mode (old, separate)\n",
            "- Use local mode (new, separate)\n",
        );
        let out = without_repeats(text);
        assert!(out.starts_with("## Council memory\n"));
        assert_eq!(out.matches("Build installers").count(), 1);
        assert!(out.contains("Build installers on the Mac (Claude, 2026-11-19)"));
        assert!(out.contains("Keep restart disabled") && out.contains("Keep restart enabled"));
        assert_eq!(out.matches("Use local mode").count(), 2);
    }

    #[test]
    fn remember_lines_are_parsed_and_secrets_refused() {
        let r = remember_lines("ok\nREMEMBER: The human prefers short answers\nremember: Builds use npm run app:build\nREMEMBER: my API key is abc");
        assert_eq!(r, vec!["The human prefers short answers", "Builds use npm run app:build"]);
    }

    #[test]
    fn memory_appends_once_and_clips_from_the_oldest_end() {
        let p = std::env::temp_dir().join(format!("consilium-memory-{}.md", uuid::Uuid::new_v4()));
        let m = Memory::new(p.clone());
        assert!(m.for_prompt().is_none());
        let added = m.remember("Gemini", "2026-11-14", &["Use Tauri 2".into()]).unwrap();
        assert_eq!(added.len(), 1);
        assert!(m.remember("Claude", "2026-11-14", &["Use Tauri 2".into()]).unwrap().is_empty());
        assert_eq!(m.read(), "- Use Tauri 2 (Gemini, 2026-11-14)\n");
        m.write(&format!("{}\nnewest line", "old line\n".repeat(2000))).unwrap();
        let clipped = m.for_prompt().unwrap();
        assert!(clipped.starts_with("(earlier notes omitted)") && clipped.ends_with("newest line"));
        assert!(clipped.chars().count() <= MAX_CHARS + 30);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn project_notes_come_from_consilium_md() {
        let dir = std::env::temp_dir().join(format!("consilium-proj-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(project_notes(&dir).is_none());
        std::fs::write(dir.join(PROJECT_NOTES), "# Rules\nRun cargo test before pushing.").unwrap();
        assert!(project_notes(&dir).unwrap().contains("cargo test"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn test_verified_and_replaceable_memory_notes() {
        let p = std::env::temp_dir().join(format!("consilium-rep-memory-{}.md", uuid::Uuid::new_v4()));
        let m = Memory::new(p.clone());

        let fact1 = "AnonRouter might be at /tmp/anonrouter".to_string();
        let fact2 = "Zero Breach is v3.0.2".to_string();
        let added = m.remember("Gemini", "2026-11-16", &[fact1, fact2]).unwrap();
        assert_eq!(added.len(), 2);
        assert!(m.read().contains("AnonRouter might be at /tmp/anonrouter"));
        assert!(m.read().contains("Zero Breach is v3.0.2"));

        // Replace AnonRouter note with verified location
        let rep_fact = "[replaces: AnonRouter] [verified] AnonRouter is at ~/.local/opt/python-3.11".to_string();
        assert!(is_verified(&rep_fact));
        let (needle, clean) = extract_replaces(&rep_fact);
        assert_eq!(needle.as_deref(), Some("AnonRouter"));
        assert_eq!(clean, "[verified] AnonRouter is at ~/.local/opt/python-3.11");

        let added2 = m.remember("Claude", "2026-11-17", &[rep_fact]).unwrap();
        assert_eq!(added2.len(), 1);

        let read_back = m.read();
        assert!(!read_back.contains("/tmp/anonrouter"));
        assert!(read_back.contains("- [verified] AnonRouter is at ~/.local/opt/python-3.11 (Claude, 2026-11-17)\n"));
        assert!(read_back.contains("Zero Breach is v3.0.2"));

        let _ = std::fs::remove_file(p);
    }
}
