//! The skill library: markdown runbooks the flagship AIs (Claude, ChatGPT, Gemini) write for the
//! local models (Qwen, Llama, Phi through Ollama), which have small context windows and no tools.
//! Only a one-line index goes into a local AI's prompt; it pulls a whole runbook with a command box
//! when a task needs it (progressive disclosure), so the library can grow without filling its
//! context.
//!
//! Where it lives: `CONSILIUM_SKILLS_DIR` if set; on Windows `F:\Consilium\skills` when an F: drive
//! exists (a big data drive, apart from the system drive); otherwise `skills` in Consilium's data
//! folder.

use std::path::{Path, PathBuf};

/// The most runbooks listed in a prompt; the rest are still on disk for `ls`.
const MAX_LISTED: usize = 60;
const SUMMARY_CHARS: usize = 140;

#[derive(Debug, Clone, PartialEq)]
pub struct Skill {
    pub name: String,
    pub summary: String,
    pub path: PathBuf,
}

pub fn library_dir(data: &Path) -> PathBuf {
    if let Some(d) = std::env::var_os("CONSILIUM_SKILLS_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(d);
    }
    if cfg!(windows) && Path::new(r"F:\").is_dir() {
        return PathBuf::from(r"F:\Consilium\skills");
    }
    data.join("skills")
}

/// The library folder, created if missing. Falls back to the data folder when it can't be made.
pub fn ensure_library(data: &Path) -> PathBuf {
    let dir = library_dir(data);
    if std::fs::create_dir_all(&dir).is_ok() {
        return dir;
    }
    let fallback = data.join("skills");
    let _ = std::fs::create_dir_all(&fallback);
    fallback
}

/// The runbooks in `dir` (`*.md`, by name), each with its summary: the first line of text that
/// isn't a heading, or the first heading when there is none.
pub fn index(dir: &Path) -> Vec<Skill> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<Skill> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x.eq_ignore_ascii_case("md")))
        .filter_map(|path| {
            let name = path.file_stem()?.to_string_lossy().into_owned();
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            Some(Skill { summary: summary(&text), name, path })
        })
        .collect();
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out
}

fn summary(text: &str) -> String {
    let lines = || text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with("---"));
    let line = lines()
        .find(|l| !l.starts_with('#'))
        .or_else(|| lines().next())
        .unwrap_or("")
        .trim_start_matches('#')
        .trim();
    let mut s: String = line.chars().take(SUMMARY_CHARS).collect();
    if line.chars().count() > SUMMARY_CHARS {
        s.push('…');
    }
    s
}

/// The prompt section for a local AI: what is in the library and how to pull a runbook.
pub fn for_local(dir: &Path, skills: &[Skill], os: &str) -> String {
    let d = dir.display();
    let (shell, read, list) = if os == "windows" {
        ("powershell", "Get-Content -Raw", format!("Get-ChildItem \"{d}\" -Filter *.md"))
    } else {
        ("bash", "cat", format!("ls \"{d}\""))
    };
    let mut s = format!(
        "\n\nSKILL LIBRARY\nRunbooks written for you by the bigger AIs are in {d}. Before a task one of them covers, pull it \
         with a command box and follow it, rather than working it out from scratch:\n```{shell}\n{read} \"{}\"\n```\n",
        dir.join("NAME.md").display()
    );
    if skills.is_empty() {
        s.push_str("The library is empty for now.\n");
        return s;
    }
    s.push_str("Available (name: what it covers):\n");
    for k in skills.iter().take(MAX_LISTED) {
        s.push_str(&format!("- {}: {}\n", k.name, k.summary));
    }
    if skills.len() > MAX_LISTED {
        s.push_str(&format!("…and {} more; list them with `{list}`.\n", skills.len() - MAX_LISTED));
    }
    s
}

/// The prompt section for a flagship AI when local AIs share its council: write runbooks for them.
pub fn for_flagship(dir: &Path, locals: &[String], count: usize) -> String {
    format!(
        "\n\nSKILL LIBRARY FOR THE LOCAL AIs\n{} run offline on small local models with short context and no tools of their own. \
         When you solve something they would struggle with (a tricky build step, a parser, a repo's conventions, a fix that keeps \
         coming back), save a runbook for them in {} as `short-name.md`: first line a one-sentence summary, then concrete steps, \
         exact commands and code to copy. Keep each under about 150 lines, one topic per file, and update a runbook rather than \
         adding a near-copy. The library holds {count} runbook(s) now.",
        locals.join(", "),
        dir.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!("consilium-skills-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn indexes_runbooks_with_a_one_line_summary_and_tells_local_ais_how_to_pull_them() {
        let d = tmp();
        std::fs::write(d.join("rust-tests.md"), "# Rust tests\n\nRun cargo test for one module and read the first failure.\n\nsteps…").unwrap();
        std::fs::write(d.join("Git-branch.md"), "# Work on a branch and commit").unwrap();
        std::fs::write(d.join("notes.txt"), "not a runbook").unwrap();
        let skills = index(&d);
        assert_eq!(skills.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["Git-branch", "rust-tests"]);
        assert_eq!(skills[1].summary, "Run cargo test for one module and read the first failure.");
        assert_eq!(skills[0].summary, "Work on a branch and commit");

        let win = for_local(&d, &skills, "windows");
        assert!(win.contains("```powershell") && win.contains("Get-Content -Raw") && win.contains("- rust-tests: Run cargo test"));
        let mac = for_local(&d, &[], "macos");
        assert!(mac.contains("```bash") && mac.contains("cat \"") && mac.contains("empty for now"));

        let flag = for_flagship(&d, &["Qwen Coder".into(), "Llama".into()], 2);
        assert!(flag.contains("Qwen Coder, Llama") && flag.contains("2 runbook"));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn long_summaries_are_cut_and_a_missing_folder_is_an_empty_library() {
        assert_eq!(summary(&"x".repeat(300)).chars().count(), SUMMARY_CHARS + 1);
        assert!(index(Path::new("/no/such/consilium/skills")).is_empty());
    }
}
