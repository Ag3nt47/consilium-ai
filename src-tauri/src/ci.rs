//! GitHub checks for the projects being worked on, read with the GitHub CLI (`gh`) so the AIs
//! don't have to keep running `gh run list` themselves.
//!
//! One background watcher checks each active project's default branch about once a minute. The
//! title bar shows the result as a green, red or amber light, every AI's prompt carries the last
//! result, and a Consilium note goes into the conversation when the newest commit's checks finish
//! (passed or failed), so a failure is noticed straight away.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// A project's checks are read from GitHub at most this often.
const REFRESH_EVERY: Duration = Duration::from_secs(60);
const GH_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CiStatus {
    /// Folder name of the project.
    pub project: String,
    pub path: String,
    pub branch: String,
    /// Short ID of the newest commit that has checks.
    pub commit: Option<String>,
    /// "pass", "fail", "running", "none" (no checks on GitHub) or "error" (couldn't ask).
    pub state: String,
    /// Each workflow and job that failed, such as "CI › Rust core (windows-latest)".
    pub failed: Vec<String>,
    /// How many workflow runs belong to that commit.
    pub runs: usize,
    /// Page to open: the failing run, or the newest one.
    pub url: Option<String>,
    pub detail: Option<String>,
    /// When this was read, ms since the epoch.
    pub checked_at: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Run {
    database_id: u64,
    status: String,
    #[serde(default)]
    conclusion: Option<String>,
    head_sha: String,
    workflow_name: String,
    url: String,
}

static CACHE: Mutex<Option<HashMap<PathBuf, (Instant, CiStatus)>>> = Mutex::new(None);

fn gh() -> Option<PathBuf> {
    crate::bridge::find_tool("gh")
}

pub fn request_draft_installers(data: &Path) -> Result<Option<String>, String> {
    let program = gh().ok_or("Install and sign in to the GitHub CLI to build installers.")?;
    let output = run(&program, data, &[
        "workflow", "run", "release.yml", "--repo", "Ag3nt47/consilium-ai",
        "--ref", "main", "--raw-field", "draft_only=true",
    ])?;
    Ok(installer_run_url(&output))
}

fn installer_run_url(output: &str) -> Option<String> {
    const PREFIX: &str = "https://github.com/Ag3nt47/consilium-ai/actions/runs/";
    output.lines().map(str::trim).find_map(|line| {
        let id = line.strip_prefix(PREFIX)?;
        (!id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit())).then(|| line.to_string())
    })
}

/// Runs a command in `dir` with a hard timeout, returning stdout when it succeeds.
fn run(program: &Path, dir: &Path, args: &[&str]) -> Result<String, String> {
    run_with_timeout(program, dir, args, GH_TIMEOUT)
}

fn run_with_timeout(program: &Path, dir: &Path, args: &[&str], timeout: Duration) -> Result<String, String> {
    let mut command = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = command
        .current_dir(dir)
        .args(args)
        .env("GH_PROMPT_DISABLED", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("GitHub didn't answer in time".into());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => return Err(e.to_string()),
        }
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(err.lines().next().unwrap_or("gh failed").trim().chars().take(160).collect())
    }
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    run(Path::new("git"), dir, args).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// The branch GitHub's checks are read for: the remote's default branch, else the current one.
fn branch_of(dir: &Path) -> String {
    git(dir, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"])
        .and_then(|r| r.strip_prefix("origin/").map(str::to_string))
        .or_else(|| git(dir, &["rev-parse", "--abbrev-ref", "HEAD"]))
        .unwrap_or_else(|| "main".into())
}

fn has_github_remote(dir: &Path) -> bool {
    git(dir, &["remote", "get-url", "origin"]).is_some_and(|u| u.contains("github.com"))
}

/// Works out the overall state from the runs of one commit. A failure anywhere wins; then
/// anything still queued or running; then success. Runs GitHub cancelled because a newer push
/// replaced them don't count either way.
fn summarize(runs: &[&Run]) -> &'static str {
    const FAILED: [&str; 4] = ["failure", "timed_out", "startup_failure", "action_required"];
    if runs.iter().any(|r| r.conclusion.as_deref().is_some_and(|c| FAILED.contains(&c))) {
        "fail"
    } else if runs.iter().any(|r| r.status != "completed") {
        "running"
    } else if runs.iter().any(|r| r.conclusion.as_deref() == Some("success")) {
        "pass"
    } else {
        "none"
    }
}

