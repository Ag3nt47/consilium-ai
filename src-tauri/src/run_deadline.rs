//! CLI turns get an inactivity budget and a separate bound on total runtime.
//! Only recognized provider activity renews the budget; raw output does not.

use std::time::{Duration, Instant};

pub(super) struct RunDeadline {
    started: Instant,
    last_progress: Instant,
    idle_budget: Duration,
    total_budget: Duration,
    last_activity: Option<String>,
}

impl RunDeadline {
    pub(super) fn new(now: Instant, idle_budget: Duration) -> Self {
        Self {
            started: now,
            last_progress: now,
            idle_budget,
            total_budget: idle_budget.saturating_mul(4),
            last_activity: None,
        }
    }

    pub(super) fn progress(&mut self, now: Instant, label: &str) {
        self.last_progress = now;
        // Callers supply the activity label, never commands, reasoning or provider output.
        self.last_activity = Some(label.chars().take(80).collect());
    }

    pub(super) fn remaining(&self, now: Instant) -> Duration {
        self.idle_budget
            .saturating_sub(now.saturating_duration_since(self.last_progress))
            .min(
                self.total_budget
                    .saturating_sub(now.saturating_duration_since(self.started)),
            )
    }

    pub(super) fn message(&self, now: Instant) -> String {
        let reason = if now.saturating_duration_since(self.started) >= self.total_budget {
            format!(
                "no answer within {} seconds total runtime",
                self.total_budget.as_secs()
            )
        } else {
            format!("no activity for {} seconds", self.idle_budget.as_secs())
        };
        match &self.last_activity {
            Some(label) => format!("{reason}; last activity: {label}"),
            None => format!("{reason}; no provider activity received"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUDGET: Duration = Duration::from_secs(900);

    #[test]
    fn active_work_can_cross_the_old_fifteen_minute_deadline() {
        let start = Instant::now();
        let mut deadline = RunDeadline::new(start, BUDGET);
        deadline.progress(start + Duration::from_secs(720), "Checking tool result");
        assert_eq!(deadline.remaining(start + BUDGET), Duration::from_secs(720));
    }

    #[test]
    fn silent_startup_still_times_out() {
        let start = Instant::now();
        let deadline = RunDeadline::new(start, BUDGET);
        assert_eq!(deadline.remaining(start + BUDGET), Duration::ZERO);
        assert_eq!(
            deadline.message(start + BUDGET),
            "no activity for 900 seconds; no provider activity received"
        );
    }

    #[test]
    fn a_stalled_tool_expires_after_its_last_progress() {
        let start = Instant::now();
        let mut deadline = RunDeadline::new(start, BUDGET);
        deadline.progress(start + Duration::from_secs(720), "Running command");
        let expired = start + Duration::from_secs(1620);
        assert_eq!(deadline.remaining(expired), Duration::ZERO);
        assert_eq!(
            deadline.message(expired),
            "no activity for 900 seconds; last activity: Running command"
        );
    }

    #[test]
    fn repeated_activity_cannot_exceed_the_total_runtime_cap() {
        let start = Instant::now();
        let mut deadline = RunDeadline::new(start, BUDGET);
        deadline.progress(start + Duration::from_secs(3500), "Checking tool result");
        assert_eq!(
            deadline.remaining(start + Duration::from_secs(3599)),
            Duration::from_secs(1)
        );
        let expired = start + Duration::from_secs(3600);
        assert_eq!(deadline.remaining(expired), Duration::ZERO);
        assert_eq!(
            deadline.message(expired),
            "no answer within 3600 seconds total runtime; last activity: Checking tool result"
        );
    }
}
