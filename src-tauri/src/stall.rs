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
/// A live PeerLink session normally receives an authenticated heartbeat every 1.5 seconds. Give
/// it ample room for sleep/wake and a busy machine before forcing a clean redial.
const SYNC_SESSION_STALE_MS: u64 = 15_000;
/// A packet waiting this long in the local filing worker means chats are durable but invisible.
/// Restarting recovers inbox.jsonl before acknowledging any new work to the user.
const SYNC_DELIVERY_STALE_MS: u64 = 60_000;
const MIN_GAP_MS: u64 = 10 * 60_000;
const HELPER_RETRY_MS: u64 = 60_000;

/// What one check found.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Probe {
    pub lock_issue: Option<&'static str>,
    pub beat_age_ms: u64,
}

impl Probe {
    pub fn stuck(&self) -> Option<&'static str> {
        if self.lock_issue.is_some() {
            self.lock_issue
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

fn sync_delivery_stuck(health: crate::link::DeliveryHealth) -> bool {
    health.pending > 0 && health.oldest_age_ms > SYNC_DELIVERY_STALE_MS
}

fn log(data: &Path, line: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(data.join("stall.log")) {
        let _ = writeln!(f, "{} {line}", crate::store::now_ms());
    }
}

fn recover(app: &tauri::AppHandle, data: &Path, now: u64, reason: &str) -> u64 {
    log(data, reason);
    match crate::restart::request(app, data, true) {
        Ok(()) => {
            let _ = std::fs::write(last_restart_path(data), now.to_string());
            0
        }
        Err(error) => {
            log(data, &format!("Consilium relaunch could not be scheduled: {error}"));
            crate::store::now_ms() + HELPER_RETRY_MS
        }
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
        let mut retry_after = 0;
        loop {
            std::thread::sleep(CHECK);
            let now = crate::store::now_ms();
            if let Some(link) = crate::link_handle().filter(|link| link.enabled_for_watchdog()) {
                let repaired = link.repair_stale_sessions(now, SYNC_SESSION_STALE_MS);
                if !repaired.is_empty() {
                    log(&data, &format!(
                        "peer sync stopped receiving authenticated traffic; redialing {}",
                        repaired.join(", ")
                    ));
                }
                let delivery = link.delivery_health(now);
                if sync_delivery_stuck(delivery) {
                    if now < retry_after {
                        continue;
                    }
                    if may_restart(&data, now) {
                        retry_after = recover(&app, &data, now, &format!(
                            "peer sync filing worker stalled with {} durable packet(s) for {} ms; restarting Consilium",
                            delivery.pending, delivery.oldest_age_ms
                        ));
                    } else {
                        log(&data, &format!(
                            "peer sync filing worker is stalled with {} durable packet(s), but Consilium restarted less than 10 min ago",
                            delivery.pending
                        ));
                    }
                    continue;
                }
            }
            let probe = Probe { lock_issue: engine.turn_lock_issue(), beat_age_ms: now.saturating_sub(beat.load(Ordering::Relaxed)) };
            let Some(why) = judge.check(probe) else { continue };
            if now < retry_after {
                continue;
            }
            if !may_restart(&data, now) {
                log(&data, &format!("stuck ({why}); restarted less than 10 min ago, so not again"));
                judge = Judge::default();
                continue;
            }
            retry_after = recover(&app, &data, now, &format!("stuck ({why}) for about a minute; restarting Consilium"));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restarts_only_after_a_minute_of_being_stuck_and_not_twice_in_ten_minutes() {
        let ok = Probe { lock_issue: None, beat_age_ms: 5_000 };
        let locked = Probe { lock_issue: Some("the turn table's lock stayed taken"), beat_age_ms: 5_000 };
        let stale = Probe { lock_issue: None, beat_age_ms: 90_000 };
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
        assert!(!sync_delivery_stuck(crate::link::DeliveryHealth {
            pending: 0, oldest_age_ms: SYNC_DELIVERY_STALE_MS + 1, last_completed_ms: 0,
        }));
        assert!(!sync_delivery_stuck(crate::link::DeliveryHealth {
            pending: 1, oldest_age_ms: SYNC_DELIVERY_STALE_MS, last_completed_ms: 0,
        }));
        assert!(sync_delivery_stuck(crate::link::DeliveryHealth {
            pending: 1, oldest_age_ms: SYNC_DELIVERY_STALE_MS + 1, last_completed_ms: 0,
        }));
        let _ = std::fs::remove_dir_all(d);
    }
}
