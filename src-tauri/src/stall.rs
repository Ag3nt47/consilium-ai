//! Freeze guard. If the app stops making progress (the turn table's lock stays taken, or the
//! async runtime stops running tasks), messages the human sends are never saved and no AI answers,
//! on this computer and, through the link, on the others. The guard notices within about a minute,
//! logs it to `stall.log` in the data folder, and restarts Consilium; the open turn resumes from
//! its journal as after any restart. At most one restart per `MIN_GAP_MS`, so a freeze that comes
//! straight back can't loop.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

const CHECK: Duration = Duration::from_secs(15);
/// Checks in a row that must find the app stuck before it is restarted (about a minute).
const STRIKES: u32 = 4;
/// The runtime heartbeat is stale after this long.
const BEAT_STALE_MS: u64 = 60_000;
const MIN_GAP_MS: u64 = 10 * 60_000;

/// What one check found.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Probe {
    pub lock_free: bool,
    pub beat_age_ms: u64,
}

impl Probe {
    pub fn stuck(&self) -> Option<&'static str> {
        if !self.lock_free {
            Some("the turn table's lock stayed taken")
        } else if self.beat_age_ms > BEAT_STALE_MS {
            Some("the background runtime stopped running tasks")
        } else {
            None
        }
    }
}

/// Counts consecutive stuck checks; says when to restart and why.
#[derive(Default)]
pub struct Judge {
    strikes: u32,
}

impl Judge {
    pub fn check(&mut self, p: Probe) -> Option<&'static str> {
        match p.stuck() {
            Some(why) => {
                self.strikes += 1;
                (self.strikes >= STRIKES).then_some(why)
            }
            None => {
                self.strikes = 0;
                None
            }
        }
    }
}

fn last_restart_path(data: &Path) -> PathBuf {
    data.join("stall-restart.txt")
}

/// Whether a restart is allowed now (none in the last `MIN_GAP_MS`).
pub fn may_restart(data: &Path, now: u64) -> bool {
    let last = std::fs::read_to_string(last_restart_path(data)).ok().and_then(|s| s.trim().parse::<u64>().ok()).unwrap_or(0);
    now.saturating_sub(last) >= MIN_GAP_MS
}

fn log(data: &Path, line: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(data.join("stall.log")) {
        let _ = writeln!(f, "{} {line}", crate::store::now_ms());
    }
}

/// Starts the guard: a runtime heartbeat task and a plain thread that checks it and the lock.
pub fn start(app: tauri::AppHandle, engine: Arc<crate::engine::Engine>, data: PathBuf) {
    let beat = Arc::new(AtomicU64::new(crate::store::now_ms()));
    let b = beat.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            b.store(crate::store::now_ms(), Ordering::Relaxed);
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    });
    std::thread::spawn(move || {
        let mut judge = Judge::default();
        loop {
            std::thread::sleep(CHECK);
            let now = crate::store::now_ms();
            let probe = Probe { lock_free: engine.turns_lock_free(), beat_age_ms: now.saturating_sub(beat.load(Ordering::Relaxed)) };
            let Some(why) = judge.check(probe) else { continue };
            if !may_restart(&data, now) {
                log(&data, &format!("stuck ({why}); restarted less than 10 min ago, so not again"));
                judge = Judge::default();
                continue;
            }
            log(&data, &format!("stuck ({why}) for about a minute; restarting Consilium"));
            let _ = std::fs::write(last_restart_path(&data), now.to_string());
            app.restart();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restarts_only_after_a_minute_of_being_stuck_and_not_twice_in_ten_minutes() {
        let ok = Probe { lock_free: true, beat_age_ms: 5_000 };
        let locked = Probe { lock_free: false, beat_age_ms: 5_000 };
        let stale = Probe { lock_free: true, beat_age_ms: 90_000 };
        let mut j = Judge::default();
        for _ in 0..STRIKES - 1 {
            assert_eq!(j.check(locked), None);
        }
        assert_eq!(j.check(ok), None, "a free check resets the count");
        for _ in 0..STRIKES - 1 {
            assert_eq!(j.check(stale), None);
        }
        assert_eq!(j.check(stale), Some("the background runtime stopped running tasks"));

        let d = std::env::temp_dir().join(format!("consilium-stall-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        assert!(may_restart(&d, 1_000_000));
        std::fs::write(last_restart_path(&d), "1000000").unwrap();
        assert!(!may_restart(&d, 1_000_000 + MIN_GAP_MS - 1));
        assert!(may_restart(&d, 1_000_000 + MIN_GAP_MS));
        let _ = std::fs::remove_dir_all(d);
    }
}
