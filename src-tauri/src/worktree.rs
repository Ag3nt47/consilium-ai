//! Separate copies of the code for each AI in Full capacity ("isolated copies").
//!
//! When turned on, an AI with Unrestricted access no longer works in the project folder itself.
//! It gets its own folder that mirrors the project: every git repository in the project becomes a
//! `git worktree` on the branch `consilium/<agent>`. The AI commits there; nothing reaches the
//! person's own checkout until they merge that branch from Consilium (`merge`), and `discard`
//! throws it away. Only git repositories are copied: loose files outside a repository aren't.
//!
//! Disk: a worktree shares the repository's object store, so a copy costs only its checked-out
//! files. Rust builds would otherwise each need their own multi-GB `target` folder, so the
//! mirror folder gets a `.cargo/config.toml` (cargo reads config from parent folders) pointing
//! every copy at the main checkout's existing `target`. Cargo locks a target folder while it
//! builds, so the copies' builds also run one at a time. `node_modules` is linked, not copied.
//!
//! Layout: `<data>/worktrees/<workspace hash>/<agent>/<repo name>`.

use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The branch an AI's copy works on.
pub fn branch(agent: &str) -> String {
    format!("consilium/{agent}")
}

/// Where an AI's copies of `workspace` live, stable across turns and restarts.
pub fn agent_root(data: &Path, workspace: &Path, agent: &str) -> PathBuf {
    // FNV-1a: stable across Rust versions, unlike `DefaultHasher`.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in workspace.to_string_lossy().as_bytes() {
        h = (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3);
    }
    data.join("worktrees").join(format!("{h:016x}")).join(agent)
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir).args(args);
    // Never wait on a credential or editor prompt.
    cmd.env("GIT_TERMINAL_PROMPT", "0").env("GIT_EDITOR", "true");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let out = cmd.output().map_err(|e| format!("couldn't run git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// A main checkout: a folder with a `.git` *directory* (a `.git` file marks a worktree or
/// submodule, which we don't copy again).
fn is_main_repo(dir: &Path) -> bool {
    dir.join(".git").is_dir()
}

/// The git repositories in a project folder: the folder itself if it is one, otherwise the
/// repositories directly inside it (hidden folders skipped), sorted by name.
pub fn repos(workspace: &Path) -> Vec<PathBuf> {
    if is_main_repo(workspace) {
        return vec![workspace.to_path_buf()];
    }
    let mut found: Vec<PathBuf> = std::fs::read_dir(workspace)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')))
        .filter(|p| is_main_repo(p))
        .collect();
    found.sort();
    found
}

fn name_of(repo: &Path) -> String {
    repo.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "repo".into())
}

/// The main checkout's existing Rust build folder, if any, for the copies to share.
fn shared_target(repos: &[PathBuf]) -> Option<PathBuf> {
    repos.iter().flat_map(|r| [r.join("target"), r.join("src-tauri").join("target")]).find(|t| t.is_dir())
}

fn link_dir(original: &Path, link: &Path) {
    if !original.is_dir() || link.exists() || link.symlink_metadata().is_ok() {
        return;
    }
    #[cfg(unix)]
    let _ = std::os::unix::fs::symlink(original, link);
    #[cfg(windows)]
    {
        // A junction needs no admin rights, unlike a directory symlink.
        use std::os::windows::process::CommandExt;
        let _ = Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(original)
            .creation_flags(0x0800_0000)
            .output();
    }
}

/// Makes or refreshes one repository's copy on the AI's branch.
fn prepare_repo(repo: &Path, copy: &Path, branch: &str) -> Result<(), String> {
    let head = git(repo, &["rev-parse", "HEAD"])?;
    if copy.join(".git").exists() {
        // Bring an idle copy up to date: clean, and nothing of its own beyond the main checkout's
        // HEAD (everything it made has been merged or it made nothing). Work in progress is left
        // exactly as the AI left it.
        let clean = git(copy, &["status", "--porcelain"])?.is_empty();
        let own = git(copy, &["rev-list", "--count", &format!("{head}..HEAD")])?;
        if clean && own == "0" {
            git(copy, &["merge", "--ff-only", "--quiet", &head])?;
        }
        return Ok(());
    }
    // A copy folder deleted by hand leaves a stale worktree record behind.
    let _ = git(repo, &["worktree", "prune"]);
    if let Some(parent) = copy.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("couldn't create {}: {e}", parent.display()))?;
    }
    let copy_s = copy.to_string_lossy().into_owned();
    let exists = git(repo, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")]).is_ok();
    if exists {
        git(repo, &["worktree", "add", "--quiet", &copy_s, branch])?;
    } else {
        git(repo, &["worktree", "add", "--quiet", "-b", branch, &copy_s, &head])?;
    }
    Ok(())
}

/// The AI's isolated folder for this turn.
pub struct Isolated {
    /// Folder to run the AI in, in place of the project folder.
    pub dir: PathBuf,
    /// Added to the AI's instructions.
    pub note: String,
}

