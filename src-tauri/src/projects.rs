//! Versions of the projects being worked on: what's on this PC next to what's on GitHub.
//!
//! A project counts as "being worked on" while it has uncommitted changes, or a commit or index
//! update in the last few hours. When it goes quiet it drops out, so the title-bar tab disappears.
//! The workspace itself is checked if it is a git repo, otherwise each git repo directly inside it.

use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

/// How long after its last change a clean project still counts as active.
const ACTIVE_FOR: Duration = Duration::from_secs(3 * 60 * 60);
/// GitHub is fetched at most this often per project.
const FETCH_EVERY: Duration = Duration::from_secs(120);
const GIT_TIMEOUT: Duration = Duration::from_secs(15);
/// A workspace like ~/Projects can hold many repos; never report more than this.
const MAX_PROJECTS: usize = 6;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ProjectVersion {
    pub name: String,
    pub path: String,
    /// Version from the project's own files on this PC, if it declares one.
    pub local_version: Option<String>,
    /// The same file as it is on GitHub (the remote's default branch, after the last fetch).
    pub remote_version: Option<String>,
    pub local_commit: Option<String>,
    pub remote_commit: Option<String>,
    /// Commits on this PC not yet on GitHub, and the other way round.
    pub ahead: u32,
    pub behind: u32,
    /// Files with uncommitted changes.
    pub dirty: u32,
    /// Set when GitHub couldn't be reached on the last try.
    pub remote_error: Option<String>,
}

static LAST_FETCH: Mutex<Option<HashMap<PathBuf, Instant>>> = Mutex::new(None);

