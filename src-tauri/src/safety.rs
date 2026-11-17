//! Session Safety: snapshots uncommitted changes across all active workspace repositories
//! before Update Now closes the application, protecting in-progress work across app restarts
//! and updates without modifying the active working tree.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SnapshotReport {
    pub repo: String,
    pub name: String,
    pub commit: Option<String>,
    pub tracked_files: Vec<String>,
    pub untracked_files: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SafetyLedger {
    pub timestamp: u64,
    pub version: String,
    pub snapshots: Vec<SnapshotReport>,
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir);
    // Explicit identity and non-interactive environment to ensure stash create never prompts or fails
    cmd.args(["-c", "user.name=Consilium", "-c", "user.email=consilium@local", "-c", "commit.gpgsign=false"]);
    cmd.args(args);
    cmd.env("GIT_TERMINAL_PROMPT", "0").env("GIT_EDITOR", "true");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let out = cmd.output().map_err(|e| format!("couldn't run git in {}: {e}", dir.display()))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

fn copy_dir_all(src: &Path, dst: &Path) {
    if std::fs::create_dir_all(dst).is_err() {
        return;
    }
    if let Ok(entries) = std::fs::read_dir(src) {
        for entry in entries.flatten() {
            let path = entry.path();
            let dest_path = dst.join(entry.file_name());
            if path.is_dir() {
                copy_dir_all(&path, &dest_path);
            } else if path.is_file() {
                let _ = std::fs::copy(&path, &dest_path);
            }
        }
    }
}

fn parse_status_line(line: &str) -> Option<(String, String)> {
    let line = line.trim_end();
    if line.len() < 3 {
        return None;
    }
    let (code, rest) = if let Some(stripped) = line.strip_prefix("?? ") {
        ("??", stripped)
    } else if line.len() >= 3 && &line[2..3] == " " {
        (line[..2].trim(), &line[3..])
    } else if let Some((c, r)) = line.split_once(' ') {
        (c.trim(), r)
    } else {
        return None;
    };
    let mut file_path = rest.trim();
    if let Some((_, target)) = file_path.split_once(" -> ") {
        file_path = target.trim();
    }
    let path = if file_path.starts_with('"') && file_path.ends_with('"') && file_path.len() >= 2 {
        &file_path[1..file_path.len() - 1]
    } else {
        file_path
    };
    Some((code.to_string(), path.to_string()))
}

/// Snapshots one repository's uncommitted tracked changes into a commit object (`git stash create`)
/// and preserves untracked files in `backup_dir` without touching the active working tree.
pub fn snapshot_repo(repo: &Path, backup_dir: &Path, version: &str) -> Option<SnapshotReport> {
    let status_out = git(repo, &["status", "--porcelain"]).ok()?;
    let lines: Vec<&str> = status_out.lines().map(|l| l.trim_end()).filter(|l| !l.trim().is_empty()).collect();
    if lines.is_empty() {
        return None;
    }

    let repo_name = repo
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".into());

    let mut tracked = Vec::new();
    let mut untracked = Vec::new();

    for l in lines {
        if let Some((code, path)) = parse_status_line(l) {
            if code == "??" {
                untracked.push(path);
            } else {
                tracked.push(path);
            }
        }
    }

    if tracked.is_empty() && untracked.is_empty() {
        return None;
    }

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let mut commit = None;

    // Tracked changes: create stash commit without modifying working tree or index
    if !tracked.is_empty() {
        let msg = format!("consilium-session-safety-{version}-{ts}");
        if let Ok(hash) = git(repo, &["stash", "create", &msg]) {
            let h = hash.trim();
            if h.len() >= 40 {
                commit = Some(h.to_string());
                // Pin branch consilium/session-safety so commit is preserved and never garbage-collected
                let _ = git(repo, &["branch", "-f", "consilium/session-safety", h]);
            }
        }
    }

    // Untracked files: backup to safety directory
    if !untracked.is_empty() {
        let repo_backup = backup_dir.join(&repo_name).join("untracked");
        for rel in &untracked {
            let src = repo.join(rel);
            let dst = repo_backup.join(rel);
            if src.is_file() {
                if let Some(parent) = dst.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::copy(&src, &dst);
            } else if src.is_dir() {
                copy_dir_all(&src, &dst);
            }
        }
    }

    Some(SnapshotReport {
        repo: repo.to_string_lossy().into_owned(),
        name: repo_name,
        commit,
        tracked_files: tracked,
        untracked_files: untracked,
    })
}

