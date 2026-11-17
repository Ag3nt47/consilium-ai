//! Keeps linked computers on the same build of Consilium.
//!
//! Every linked computer reports the build it runs in its heartbeat (see `build_id`, `link`). Once
//! a minute this computer looks for a linked computer running a later build that anyone can
//! rebuild (committed, with a known commit). If there is one, and nothing as new is already
//! staged here, it fetches that exact commit from GitHub into its own folder
//! (`<data>/update/source`, never the human's checkout), builds it with
//! `scripts/stage-update.mjs` and stages it. Installing then goes the usual way (Update Now, or the
//! automatic install), and turns cut short resume after the restart.
//!
//! So every computer ends up on the newest build any of them runs, never an older one: a build is
//! only staged when it is later than both the running and the staged build, and the staging script
//! itself refuses to replace a newer staged build. A note in the newest conversation tells the
//! human and the AIs what is happening.

use crate::build_id::Build;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Where builds are fetched from.
const REPO_URL: &str = "https://github.com/Ag3nt47/consilium-ai.git";
const CHECK_EVERY: Duration = Duration::from_secs(60);
/// After a failed attempt, the same build isn't tried again for this long.
const RETRY_AFTER_MS: u64 = 30 * 60_000;
/// Longest a fetch or install step may take, and the build itself.
const STEP_LIMIT: Duration = Duration::from_secs(15 * 60);
const BUILD_LIMIT: Duration = Duration::from_secs(90 * 60);

/// What this computer is doing to keep up with the others, shown with the linked computers.
#[derive(Serialize, Clone, Debug, Default, PartialEq)]
pub struct Status {
    /// "idle", "waiting" (the commit isn't on GitHub yet), "preparing", "staged" or "failed".
    pub state: String,
    /// The build being prepared or staged, e.g. "0.3.37 #812 (76debf3a1b2c)".
    pub target: Option<String>,
    /// The linked computer running it.
    pub from: Option<String>,
    pub detail: Option<String>,
}

static STATUS: Mutex<Option<Status>> = Mutex::new(None);
/// Builds other computers said they are installing (see `announced`), as (computer, build).
static ANNOUNCED: Mutex<Vec<(String, Build)>> = Mutex::new(Vec::new());
/// Wakes the check at once instead of at the next minute.
static WAKE: tokio::sync::Notify = tokio::sync::Notify::const_new();
/// Set when another computer announced an update: the window checks GitHub for it at once.
static CHECK_RELEASE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Another computer is installing `build` and restarting: this one moves to it too, right away,
/// without waiting for that computer to come back and report it.
pub fn announced(computer: &str, build: Build) {
    {
        let mut a = ANNOUNCED.lock().unwrap();
        a.retain(|(c, _)| c != computer);
        a.push((computer.to_string(), build));
    }
    CHECK_RELEASE.store(true, std::sync::atomic::Ordering::SeqCst);
    WAKE.notify_one();
}

/// Whether the window should check GitHub for a release now (once per announcement).
pub fn take_release_check() -> bool {
    CHECK_RELEASE.swap(false, std::sync::atomic::Ordering::SeqCst)
}
/// The build that last failed, and when.
static FAILED: Mutex<Option<(Build, u64)>> = Mutex::new(None);

pub fn status() -> Status {
    STATUS.lock().unwrap().clone().unwrap_or_else(|| Status { state: "idle".into(), ..Default::default() })
}

fn set(state: &str, target: &Build, from: &str, detail: impl Into<String>) {
    *STATUS.lock().unwrap() =
        Some(Status { state: state.into(), target: Some(target.label()), from: Some(from.into()), detail: Some(detail.into()) });
}

/// The build to move to: the latest one a linked computer runs that anyone can rebuild, if it is
/// later than this build and than anything already staged here. Returns (computer, build).
pub fn target(this: &Build, staged: Option<&Build>, peers: &[(String, Build)]) -> Option<(String, Build)> {
    let mut best: Option<&(String, Build)> = None;
    for p in peers {
        if !p.1.reproducible() || !p.1.newer_than(this) || staged.is_some_and(|s| !p.1.newer_than(s)) {
            continue;
        }
        if best.is_none_or(|b| p.1.newer_than(&b.1)) {
            best = Some(p);
        }
    }
    best.cloned()
}

