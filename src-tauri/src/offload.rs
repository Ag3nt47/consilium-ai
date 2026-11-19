//! Tool-output offload for local models (offline blueprint, phase 4).
//!
//! A local model served by Ollama or llama.cpp often has a context window of 4k–32k tokens. One
//! command's output, a stack trace or an attached file can fill it alone and push out the
//! conversation it belongs to. Before such a message reaches a local model, its full text is
//! written to `<data>/offload/<hash>.txt` and the prompt carries only its start, its end, the
//! lines that look like errors, and the path, so the model can read more through a command box.
//!
//! Files are named after a hash of their text, so the same message offloads to the same file
//! every turn and the prompt stays identical (and cacheable). Files older than a week are pruned.

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Offloaded files older than this are removed the next time something is offloaded.
const KEEP_FOR: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// At most this many lines that look like errors are kept from the part left out.
const MAX_ERROR_LINES: usize = 8;
/// An error line longer than this is shortened.
const MAX_ERROR_LINE_CHARS: usize = 200;

/// The folder offloaded text lives in.
pub fn dir(data: &Path) -> PathBuf {
    data.join("offload")
}

/// Writes `text` to the offload folder (once: an existing file with the same text is reused)
/// and returns its path. None when the folder can't be written; the caller still clips.
pub fn store(data: &Path, text: &str) -> Option<PathBuf> {
    let folder = dir(data);
    std::fs::create_dir_all(&folder).ok()?;
    let hash = Sha256::digest(text.as_bytes());
    let name: String = hash.iter().take(8).map(|b| format!("{b:02x}")).collect();
    let path = folder.join(format!("{name}.txt"));
    if !path.is_file() {
        prune(&folder);
        // Written beside it, then renamed, so a reader never sees half a file.
        let tmp = folder.join(format!("{name}.tmp"));
        std::fs::write(&tmp, text).ok()?;
        std::fs::rename(&tmp, &path).ok()?;
    }
    Some(path)
}

fn prune(folder: &Path) {
    let Ok(entries) = std::fs::read_dir(folder) else { return };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|age| age > KEEP_FOR);
        if old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Whether a line looks like an error, a failure or a warning worth keeping in view.
fn notable(line: &str) -> bool {
    let l = line.to_ascii_lowercase();
    ["error", "failed", "failure", "panic", "exception", "traceback", "fatal", "denied", "not found", "warning"]
        .iter()
        .any(|w| l.contains(w))
}

fn shorten(line: &str, max: usize) -> String {
    let mut chars = line.chars();
    let shown: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() { format!("{shown}…") } else { shown }
}

/// `text` cut to about `max` characters: its start and end, the notable lines from the middle,
/// and where the full text is (`saved`), or `text` unchanged when it already fits.
pub fn compact(text: &str, max: usize, saved: Option<&Path>) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max {
        return text.to_string();
    }
    let head_len = max * 2 / 5;
    let tail_len = max * 2 / 5;
    let head: String = chars[..head_len].iter().collect();
    let middle: String = chars[head_len..chars.len() - tail_len].iter().collect();
    let tail: String = chars[chars.len() - tail_len..].iter().collect();
    let lines = text.lines().count();
    let errors: Vec<String> = middle
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && notable(l))
        .take(MAX_ERROR_LINES)
        .map(|l| format!("  {}", shorten(l, MAX_ERROR_LINE_CHARS)))
        .collect();
    let left_out = chars.len() - head_len - tail_len;
    let mut note = format!("\n… [{left_out} of {} characters ({lines} lines) left out", chars.len());
    match saved {
        Some(p) => note.push_str(&format!("; the full text is in {} — read the part you need with a command box", p.display())),
        None => note.push_str("; the full text couldn't be saved"),
    }
    note.push(']');
    if !errors.is_empty() {
        note.push_str("\n[lines from the part left out that look like errors:]\n");
        note.push_str(&errors.join("\n"));
    }
    note.push_str(" …\n");
    format!("{head}{note}{tail}")
}

/// Offloads `text` when it's longer than `max` and returns what the prompt should carry.
pub fn offload(data: Option<&Path>, text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let saved = data.and_then(|d| store(d, text));
    compact(text, max, saved.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> PathBuf {
        let d = std::env::temp_dir().join(format!("consilium-offload-test-{}-{}", std::process::id(), rand_suffix()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn rand_suffix() -> u128 {
        SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_nanos()
    }

    #[test]
    fn short_text_is_left_alone_and_not_saved() {
        let d = temp();
        assert_eq!(offload(Some(&d), "hello", 100), "hello");
        assert!(!dir(&d).exists());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn long_text_is_saved_whole_and_clipped_with_its_path_and_errors() {
        let d = temp();
        let mut text = String::from("START\n");
        for i in 0..400 {
            text.push_str(&format!("line {i} compiling crate\n"));
        }
        text.push_str("error[E0308]: mismatched types in agents.rs\n");
        for i in 0..400 {
            text.push_str(&format!("line {i} more output\n"));
        }
        text.push_str("END\n");
        let out = offload(Some(&d), &text, 1_000);
        assert!(out.len() < 2_000, "{}", out.len());
        assert!(out.starts_with("START"));
        assert!(out.trim_end().ends_with("END"));
        assert!(out.contains("error[E0308]: mismatched types"));
        let saved = store(&d, &text).unwrap();
        assert!(out.contains(&saved.display().to_string()));
        assert_eq!(std::fs::read_to_string(&saved).unwrap(), text);
        // The same text maps to the same file, so the prompt is the same every turn.
        assert_eq!(offload(Some(&d), &text, 1_000), out);
        assert_eq!(std::fs::read_dir(dir(&d)).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn clips_even_without_a_data_folder() {
        let text = "é".repeat(5_000);
        let out = offload(None, &text, 500);
        assert!(out.chars().count() < 800);
        assert!(out.contains("couldn't be saved"));
    }
}