/// Runs git with a hard timeout: the process is killed, not left running, if it hangs.
fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let mut command = Command::new("git");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = command
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .env(
            "GIT_SSH_COMMAND",
            "ssh -o BatchMode=yes -o ConnectTimeout=10",
        )
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let out = child.wait_with_output().ok()?;
                return status
                    .success()
                    .then(|| String::from_utf8_lossy(&out.stdout).trim_end().to_string());
            }
            Ok(None) if start.elapsed() < GIT_TIMEOUT => {
                std::thread::sleep(Duration::from_millis(40))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// Pulls a version out of the usual manifest files, in order of preference.
fn version_from(read: impl Fn(&str) -> Option<String>) -> Option<String> {
    for file in ["package.json", "src-tauri/tauri.conf.json"] {
        if let Some(v) = read(file)
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .and_then(|j| j.get("version")?.as_str().map(str::to_string))
        {
            return Some(v);
        }
    }
    for file in ["Cargo.toml", "src-tauri/Cargo.toml", "pyproject.toml"] {
        if let Some(v) = read(file).and_then(|t| toml_version(&t)) {
            return Some(v);
        }
    }
    read("VERSION")
        .map(|t| t.trim().to_string())
        .filter(|v| !v.is_empty() && v.len() < 40)
}

/// `version = "x"` in the first `[package]` / `[project]` / `[tool.poetry]` table.
fn toml_version(text: &str) -> Option<String> {
    let mut in_table = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_table = matches!(line, "[package]" | "[project]" | "[tool.poetry]");
            continue;
        }
        if in_table {
            if let Some(rest) = line.strip_prefix("version") {
                let v = rest
                    .trim_start()
                    .strip_prefix('=')?
                    .trim()
                    .trim_matches('"');
                if !v.is_empty() {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

fn modified_recently(path: &Path) -> bool {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age < ACTIVE_FOR)
}

fn is_repo(dir: &Path) -> bool {
    dir.join(".git").exists()
}

fn candidates(workspace: &Path) -> Vec<PathBuf> {
    if is_repo(workspace) {
        return vec![workspace.to_path_buf()];
    }
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(workspace)
        .map(|r| {
            r.flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir() && is_repo(p))
                .collect()
        })
        .unwrap_or_default();
    dirs.sort();
    dirs
}

fn should_fetch(dir: &Path) -> bool {
    let mut guard = LAST_FETCH.lock().unwrap_or_else(|e| e.into_inner());
    let map = guard.get_or_insert_with(HashMap::new);
    match map.get(dir) {
        Some(t) if t.elapsed() < FETCH_EVERY => false,
        _ => {
            map.insert(dir.to_path_buf(), Instant::now());
            true
        }
    }
}

/// Uncommitted files, or `None` when `dir` isn't a working git checkout.
fn dirty_count(dir: &Path) -> Option<u32> {
    let status = git(dir, &["status", "--porcelain"])?;
    Some(status.lines().filter(|l| !l.trim().is_empty()).count() as u32)
}

/// Whether a checkout has been worked on lately: uncommitted changes, or a recent commit or
/// index update.
fn is_active(dir: &Path, dirty: u32) -> bool {
    let recent_commit = git(dir, &["log", "-1", "--format=%ct"])
        .and_then(|s| s.trim().parse::<u64>().ok())
        .and_then(|t| {
            let now = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .ok()?
                .as_secs();
            Some(now.saturating_sub(t) < ACTIVE_FOR.as_secs())
        })
        .unwrap_or(false);
    dirty > 0 || recent_commit || modified_recently(&dir.join(".git/index"))
}

/// The repository a checkout belongs to. Every worktree of one repo shares it.
fn repo_key(dir: &Path) -> PathBuf {
    git(
        dir,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .map(|s| PathBuf::from(s.trim()))
    .and_then(|p| std::fs::canonicalize(&p).ok().or(Some(p)))
    .unwrap_or_else(|| dir.join(".git"))
}

/// The main checkout (its `.git` is a folder, not a worktree's pointer file), else the first.
fn main_checkout(members: &[PathBuf]) -> &PathBuf {
    members
        .iter()
        .find(|d| d.join(".git").is_dir())
        .unwrap_or(&members[0])
}

/// Reports `dir`, if it or (with `sibling_active`) another worktree of its repo is active.
fn inspect(dir: &Path, sibling_active: bool) -> Option<ProjectVersion> {
    let dirty = dirty_count(dir)?;
    if !sibling_active && !is_active(dir, dirty) {
        return None;
    }

    let has_remote = git(dir, &["remote", "get-url", "origin"]).is_some();
    let mut remote_error = None;
    if has_remote && should_fetch(dir) && git(dir, &["fetch", "--quiet", "origin"]).is_none() {
        remote_error = Some("Couldn't reach GitHub; showing the last fetched version".into());
    }
    // The remote's default branch, else the branch this one tracks.
    let remote_ref = ["origin/HEAD", "@{upstream}", "origin/main", "origin/master"]
        .into_iter()
        .find(|r| git(dir, &["rev-parse", "--verify", "--quiet", r]).is_some());

    let local_version = version_from(|f| std::fs::read_to_string(dir.join(f)).ok());
    let (remote_version, remote_commit, ahead, behind) = match remote_ref {
        Some(r) => {
            let counts = git(
                dir,
                &[
                    "rev-list",
                    "--left-right",
                    "--count",
                    &format!("HEAD...{r}"),
                ],
            );
            let mut it = counts
                .as_deref()
                .unwrap_or("")
                .split_whitespace()
                .map(|n| n.parse().unwrap_or(0));
            (
                version_from(|f| git(dir, &["show", &format!("{r}:{f}")])),
                git(dir, &["rev-parse", "--short", r]),
                it.next().unwrap_or(0),
                it.next().unwrap_or(0),
            )
        }
        None => (None, None, 0, 0),
    };

    Some(ProjectVersion {
        name: dir.file_name()?.to_string_lossy().into_owned(),
        path: dir.to_string_lossy().into_owned(),
        local_version,
        remote_version,
        local_commit: git(dir, &["rev-parse", "--short", "HEAD"]),
        remote_commit,
        ahead,
        behind,
        dirty,
        remote_error: if has_remote {
            remote_error
        } else {
            Some("No GitHub remote".into())
        },
    })
}

/// Groups checkouts by repository, so a repo's extra git worktrees (for example
/// `consilium-autorun` beside `consilium-ai`) don't each show up as another project.
fn group_by_repo(dirs: Vec<PathBuf>, key: impl Fn(&Path) -> PathBuf) -> Vec<Vec<PathBuf>> {
    let mut groups: Vec<(PathBuf, Vec<PathBuf>)> = Vec::new();
    for d in dirs {
        let k = key(&d);
        match groups.iter_mut().find(|(g, _)| *g == k) {
            Some((_, members)) => members.push(d),
            None => groups.push((k, vec![d])),
        }
    }
    groups.into_iter().map(|(_, m)| m).collect()
}

/// Active projects in `workspace`, one per repository, sorted by folder name. A repo counts as
/// active when any of its worktrees is, and is reported from its main checkout.
pub fn active(workspace: &Path) -> Vec<ProjectVersion> {
    let groups = group_by_repo(candidates(workspace), repo_key);
    groups
        .iter()
        .filter_map(|members| {
            let main = main_checkout(members);
            let sibling_active = members.iter().filter(|m| *m != main).any(|m| {
                dirty_count(m).is_some_and(|dirty| is_active(m, dirty))
            });
            inspect(main, sibling_active)
        })
        .take(MAX_PROJECTS)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_versions_from_common_manifests() {
        let files = |name: &'static str, body: &'static str| {
            move |f: &str| (f == name).then(|| body.to_string())
        };
        assert_eq!(
            version_from(files("package.json", r#"{"version":"1.2.3"}"#)),
            Some("1.2.3".into())
        );
        assert_eq!(
            version_from(files(
                "Cargo.toml",
                "[dependencies]\nversion = \"9\"\n[package]\nname=\"x\"\nversion = \"0.4.0\"\n"
            )),
            Some("0.4.0".into())
        );
        assert_eq!(
            version_from(files("pyproject.toml", "[project]\nversion = \"2.0\"\n")),
            Some("2.0".into())
        );
        assert_eq!(version_from(|_| None), None);
    }

    #[test]
    fn worktrees_of_one_repo_are_one_project() {
        let key = |d: &Path| {
            let name = d.file_name().unwrap().to_string_lossy().into_owned();
            PathBuf::from(if name.starts_with("consilium") { "repo-a" } else { "repo-b" })
        };
        let dirs = ["consilium-ai", "consilium-autorun", "other", "consilium-retro"]
            .map(PathBuf::from)
            .to_vec();
        let groups = group_by_repo(dirs, key);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].len(), 3);
        assert_eq!(groups[0][0], PathBuf::from("consilium-ai"));
        assert_eq!(groups[1], vec![PathBuf::from("other")]);
    }

    #[test]
    fn a_linked_worktree_is_folded_into_its_main_checkout() {
        let dir =
            std::env::temp_dir().join(format!("consilium-projects-wt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let main = dir.join("app");
        std::fs::create_dir_all(&main).unwrap();
        if git(&main, &["init", "-q"]).is_none() {
            return; // no git on this machine
        }
        std::fs::write(main.join("package.json"), r#"{"version":"0.2.0"}"#).unwrap();
        let committed = git(&main, &["add", "-A"]).is_some()
            && git(
                &main,
                &["-c", "user.name=t", "-c", "user.email=test", "commit", "-qm", "init"],
            )
            .is_some()
            && git(&main, &["worktree", "add", "-q", "../app-feature", "-b", "feature"]).is_some();
        if committed {
            let found = active(&dir);
            assert_eq!(found.len(), 1, "{found:?}");
            assert_eq!(found[0].name, "app");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_quiet_folder_without_git_reports_nothing() {
        let dir = std::env::temp_dir().join(format!("consilium-projects-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("plain")).unwrap();
        assert!(active(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_new_repo_with_changes_is_active_and_has_no_remote() {
        let dir =
            std::env::temp_dir().join(format!("consilium-projects-repo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        if git(&dir, &["init", "-q"]).is_none() {
            return; // no git on this machine
        }
        std::fs::write(dir.join("package.json"), r#"{"version":"0.1.0"}"#).unwrap();
        let found = active(&dir);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].local_version.as_deref(), Some("0.1.0"));
        assert_eq!(found[0].remote_version, None);
        assert!(found[0].dirty >= 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