/// Starts the once-a-minute check. Call once, after the link has opened.
pub fn start(engine: Arc<crate::engine::Engine>, data: PathBuf) {
    let e = engine.clone();
    engine.spawn_task(Box::pin(async move {
        loop {
            tokio::select! {
                _ = tokio::time::sleep(CHECK_EVERY) => {}
                _ = WAKE.notified() => {}
            }
            check(&e, &data).await;
        }
    }));
}

async fn check(engine: &Arc<crate::engine::Engine>, data: &Path) {
    let Some(link) = crate::link_handle() else { return };
    if !link.enabled() {
        return;
    }
    let mut peers: Vec<(String, Build)> =
        link.status().peers.into_iter().filter(|p| p.connected).filter_map(|p| Some((p.name, p.build?))).collect();
    let this = Build::this();
    // Builds announced by computers that are installing them (and may be restarting right now).
    ANNOUNCED.lock().unwrap().retain(|(_, b)| b.newer_than(&this));
    peers.extend(ANNOUNCED.lock().unwrap().iter().cloned());
    let staged = crate::update::staged_build(data).filter(|s| s.newer_than(&this));
    let Some((from, build)) = target(&this, staged.as_ref(), &peers) else {
        // Nothing to do, but keep showing a staged build until it is installed.
        if staged.is_none() && status().state == "staged" {
            *STATUS.lock().unwrap() = None;
        }
        return;
    };
    if FAILED.lock().unwrap().as_ref().is_some_and(|(b, at)| *b == build && crate::store::now_ms() < at + RETRY_AFTER_MS) {
        return;
    }
    tell(engine, &format!("{from} runs Consilium {}, newer than this computer's {}. Preparing the same build here so the computers stay in step; it installs like any update, and the AIs carry on after the restart.", build.label(), this.label()));
    set("preparing", &build, &from, "Getting the code");
    match prepare(data, &build, &from).await {
        Ok(true) => {
            set("staged", &build, &from, "Ready to install");
            tell(engine, &format!("Consilium {} is ready to install on this computer, matching {from}.", build.label()));
        }
        Ok(false) => set("waiting", &build, &from, "That commit isn't on GitHub yet; trying again in a minute"),
        Err(why) => {
            *FAILED.lock().unwrap() = Some((build.clone(), crate::store::now_ms()));
            set("failed", &build, &from, why.clone());
            tell(engine, &format!("Couldn't prepare Consilium {} to match {from}: {why}. Trying again in 30 minutes; see update/follow.log in Consilium's data folder.", build.label()));
        }
    }
}

/// A note in the newest conversation of this computer's, for the human and the AIs.
fn tell(engine: &Arc<crate::engine::Engine>, text: &str) {
    if let Some(c) = engine.store.list().into_iter().find(|c| !crate::peer_sync::is_mirror(&c.id)) {
        let _ = engine.note(&c.id, text);
    }
}

/// Fetches `build`'s commit and stages it. Ok(false) if GitHub doesn't have that commit yet.
async fn prepare(data: &Path, build: &Build, from: &str) -> Result<bool, String> {
    let src = data.join("update").join("source");
    let log = data.join("update").join("follow.log");
    let _ = std::fs::create_dir_all(data.join("update"));
    append(&log, &format!("\n== {} preparing {} to match {from}\n", crate::store::now_ms(), build.label()));
    if !src.join(".git").exists() {
        let _ = std::fs::remove_dir_all(&src);
        let dest = src.to_string_lossy().to_string();
        run(&log, None, "git", &["clone", "--no-checkout", "--quiet", REPO_URL, &dest], STEP_LIMIT).await?;
    }
    run(&log, Some(&src), "git", &["fetch", "--quiet", "--force", "origin", "+refs/heads/*:refs/remotes/origin/*"], STEP_LIMIT).await?;
    let wanted = format!("{}^{{commit}}", build.commit);
    if run(&log, Some(&src), "git", &["cat-file", "-e", &wanted], STEP_LIMIT).await.is_err() {
        return Ok(false);
    }
    run(&log, Some(&src), "git", &["checkout", "--quiet", "--force", "--detach", &build.commit], STEP_LIMIT).await?;
    set("preparing", build, from, "Installing build tools' packages");
    run(&log, Some(&src), "npm", &["ci", "--no-audit", "--no-fund"], STEP_LIMIT).await?;
    set("preparing", build, from, "Building (this takes several minutes)");
    run(&log, Some(&src), "node", &["scripts/stage-update.mjs"], BUILD_LIMIT).await?;
    match crate::update::staged_build(data) {
        Some(s) if s == *build => Ok(true),
        Some(s) => Err(format!("the staged build is {}, not {}", s.label(), build.label())),
        None => Err("the build finished but nothing was staged".into()),
    }
}

