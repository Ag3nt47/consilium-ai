//! Offline probes own their subprocess group; no user shell/profile is executed.
use super::*;
use std::sync::{Arc, mpsc};

fn folders(names: &[&str]) -> Vec<PathBuf> { names.iter().map(PathBuf::from).collect() }

fn contention(cached: bool) {
    let cache = Arc::new(PathCache::new());
    if cached {
        *cache.snapshot.lock().unwrap() = Some((Instant::now() - REFRESH, folders(&["prior-login-bin"])));
    }
    let (entered, started) = mpsc::channel();
    let (release, proceed) = mpsc::channel();
    let worker_cache = cache.clone();
    let worker = std::thread::spawn(move || worker_cache.dirs(folders(&["current-bin"]), folders(&["usual-bin"]), || {
        entered.send(()).unwrap();
        proceed.recv().unwrap();
        Some(folders(&["new-login-bin"]))
    }));
    started.recv_timeout(Duration::from_secs(2)).unwrap();
    let (finished, result) = mpsc::channel();
    let caller_cache = cache.clone();
    let caller = std::thread::spawn(move || {
        let paths = caller_cache.dirs(folders(&["current-bin"]), folders(&["usual-bin"]), || panic!("Only one probe may run"));
        finished.send(paths).unwrap();
    });
    // Record the result before releasing the probe. A regression unwinds both
    // threads after this timeout instead of leaving a hung test process.
    let immediate = result.recv_timeout(Duration::from_secs(2));
    release.send(()).unwrap();
    let refreshed = worker.join().unwrap();
    caller.join().unwrap();
    let expected = if cached { folders(&["current-bin", "prior-login-bin", "usual-bin"]) }
        else { folders(&["current-bin", "usual-bin"]) };
    assert_eq!(immediate.unwrap(), expected);
    assert_eq!(refreshed, folders(&["current-bin", "new-login-bin", "usual-bin"]));
    assert_eq!(cache.dirs(vec![], vec![], || panic!("Fresh snapshots must not reprobe")), refreshed);
}

#[test]
fn concurrent_first_lookup_uses_current_and_usual_folders_without_waiting_for_the_shell() { contention(false); }

#[test]
fn concurrent_stale_lookup_keeps_previous_login_folders_without_waiting_for_the_shell() { contention(true); }

#[test]
fn a_failed_refresh_keeps_prior_login_paths_and_does_not_poison_the_cache() {
    let cache = PathCache::new();
    *cache.snapshot.lock().unwrap() = Some((Instant::now() - REFRESH, folders(&["prior-login-bin"])));
    let paths = cache.dirs(folders(&["current-bin"]), folders(&["usual-bin"]), || None);
    assert_eq!(paths, folders(&["current-bin", "prior-login-bin", "usual-bin"]));
    assert_eq!(cache.dirs(vec![], vec![], || panic!("A failed probe is still throttled")), paths);
    assert!(!cache.refreshing.load(Ordering::Acquire));
}

#[test]
fn path_priority_is_current_then_saved_then_usual_with_no_duplicates() {
    assert_eq!(merge(folders(&["current", "shared"]), folders(&["saved", "shared"]), folders(&["usual", "saved"])),
        folders(&["current", "shared", "saved", "usual"]));
}

#[cfg(unix)]
fn shell(script: &str) -> std::process::Command {
    let mut command = std::process::Command::new("/bin/sh");
    command.args(["-c", script]);
    command
}

#[cfg(unix)]
#[test]
fn output_larger_than_a_pipe_buffer_is_drained_before_waiting_for_exit() {
    let output = path_output(shell("dd if=/dev/zero bs=1024 count=96"), Duration::from_secs(3), 128 * 1024).unwrap();
    assert_eq!(output.len(), 96 * 1024);
    assert!(output.iter().all(|byte| *byte == 0));
}

#[cfg(unix)]
#[test]
fn an_exited_shell_with_a_descendant_holding_stdout_cannot_escape_the_deadline() {
    let marker = std::env::temp_dir().join(format!("consilium-path-pipe-{}.pid", uuid::Uuid::new_v4()));
    let mut command = shell("sleep 30 & printf '%s' \"$!\" > \"$1\"; exit 0");
    command.arg("fixture").arg(&marker);
    let started = Instant::now();
    assert!(path_output(command, Duration::from_secs(2), 128 * 1024).is_none());
    assert!(started.elapsed() < Duration::from_secs(6));
    let pid: u32 = std::fs::read_to_string(&marker).unwrap().parse().unwrap();
    let _ = std::fs::remove_file(marker);
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let system = sysinfo::System::new_all();
        let running = system.process(sysinfo::Pid::from_u32(pid)).is_some_and(|process|
            !matches!(process.status(), sysinfo::ProcessStatus::Zombie | sysinfo::ProcessStatus::Dead));
        if !running { break; }
        assert!(Instant::now() < deadline, "The owned pipe-holding descendant survived cleanup");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(unix)]
#[test]
fn a_running_shell_and_unbounded_output_have_bounded_failure_paths() {
    let started = Instant::now();
    assert!(path_output(shell("sleep 30"), Duration::from_millis(100), 128 * 1024).is_none());
    assert!(started.elapsed() < Duration::from_secs(4));
    assert_eq!(path_output(shell("printf '123456789'"), Duration::from_secs(3), 9), Some(b"123456789".to_vec()));
    assert!(path_output(shell("printf '123456789'"), Duration::from_secs(3), 8).is_none());
    assert!(path_output(shell("while :; do printf 'xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx'; done"), Duration::from_secs(2), 1024).is_none());
}