/// Discovers all git repositories across default workspace, conversation workspaces, and current folder.
pub fn find_all_repos(engine: &crate::engine::Engine) -> Vec<PathBuf> {
    let mut workspaces = Vec::new();
    if let Some(ws) = engine.config.get().default_workspace {
        if !ws.trim().is_empty() {
            workspaces.push(PathBuf::from(ws));
        }
    }
    for conv in engine.store.list() {
        if let Some(ws) = conv.workspace {
            if !ws.trim().is_empty() {
                let p = PathBuf::from(ws);
                if !workspaces.contains(&p) {
                    workspaces.push(p);
                }
            }
        }
    }

    let mut repos = Vec::new();
    for ws in workspaces {
        if ws.is_dir() {
            for r in crate::worktree::repos(&ws) {
                if !repos.contains(&r) {
                    repos.push(r);
                }
            }
        }
    }

    if repos.is_empty() {
        if let Ok(cwd) = std::env::current_dir() {
            for r in crate::worktree::repos(&cwd) {
                if !repos.contains(&r) {
                    repos.push(r);
                }
            }
        }
    }

    if repos.is_empty() {
        if let Some(proj) = dirs::home_dir().map(|h| h.join("Projects")) {
            if proj.is_dir() {
                for r in crate::worktree::repos(&proj) {
                    if !repos.contains(&r) {
                        repos.push(r);
                    }
                }
            }
        }
    }

    repos.sort();
    repos.dedup();
    repos
}

/// Saves a snapshot of every active project repository's uncommitted work before Update Now
/// closes the app. Records snapshot metadata in `<data>/safety/latest.json`.
pub fn snapshot_all(data: &Path, engine: &crate::engine::Engine, version: &str) -> Vec<SnapshotReport> {
    let repos = find_all_repos(engine);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let session_dir = data.join("safety").join(format!("session-{ts}"));
    let mut reports = Vec::new();

    for repo in repos {
        if let Some(report) = snapshot_repo(&repo, &session_dir, version) {
            reports.push(report);
        }
    }

    if !reports.is_empty() {
        let ledger = SafetyLedger {
            timestamp: ts,
            version: version.to_string(),
            snapshots: reports.clone(),
        };
        let safety_dir = data.join("safety");
        let _ = std::fs::create_dir_all(&safety_dir);
        if let Ok(json) = serde_json::to_string_pretty(&ledger) {
            let _ = std::fs::write(safety_dir.join("latest.json"), json);
        }
    }

    reports
}