/// Picks the newest commit with checks worth reporting: its runs, newest first. A commit whose
/// runs were all cancelled (replaced by a newer push) is skipped.
fn newest_commit(runs: &[Run]) -> Vec<&Run> {
    let mut order: Vec<&str> = Vec::new();
    for r in runs {
        if !order.contains(&r.head_sha.as_str()) {
            order.push(&r.head_sha);
        }
    }
    for sha in order {
        let these: Vec<&Run> = runs.iter().filter(|r| r.head_sha == sha).collect();
        let all_cancelled = these.iter().all(|r| r.conclusion.as_deref() == Some("cancelled"));
        if !all_cancelled {
            return these;
        }
    }
    Vec::new()
}

fn now_ms() -> u64 {
    crate::store::now_ms()
}

/// Reads the checks for one project from GitHub, ignoring the cache.
fn fetch(dir: &Path) -> CiStatus {
    let project = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let branch = branch_of(dir);
    let mut status = CiStatus {
        project,
        path: dir.display().to_string(),
        branch: branch.clone(),
        commit: None,
        state: "error".into(),
        failed: Vec::new(),
        runs: 0,
        url: None,
        detail: None,
        checked_at: now_ms(),
    };
    let Some(gh) = gh() else {
        status.detail = Some("The GitHub CLI (gh) isn't installed".into());
        return status;
    };
    let json = match run(
        &gh,
        dir,
        &["run", "list", "--branch", &branch, "--limit", "20", "--json",
          "databaseId,status,conclusion,headSha,workflowName,url"],
    ) {
        Ok(j) => j,
        Err(e) => {
            status.detail = Some(e);
            return status;
        }
    };
    let all: Vec<Run> = match serde_json::from_str(&json) {
        Ok(r) => r,
        Err(e) => {
            status.detail = Some(format!("Unexpected answer from gh: {e}"));
            return status;
        }
    };
    let runs = newest_commit(&all);
    status.state = summarize(&runs).into();
    status.runs = runs.len();
    status.commit = runs.first().map(|r| r.head_sha.chars().take(7).collect());
    status.url = runs
        .iter()
        .find(|r| summarize(&[r]) == "fail")
        .or(runs.first())
        .map(|r| r.url.clone());
    if status.state == "fail" {
        for r in runs.iter().filter(|r| summarize(&[r]) == "fail") {
            let id = r.database_id.to_string();
            let jobs = run(&gh, dir, &["run", "view", &id, "--json", "jobs", "-q",
                r#".jobs[] | select(.conclusion=="failure" or .conclusion=="timed_out") | .name"#]);
            let names: Vec<String> = jobs
                .ok()
                .map(|j| j.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect())
                .unwrap_or_default();
            if names.is_empty() {
                status.failed.push(r.workflow_name.clone());
            } else {
                status.failed.extend(names.into_iter().map(|n| format!("{} › {n}", r.workflow_name)));
            }
        }
    }
    status
}

/// The project's checks, read from GitHub if the last read is older than a minute.
pub fn check(dir: &Path) -> CiStatus {
    if let Some((at, s)) = CACHE.lock().unwrap().as_ref().and_then(|c| c.get(dir)).cloned() {
        if at.elapsed() < REFRESH_EVERY {
            return s;
        }
    }
    let fresh = fetch(dir);
    CACHE
        .lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .insert(dir.to_path_buf(), (Instant::now(), fresh.clone()));
    fresh
}

/// The cached checks for projects in `workspace`, without asking GitHub.
fn cached_in(workspace: &Path) -> Vec<CiStatus> {
    let mut out: Vec<CiStatus> = CACHE
        .lock()
        .unwrap()
        .as_ref()
        .map(|c| {
            c.iter()
                .filter(|(p, _)| p.starts_with(workspace))
                .map(|(_, (_, s))| s.clone())
                .collect()
        })
        .unwrap_or_default();
    out.sort_by(|a, b| a.project.cmp(&b.project));
    out
}

/// Checks for each active project with a GitHub remote in `workspace`.
pub fn for_workspace(workspace: &Path) -> Vec<CiStatus> {
    crate::projects::active(workspace)
        .into_iter()
        .map(|p| PathBuf::from(p.path))
        .filter(|p| has_github_remote(p))
        .map(|p| check(&p))
        .collect()
}

fn ago(ms: u64) -> String {
    let s = ms / 1000;
    if s < 90 { format!("{s} s ago") } else { format!("{} min ago", s / 60) }
}

/// One line per project for the AIs' prompt, from the cache only. Empty when nothing is known.
pub fn for_prompt(workspace: &Path) -> String {
    let list = cached_in(workspace);
    if list.is_empty() {
        return String::new();
    }
    let now = now_ms();
    let mut out = String::from(
        "\n\nGitHub checks (Consilium reads these every minute and posts a note when they finish; \
you don't need to poll `gh run list` yourself, only to look at a failure's details):",
    );
    for s in list {
        let what = match s.state.as_str() {
            "pass" => "passing".to_string(),
            "running" => "still running".to_string(),
            "fail" => format!("FAILING: {}", s.failed.join(", ")),
            "none" => "no checks found".to_string(),
            _ => format!("unknown ({})", s.detail.as_deref().unwrap_or("couldn't ask GitHub")),
        };
        out.push_str(&format!(
            "\n- {} {}{}: {what}{} (checked {})",
            s.project,
            s.branch,
            s.commit.as_deref().map(|c| format!(" @{c}")).unwrap_or_default(),
            if s.state == "fail" { s.url.as_deref().map(|u| format!(", {u}")).unwrap_or_default() } else { String::new() },
            ago(now.saturating_sub(s.checked_at)),
        ));
    }
    out
}

/// What the council is asked when a commit's checks fail.
pub const FIX_ASK: &str = "Council: please fix this. Agree among yourselves who takes it (claim it \
with a TASK: line so the others don't duplicate it), read the failing job's log with \
`gh run view <id> --log-failed`, fix the cause or a flaky test, run the tests, commit and push to \
main. Everyone else: only review, and don't push while the fix is in progress.";

/// The note posted when a commit's checks finish, or None if there's nothing new to say.
/// `last` is the (commit, state) last announced for this project.
pub fn transition_note(last: Option<&(String, String)>, s: &CiStatus) -> Option<String> {
    let commit = s.commit.clone()?;
    if s.state != "pass" && s.state != "fail" {
        return None;
    }
    if last.is_some_and(|(c, st)| *c == commit && *st == s.state) {
        return None;
    }
    let was_failing = last.is_some_and(|(_, st)| st == "fail");
    Some(match s.state.as_str() {
        "fail" => format!(
            "GitHub checks failed for {} {} @{commit}: {}. {}",
            s.project,
            s.branch,
            if s.failed.is_empty() { "see the run".into() } else { s.failed.join(", ") },
            s.url.as_deref().unwrap_or("")
        )
        .trim_end()
        .to_string(),
        _ if was_failing => format!("GitHub checks pass again for {} {} @{commit}.", s.project, s.branch),
        _ => format!("GitHub checks passed for {} {} @{commit} ({} workflow{}).", s.project, s.branch, s.runs, if s.runs == 1 { "" } else { "s" }),
    })
}

/// Background watcher: once a minute, checks the projects of every conversation used in the last
/// day and posts a note in each of them when a commit's checks finish. Nothing is posted for the
/// state found on the first pass after Consilium starts, so a restart doesn't repeat old news.
pub async fn watch(engine: std::sync::Arc<crate::engine::Engine>) {
    const RECENT_MS: u64 = 24 * 60 * 60 * 1000;
    let mut announced: HashMap<PathBuf, (String, String)> = HashMap::new();
    let mut first = true;
    loop {
        let e = engine.clone();
        let snapshot = announced.clone();
        let quiet = first;
        let result = tauri::async_runtime::spawn_blocking(move || {
            let now = now_ms();
            let default_ws = e.config.get().default_workspace;
            // Workspace → conversations using it.
            let mut by_ws: HashMap<PathBuf, Vec<String>> = HashMap::new();
            for m in e.store.list() {
                if now.saturating_sub(m.updated) > RECENT_MS {
                    continue;
                }
                if let Some(w) = m.workspace.clone().or_else(|| default_ws.clone()).filter(|w| !w.is_empty()) {
                    by_ws.entry(PathBuf::from(w)).or_default().push(m.id);
                }
            }
            let mut updates: Vec<(PathBuf, (String, String))> = Vec::new();
            for (ws, convs) in by_ws {
                if !ws.is_dir() {
                    continue;
                }
                for s in for_workspace(&ws) {
                    let key = PathBuf::from(&s.path);
                    let Some(commit) = s.commit.clone() else { continue };
                    if s.state != "pass" && s.state != "fail" {
                        continue;
                    }
                    if !quiet {
                        if let Some(note) = transition_note(snapshot.get(&key), &s) {
                            // On a failure, the most recently used conversation for this project
                            // also asks its AIs to fix it; the others just get the note.
                            for (i, c) in convs.iter().enumerate() {
                                if s.state == "fail" && i == 0 {
                                    let _ = e.note_for_council(c, &format!("{note}\n\n{FIX_ASK}"));
                                } else {
                                    let _ = e.note(c, &note);
                                }
                            }
                        }
                    }
                    updates.push((key, (commit, s.state.clone())));
                }
            }
            updates
        })
        .await;
        if let Ok(updates) = result {
            announced.extend(updates);
        }
        first = false;
        tokio::time::sleep(REFRESH_EVERY).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(sha: &str, status: &str, conclusion: Option<&str>) -> Run {
        Run {
            database_id: 1,
            status: status.into(),
            conclusion: conclusion.map(str::to_string),
            head_sha: sha.into(),
            workflow_name: "CI".into(),
            url: "u".into(),
        }
    }

    #[test]
    fn a_failure_wins_then_running_then_pass() {
        let ok = r("a", "completed", Some("success"));
        let bad = r("a", "completed", Some("failure"));
        let going = r("a", "in_progress", None);
        assert_eq!(summarize(&[&ok, &bad, &going]), "fail");
        assert_eq!(summarize(&[&ok, &going]), "running");
        assert_eq!(summarize(&[&ok]), "pass");
        assert_eq!(summarize(&[]), "none");
    }

    #[test]
    fn commits_replaced_by_a_newer_push_are_skipped() {
        let runs = vec![
            r("new", "completed", Some("cancelled")),
            r("old", "completed", Some("success")),
            r("old", "completed", Some("success")),
        ];
        let picked = newest_commit(&runs);
        assert_eq!(picked.len(), 2);
        assert!(picked.iter().all(|x| x.head_sha == "old"));
        let runs = vec![r("new", "queued", None), r("old", "completed", Some("failure"))];
        assert_eq!(summarize(&newest_commit(&runs)), "running");
    }

    #[test]
    fn notes_only_when_a_commit_finishes_with_a_new_result() {
        let mut s = CiStatus {
            project: "app".into(),
            path: "/p/app".into(),
            branch: "main".into(),
            commit: Some("abc1234".into()),
            state: "fail".into(),
            failed: vec!["CI › Rust core (windows-latest)".into()],
            runs: 2,
            url: Some("https://x".into()),
            detail: None,
            checked_at: 0,
        };
        let note = transition_note(None, &s).unwrap();
        assert!(note.contains("failed") && note.contains("windows-latest") && note.contains("@abc1234"));
        let last = ("abc1234".to_string(), "fail".to_string());
        assert_eq!(transition_note(Some(&last), &s), None);
        s.commit = Some("def5678".into());
        s.state = "pass".into();
        assert!(transition_note(Some(&last), &s).unwrap().contains("pass again"));
        s.state = "running".into();
        assert_eq!(transition_note(Some(&last), &s), None);
    }

    #[test]
    fn prompt_note_lists_failures_from_the_cache() {
        let ws = std::env::temp_dir().join(format!("consilium-ci-{}", std::process::id()));
        let dir = ws.join("app");
        let s = CiStatus {
            project: "app".into(),
            path: dir.display().to_string(),
            branch: "main".into(),
            commit: Some("abc1234".into()),
            state: "fail".into(),
            failed: vec!["CI › Front end".into()],
            runs: 1,
            url: Some("https://github.com/o/r/actions/runs/1".into()),
            detail: None,
            checked_at: now_ms(),
        };
        CACHE.lock().unwrap().get_or_insert_with(HashMap::new).insert(dir, (Instant::now(), s));
        let note = for_prompt(&ws);
        assert!(note.contains("app main @abc1234: FAILING: CI › Front end"));
        assert!(for_prompt(Path::new("/nowhere-consilium")).is_empty());
    }
}

#[cfg(test)]
mod live {
    /// Reads this repo's real checks. Needs network and a signed-in gh: `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn reads_this_repos_checks() {
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf();
        let s = super::fetch(&repo);
        println!("{s:?}");
        assert!(s.state != "error", "{:?}", s.detail);
    }
}

#[cfg(all(test, windows))]
mod quiet_background_tests {
    use super::*;
    use std::ffi::OsString;

    const MODE: &str = "CONSILIUM_QUIET_BACKGROUND_FIXTURE_MODE";
    const FIXTURE: &str = "ci::quiet_background_tests::console_fixture";

    /// A harmless owned console executable: the current test binary, running
    /// only this fixture. The exact child arguments keep the parent test pass
    /// from interpreting the temporary environment flag as an exit request.
    #[test]
    fn console_fixture() {
        let args: Vec<_> = std::env::args().collect();
        if !args.windows(2).any(|pair| pair[0] == "--exact" && pair[1] == FIXTURE) {
            return;
        }
        let Ok(mode) = std::env::var(MODE) else { return };
        let window = unsafe { windows_sys::Win32::System::Console::GetConsoleWindow() };
        println!("fixture-console-window={}", window as usize);
        println!("fixture-output=ready");
        println!("fixture-gcm={}", std::env::var("GCM_INTERACTIVE").unwrap_or_default());
        match mode.as_str() {
            "error" => {
                eprintln!("owned fixture failure");
                std::process::exit(23);
            }
            "timeout" => std::thread::sleep(Duration::from_secs(5)),
            _ => {}
        }
    }

    struct RestoreMode(Option<OsString>);

    impl Drop for RestoreMode {
        fn drop(&mut self) {
            if let Some(previous) = &self.0 {
                std::env::set_var(MODE, previous);
            } else {
                std::env::remove_var(MODE);
            }
        }
    }

    #[test]
    fn background_child_has_no_console_and_preserves_output_errors_and_timeout() {
        let _restore = RestoreMode(std::env::var_os(MODE));
        let program = std::env::current_exe().unwrap();
        let dir = std::env::temp_dir();
        let args = ["--exact", FIXTURE, "--nocapture"];

        std::env::set_var(MODE, "success");
        let output = run(&program, &dir, &args).unwrap();
        assert!(output.lines().any(|line| line.ends_with("fixture-console-window=0")), "{output}");
        assert!(output.contains("fixture-output=ready"));
        assert!(output.contains("fixture-gcm=never"));

        std::env::set_var(MODE, "error");
        assert_eq!(run(&program, &dir, &args).unwrap_err(), "owned fixture failure");

        std::env::set_var(MODE, "timeout");
        let started = Instant::now();
        let result = run_with_timeout(&program, &dir, &args, Duration::from_millis(200));
        assert_eq!(result.unwrap_err(), "GitHub didn't answer in time");
        assert!(started.elapsed() < Duration::from_secs(4), "the owned child was not stopped promptly");
    }
}