/// Prepares `agent`'s copies of every repository in `workspace`. `None` when the project has no
/// git repository (there is nothing to copy, so the AI keeps working in the folder itself).
pub fn prepare(data: &Path, workspace: &Path, agent: &str) -> Result<Option<Isolated>, String> {
    let found = repos(workspace);
    if found.is_empty() {
        return Ok(None);
    }
    let root = agent_root(data, workspace, agent);
    std::fs::create_dir_all(&root).map_err(|e| format!("couldn't create {}: {e}", root.display()))?;
    let branch = branch(agent);
    for repo in &found {
        let copy = root.join(name_of(repo));
        prepare_repo(repo, &copy, &branch)?;
        link_dir(&repo.join("node_modules"), &copy.join("node_modules"));
    }
    if let Some(target) = shared_target(&found) {
        let cargo = root.join(".cargo");
        let _ = std::fs::create_dir_all(&cargo);
        // TOML basic string: escape backslashes and quotes (Windows paths).
        let t = target.to_string_lossy().replace('\\', "\\\\").replace('"', "\\\"");
        let _ = std::fs::write(cargo.join("config.toml"), format!("[build]\ntarget-dir = \"{t}\"\n"));
    }
    let dir = if found.len() == 1 && found[0] == workspace { root.join(name_of(workspace)) } else { root.clone() };
    let note = format!(
        "ISOLATED COPY: you are working in your own copy of the project at {}, not in the person's \
         folder {}. Each git repository there is a worktree on the branch `{branch}`. Commit your \
         work to that branch. Don't push, don't merge into main, and don't edit the person's folder \
         directly: the person reviews and merges your branch from Consilium. Rust builds share one \
         build folder with the main checkout, so they run one at a time.",
        dir.display(),
        workspace.display()
    );
    Ok(Some(Isolated { dir, note }))
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct BranchInfo {
    pub agent: String,
    pub repo: String,
    pub branch: String,
    /// Commits on the AI's branch that the main checkout doesn't have.
    pub ahead: u32,
    /// Commits on the main checkout the AI's copy doesn't have yet.
    pub behind: u32,
    /// Uncommitted files in the AI's copy (not included in a merge).
    pub uncommitted: u32,
    /// One line per commit to merge, newest first (at most 20).
    pub commits: Vec<String>,
}

/// Every AI copy of `workspace` that exists, with what merging it would bring in.
pub fn list(data: &Path, workspace: &Path) -> Vec<BranchInfo> {
    let base = agent_root(data, workspace, "_");
    let Some(base) = base.parent() else { return vec![] };
    let mut agents: Vec<String> = std::fs::read_dir(base)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    agents.sort();
    let mut out = vec![];
    for agent in agents {
        for repo in repos(workspace) {
            let copy = base.join(&agent).join(name_of(&repo));
            if !copy.join(".git").exists() {
                continue;
            }
            let b = branch(&agent);
            let count = |range: String| git(&repo, &["rev-list", "--count", &range]).ok().and_then(|s| s.parse().ok()).unwrap_or(0);
            let ahead = count(format!("HEAD..{b}"));
            let behind = count(format!("{b}..HEAD"));
            let uncommitted = git(&copy, &["status", "--porcelain"]).map(|s| s.lines().count() as u32).unwrap_or(0);
            let commits = git(&repo, &["log", "--format=%h %s", "-n", "20", &format!("HEAD..{b}")])
                .map(|s| s.lines().map(str::to_string).collect())
                .unwrap_or_default();
            out.push(BranchInfo { agent: agent.clone(), repo: name_of(&repo), branch: b, ahead, behind, uncommitted, commits });
        }
    }
    out
}

fn find_repo(workspace: &Path, repo: &str) -> Result<PathBuf, String> {
    repos(workspace).into_iter().find(|r| name_of(r) == repo).ok_or_else(|| format!("no repository named {repo}"))
}

fn check_agent(agent: &str) -> Result<(), String> {
    if agent.is_empty() || !agent.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err("not a valid AI id".into());
    }
    Ok(())
}

/// Merges an AI's branch into the main checkout's current branch. A conflict is backed out, so
/// the person's checkout is never left half-merged.
pub fn merge(workspace: &Path, repo: &str, agent: &str) -> Result<String, String> {
    check_agent(agent)?;
    let repo = find_repo(workspace, repo)?;
    let b = branch(agent);
    let msg = format!("Merge {b}");
    match git(&repo, &["merge", "--no-ff", "--no-edit", "-m", &msg, &b]) {
        Ok(_) => Ok(git(&repo, &["rev-parse", "--short", "HEAD"])?),
        Err(e) => {
            let _ = git(&repo, &["merge", "--abort"]);
            Err(format!("couldn't merge {b}; nothing was changed. {e}"))
        }
    }
}

