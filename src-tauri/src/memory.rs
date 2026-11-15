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
        Memory {
            path,
            lock: Mutex::new(()),
        }
    }

    pub fn read(&self) -> String {
        std::fs::read_to_string(&self.path).unwrap_or_default()
    }

    pub fn write(&self, text: &str) -> Result<(), String> {
        let _g = self.lock.lock().unwrap();
        let tmp = self.path.with_extension("md.tmp");
        std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &self.path).map_err(|e| e.to_string())
    }

    /// Adds facts an AI asked to remember, one bullet each, skipping exact duplicates.
    pub fn remember(&self, who: &str, date: &str, facts: &[String]) -> Result<Vec<String>, String> {
        let _g = self.lock.lock().unwrap();
        let mut text = std::fs::read_to_string(&self.path).unwrap_or_default();
        let mut added = Vec::new();
        for f in facts {
            if text.contains(f.as_str()) {
                continue;
            }
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str(&format!("- {f} ({who}, {date})\n"));
            added.push(f.clone());
        }
        if !added.is_empty() {
            std::fs::write(&self.path, &text).map_err(|e| e.to_string())?;
        }
        Ok(added)
    }

    /// The memory as given to the AIs: the most recent part if it is too long.
    pub fn for_prompt(&self) -> Option<String> {
        clip(&self.read())
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
    let tail = tail
        .split_once('\n')
        .map(|(_, rest)| rest.to_string())
        .unwrap_or(tail);
    Some(format!("(earlier notes omitted)\n{tail}"))
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
    [
        "password",
        "api key",
        "apikey",
        "secret",
        "token",
        "sk-",
        "ghp_",
        "-----begin",
    ]
    .iter()
    .any(|w| lower.contains(w))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remember_lines_are_parsed_and_secrets_refused() {
        let r = remember_lines("ok\nREMEMBER: The human prefers short answers\nremember: Builds use npm run app:build\nREMEMBER: my API key is abc");
        assert_eq!(
            r,
            vec![
                "The human prefers short answers",
                "Builds use npm run app:build"
            ]
        );
    }

    #[test]
    fn memory_appends_once_and_clips_from_the_oldest_end() {
        let p = std::env::temp_dir().join(format!("consilium-memory-{}.md", uuid::Uuid::new_v4()));
        let m = Memory::new(p.clone());
        assert!(m.for_prompt().is_none());
        let added = m
            .remember("Gemini", "2026-11-14", &["Use Tauri 2".into()])
            .unwrap();
        assert_eq!(added.len(), 1);
        assert!(m
            .remember("Claude", "2026-11-14", &["Use Tauri 2".into()])
            .unwrap()
            .is_empty());
        assert_eq!(m.read(), "- Use Tauri 2 (Gemini, 2026-11-14)\n");
        m.write(&format!("{}\nnewest line", "old line\n".repeat(2000)))
            .unwrap();
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
        std::fs::write(
            dir.join(PROJECT_NOTES),
            "# Rules\nRun cargo test before pushing.",
        )
        .unwrap();
        assert!(project_notes(&dir).unwrap().contains("cargo test"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