/// Reads the most recent safety snapshot ledger from `<data>/safety/latest.json`.
pub fn read_latest_ledger(data: &Path) -> Option<SafetyLedger> {
    let path = data.join("safety").join("latest.json");
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

/// Restores a repository's snapshot: copies untracked files back into the working tree
/// and applies tracked changes with standard 3-way merge (`git stash apply`).
pub fn restore_repo(repo: &Path, data: &Path) -> Result<String, String> {
    let ledger = read_latest_ledger(data).ok_or("no session safety ledger found")?;
    let repo_s = repo.to_string_lossy();
    let snapshot = ledger
        .snapshots
        .iter()
        .find(|s| s.repo == repo_s || Path::new(&s.repo) == repo)
        .ok_or_else(|| format!("no snapshot found for {}", repo.display()))?;

    let session_dir = data.join("safety").join(format!("session-{}", ledger.timestamp));
    let untracked_dir = session_dir.join(&snapshot.name).join("untracked");

    // 1. Restore untracked files if missing
    for rel in &snapshot.untracked_files {
        let src = untracked_dir.join(rel);
        let dst = repo.join(rel);
        if src.is_file() && !dst.exists() {
            if let Some(parent) = dst.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::copy(&src, &dst);
        }
    }

    // 2. Apply tracked changes via 3-way merge
    if let Some(commit) = &snapshot.commit {
        let out = git(repo, &["stash", "apply", commit])?;
        Ok(format!("Restored snapshot {commit}: {out}"))
    } else {
        Ok("Restored untracked files; no tracked changes were snapshotted".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("consilium-safety-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn init_repo(dir: &Path) {
        let _ = git(dir, &["init", "-b", "main"]);
        let _ = git(dir, &["config", "core.autocrlf", "false"]);
        std::fs::write(dir.join("tracked.txt"), "initial content\n").unwrap();
        let _ = git(dir, &["add", "tracked.txt"]);
        let _ = git(dir, &["commit", "-m", "initial commit"]);
    }

    #[test]
    fn clean_repo_yields_no_snapshot() {
        let repo = tmp_dir("clean");
        init_repo(&repo);
        let backup = tmp_dir("clean-backup");

        let snap = snapshot_repo(&repo, &backup, "0.3.37");
        assert_eq!(snap, None);

        let _ = std::fs::remove_dir_all(&repo);
        let _ = std::fs::remove_dir_all(&backup);
    }

    #[test]
    fn tracked_changes_are_snapshotted_without_touching_worktree() {
        let repo = tmp_dir("tracked");
        init_repo(&repo);
        let backup = tmp_dir("tracked-backup");

        // Modify tracked file
        std::fs::write(repo.join("tracked.txt"), "modified in session\n").unwrap();

        let snap = snapshot_repo(&repo, &backup, "0.3.37").expect("should create snapshot");
        assert!(snap.commit.is_some());
        assert_eq!(snap.tracked_files, vec!["tracked.txt"]);
        assert!(snap.untracked_files.is_empty());

        // Working tree must remain modified and untouched
        let status = git(&repo, &["status", "--porcelain"]).unwrap();
        assert!(status.contains("tracked.txt"));
        let content = std::fs::read_to_string(repo.join("tracked.txt")).unwrap();
        assert_eq!(content, "modified in session\n");

        // Branch consilium/session-safety points to the snapshot commit
        let branch_commit = git(&repo, &["rev-parse", "consilium/session-safety"]).unwrap();
        assert_eq!(branch_commit, snap.commit.unwrap());

        let _ = std::fs::remove_dir_all(&repo);
        let _ = std::fs::remove_dir_all(&backup);
    }

    #[test]
    fn untracked_files_are_backed_up_without_touching_worktree() {
        let repo = tmp_dir("untracked");
        init_repo(&repo);
        let backup = tmp_dir("untracked-backup");

        // Create untracked file
        std::fs::write(repo.join("new_script.py"), "print('hello')\n").unwrap();

        let snap = snapshot_repo(&repo, &backup, "0.3.37").expect("should create snapshot");
        assert_eq!(snap.untracked_files, vec!["new_script.py"]);
        assert!(snap.tracked_files.is_empty());

        // File must still exist in repo
        assert!(repo.join("new_script.py").is_file());

        // File must be backed up
        let repo_name = repo.file_name().unwrap().to_string_lossy();
        let backed_up = backup.join(repo_name.as_ref()).join("untracked").join("new_script.py");
        assert!(backed_up.is_file());
        assert_eq!(std::fs::read_to_string(backed_up).unwrap(), "print('hello')\n");

        let _ = std::fs::remove_dir_all(&repo);
        let _ = std::fs::remove_dir_all(&backup);
    }

    #[test]
    fn restore_applies_tracked_changes_and_untracked_files() {
        let repo = tmp_dir("restore");
        init_repo(&repo);
        let data = tmp_dir("restore-data");

        // 1. Make tracked edit and untracked file
        std::fs::write(repo.join("tracked.txt"), "modified before update\n").unwrap();
        std::fs::write(repo.join("created.txt"), "untracked file\n").unwrap();

        // 2. Snapshot
        let snap = snapshot_repo(&repo, &data.join("safety").join("session-100"), "0.3.37").unwrap();
        let ledger = SafetyLedger {
            timestamp: 100,
            version: "0.3.37".into(),
            snapshots: vec![snap],
        };
        std::fs::create_dir_all(data.join("safety")).unwrap();
        std::fs::write(data.join("safety").join("latest.json"), serde_json::to_string(&ledger).unwrap()).unwrap();

        // 3. Simulate an update: revert tracked.txt to initial, delete created.txt
        let _ = git(&repo, &["checkout", "tracked.txt"]);
        let _ = std::fs::remove_file(repo.join("created.txt"));
        assert_eq!(std::fs::read_to_string(repo.join("tracked.txt")).unwrap(), "initial content\n");
        assert!(!repo.join("created.txt").exists());

        // 4. Restore
        let res = restore_repo(&repo, &data).unwrap();
        assert!(res.contains("Restored snapshot"));

        // 5. Verify restored state
        assert_eq!(std::fs::read_to_string(repo.join("tracked.txt")).unwrap(), "modified before update\n");
        assert_eq!(std::fs::read_to_string(repo.join("created.txt")).unwrap(), "untracked file\n");

        let _ = std::fs::remove_dir_all(&repo);
        let _ = std::fs::remove_dir_all(&data);
    }
}