/// Throws away an AI's copy and its branch, including anything not merged.
pub fn discard(data: &Path, workspace: &Path, repo: &str, agent: &str) -> Result<(), String> {
    check_agent(agent)?;
    let main = find_repo(workspace, repo)?;
    let copy = agent_root(data, workspace, agent).join(repo);
    if copy.exists() {
        git(&main, &["worktree", "remove", "--force", &copy.to_string_lossy()])?;
    }
    let _ = git(&main, &["worktree", "prune"]);
    let _ = git(&main, &["branch", "-D", &branch(agent)]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("consilium-wt-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn sh(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t", "-c", "commit.gpgsign=false"])
            .args(args)
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    }

    fn init(repo: &Path) {
        std::fs::create_dir_all(repo).unwrap();
        sh(repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("a.txt"), "one\n").unwrap();
        sh(repo, &["add", "."]);
        sh(repo, &["commit", "-q", "-m", "first"]);
    }

    #[test]
    fn copies_merge_on_request_and_refresh_when_idle() {
        let ws = tmp("ws");
        let data = tmp("data");
        let repo = ws.join("proj");
        init(&repo);
        std::fs::create_dir_all(ws.join("loose")).unwrap(); // not a repository: not copied

        let iso = prepare(&data, &ws, "claude").unwrap().unwrap();
        assert_eq!(iso.dir, agent_root(&data, &ws, "claude"));
        let copy = iso.dir.join("proj");
        assert!(copy.join("a.txt").is_file());
        assert!(!iso.dir.join("loose").exists());

        // The AI commits in its copy; the person's checkout is untouched until merged.
        std::fs::write(copy.join("a.txt"), "two\n").unwrap();
        sh(&copy, &["commit", "-q", "-am", "ai change"]);
        assert_eq!(std::fs::read_to_string(repo.join("a.txt")).unwrap(), "one\n");
        let info = list(&data, &ws);
        assert_eq!(info.len(), 1);
        assert_eq!((info[0].ahead, info[0].uncommitted), (1, 0));
        assert!(info[0].commits[0].ends_with("ai change"));

        // Preparing again keeps unmerged work.
        prepare(&data, &ws, "claude").unwrap();
        assert_eq!(std::fs::read_to_string(copy.join("a.txt")).unwrap(), "two\n");

        // Merging needs a committer identity in the main repo.
        sh(&repo, &["config", "user.name", "t"]);
        sh(&repo, &["config", "user.email", "t"]);
        merge(&ws, "proj", "claude").unwrap();
        assert_eq!(std::fs::read_to_string(repo.join("a.txt")).unwrap(), "two\n");

        // The person commits more; the idle, fully merged copy catches up on the next turn.
        std::fs::write(repo.join("b.txt"), "b\n").unwrap();
        sh(&repo, &["add", "."]);
        sh(&repo, &["commit", "-q", "-m", "human change"]);
        prepare(&data, &ws, "claude").unwrap();
        assert!(copy.join("b.txt").is_file());

        discard(&data, &ws, "proj", "claude").unwrap();
        assert!(!copy.exists());
        assert!(list(&data, &ws).is_empty());
        let _ = std::fs::remove_dir_all(&ws);
        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn conflicting_merge_is_backed_out_and_bad_ids_refused() {
        let ws = tmp("conflict");
        let data = tmp("conflict-data");
        init(&ws); // the project folder is itself the repository
        sh(&ws, &["config", "user.name", "t"]);
        sh(&ws, &["config", "user.email", "t"]);
        let iso = prepare(&data, &ws, "gpt").unwrap().unwrap();
        assert!(iso.dir.join("a.txt").is_file());
        std::fs::write(iso.dir.join("a.txt"), "ai\n").unwrap();
        sh(&iso.dir, &["commit", "-q", "-am", "ai"]);
        std::fs::write(ws.join("a.txt"), "human\n").unwrap();
        sh(&ws, &["commit", "-q", "-am", "human"]);
        let name = ws.file_name().unwrap().to_string_lossy().into_owned();
        assert!(merge(&ws, &name, "gpt").is_err());
        assert_eq!(std::fs::read_to_string(ws.join("a.txt")).unwrap(), "human\n");
        assert!(Command::new("git").arg("-C").arg(&ws).args(["status", "--porcelain"]).output().unwrap().stdout.is_empty());
        assert!(merge(&ws, &name, "../x").is_err());
        assert!(discard(&data, &ws, &name, "a b").is_err());
        discard(&data, &ws, &name, "gpt").unwrap();
        let _ = std::fs::remove_dir_all(&ws);
        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn no_repository_means_no_copy() {
        let ws = tmp("plain");
        let data = tmp("plain-data");
        assert!(prepare(&data, &ws, "claude").unwrap().is_none());
        let _ = std::fs::remove_dir_all(&ws);
        let _ = std::fs::remove_dir_all(&data);
    }
}