fn append(log: &Path, text: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log) {
        let _ = f.write_all(text.as_bytes());
    }
}

/// Runs `program args` in `dir`, with its output added to `log`. Builds go into the app's own
/// folder, and the human's usual tool folders are on the PATH.
async fn run(log: &Path, dir: Option<&Path>, program: &str, args: &[&str], limit: Duration) -> Result<(), String> {
    let data = log.parent().and_then(Path::parent).map(Path::to_path_buf).unwrap_or_default();
    let mut cmd = if cfg!(windows) && program == "npm" {
        // npm is a batch file on Windows.
        let mut c = tokio::process::Command::new("cmd");
        c.arg("/C").arg("npm");
        c
    } else {
        tokio::process::Command::new(program)
    };
    cmd.args(args).stdin(std::process::Stdio::null()).kill_on_drop(true);
    if let Some(d) = dir {
        cmd.current_dir(d);
    }
    if let Some(home) = dirs::home_dir() {
        let mut path = vec![home.join(".cargo").join("bin"), home.join(".local").join("node").join("bin")];
        path.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()));
        if let Ok(p) = std::env::join_paths(path) {
            cmd.env("PATH", p);
        }
    }
    cmd.env("CONSILIUM_DATA", &data).env("CARGO_TARGET_DIR", data.join("update").join("target"));
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let out = tokio::time::timeout(limit, cmd.output())
        .await
        .map_err(|_| format!("`{program} {}` took too long", args.join(" ")))?
        .map_err(|e| format!("couldn't run {program} ({e}); it's needed to build updates on this computer"))?;
    append(log, &String::from_utf8_lossy(&out.stdout));
    append(log, &String::from_utf8_lossy(&out.stderr));
    if out.status.success() {
        Ok(())
    } else {
        let last = String::from_utf8_lossy(&out.stderr).lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim().chars().take(200).collect::<String>();
        Err(format!("`{program} {}` failed{}", args.first().unwrap_or(&""), if last.is_empty() { String::new() } else { format!(": {last}") }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(number: u64, commit: &str) -> Build {
        Build { version: "0.3.37".into(), number, commit: commit.into() }
    }

    #[test]
    fn follows_only_the_newest_rebuildable_build_and_never_goes_back() {
        let this = b(790, "aaaaaaaaaaaa");
        let peers = vec![
            ("MacBook".to_string(), b(812, "bbbbbbbbbbbb")),
            ("Laptop".to_string(), b(820, "cccccccccccc+dirty")),
            ("Old PC".to_string(), b(700, "dddddddddddd")),
        ];
        assert_eq!(target(&this, None, &peers), Some(("MacBook".to_string(), b(812, "bbbbbbbbbbbb"))), "uncommitted builds aren't followed");
        assert_eq!(target(&b(812, "bbbbbbbbbbbb"), None, &peers), None, "already there");
        assert_eq!(target(&b(900, "eeeeeeeeeeee"), None, &peers), None, "never back to an older build");
        assert_eq!(target(&this, Some(&b(812, "bbbbbbbbbbbb")), &peers), None, "already staged");
        assert_eq!(target(&this, Some(&b(800, "ffffffffffff")), &peers).map(|t| t.1.number), Some(812), "a newer one replaces an older staged build");
        let two = vec![("A".to_string(), b(812, "bbbbbbbbbbbb")), ("B".to_string(), b(830, "cccccccccccc"))];
        assert_eq!(target(&this, None, &two).map(|t| t.0), Some("B".to_string()), "the newest of several");
    }

    #[test]
    fn status_starts_idle() {
        assert_eq!(status().state, "idle");
    }
}
