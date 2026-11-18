//! AIs that have run out of usage (a plan's message allowance, a rate limit, or a quota).
//!
//! When an AI's CLI reports a usage limit, that AI is "benched" until its limit resets: the
//! council carries on without it, its turns go to the other members, and it isn't asked again
//! until the reset time (when the CLI says it) or a default cooldown has passed. A successful
//! answer, or Retry in the UI, takes it off the bench early.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;

/// How long an AI sits out when its CLI doesn't say when the limit resets.
pub const DEFAULT_COOLDOWN_MS: u64 = 30 * 60 * 1000;
/// Reset times further away than this are capped, so a misread number can't bench an AI for good.
const MAX_COOLDOWN_MS: u64 = 7 * 24 * 60 * 60 * 1000;

#[derive(Default)]
pub struct Bench {
    /// Agent id → time (ms since the epoch) its usage limit resets.
    until: Mutex<HashMap<String, u64>>,
    /// Agents whose reset time came from the AI itself (its CLI's error or its usage reading),
    /// not Consilium's default retry guess. Only these show a "back at" time.
    known: Mutex<HashSet<String>>,
    /// Consilium's data folder, whose usage.json says when each AI's plan limits reset.
    data: Option<PathBuf>,
}

impl Bench {
    #[cfg(test)]
    pub fn new() -> Self {
        Self::default()
    }

    /// A bench that also reads each AI's own usage reading, so an AI whose plan is used up until
    /// a later reset (a weekly limit) isn't shown as back after the short default cooldown.
    pub fn with_usage(data: PathBuf) -> Self {
        Self { data: Some(data), ..Self::default() }
    }

    /// Bench `agent` until `until_ms`. `reported` says the time came from the AI's own error
    /// message. Returns the reset time if it is verified, None if it is only Consilium's guess.
    pub fn bench(&self, agent: &str, until_ms: u64, reported: bool) -> Option<u64> {
        let now = crate::store::now_ms();
        let later = self.data.as_ref().and_then(|d| exhausted_until(&crate::usage::load(d), agent, now));
        let until = later.map_or(until_ms, |t| t.max(until_ms));
        self.until.lock().unwrap().insert(agent.to_string(), until);
        let known = reported || later.is_some();
        let mut set = self.known.lock().unwrap();
        if known {
            set.insert(agent.to_string());
        } else {
            set.remove(agent);
        }
        known.then_some(until)
    }

    /// Whether the agent's reset time is verified by the AI itself.
    pub fn reset_known(&self, agent: &str) -> bool {
        self.known.lock().unwrap().contains(agent)
    }

    /// After a new usage reading: an AI already sitting out whose plan is used up until later
    /// stays out until then; an AI whose quota has reached 0% is benched; one whose reading
    /// shows usage left again is let back in.
    pub fn update_from(&self, snapshot: &crate::usage::Snapshot, now: u64) {
        let mut map = self.until.lock().unwrap();
        for (agent, u) in &snapshot.agents {
            match exhausted_until(snapshot, agent, now) {
                Some(t) => {
                    let entry = map.entry(agent.clone()).or_insert(t);
                    *entry = (*entry).max(t);
                    self.known.lock().unwrap().insert(agent.clone());
                }
                None if plan_meter(u).0 > 0 => {
                    if let Some(until) = map.get_mut(agent) {
                        if *until > now + DEFAULT_COOLDOWN_MS {
                            *until = now;
                        }
                    }
                }
                None => {}
            }
        }
    }

    /// When the agent's limit resets, if it is still benched at `now`.
    pub fn until(&self, agent: &str, now: u64) -> Option<u64> {
        let mut map = self.until.lock().unwrap();
        match map.get(agent) {
            Some(&t) if t > now => Some(t),
            Some(_) => {
                map.remove(agent);
                None
            }
            None => {
                if let Some(data) = &self.data {
                    if let Some(t) = exhausted_until(&crate::usage::load(data), agent, now) {
                        map.insert(agent.to_string(), t);
                        self.known.lock().unwrap().insert(agent.to_string());
                        return Some(t);
                    }
                }
                None
            }
        }
    }

    pub fn clear(&self, agent: &str) {
        self.until.lock().unwrap().remove(agent);
        self.known.lock().unwrap().remove(agent);
    }
}

/// The meter that decides whether an AI can answer at all: its whole plan when the CLI reports
/// one (a single model being used up doesn't stop it), otherwise the lowest limit.
fn plan_meter(u: &crate::agents::UsageStatus) -> (u8, &[crate::agents::UsageWindow]) {
    match &u.overall {
        Some(o) => (o.remaining_percent, &o.windows),
        None => (u.remaining_percent, &u.windows),
    }
}

/// When an AI whose usage reading shows its plan used up (0% left) gets usage back: the latest
/// reset among its used-up limits, capped at a week. None if it has usage left or no reset is known.
pub fn exhausted_until(s: &crate::usage::Snapshot, agent: &str, now: u64) -> Option<u64> {
    let (left, windows) = plan_meter(s.agents.get(agent)?);
    if left > 0 {
        return None;
    }
    windows
        .iter()
        .filter(|w| w.remaining_percent == 0)
        .filter_map(|w| w.resets_at)
        .filter(|&t| t > now)
        .max()
        .map(|t| t.min(now + MAX_COOLDOWN_MS))
}

/// If `error` says the AI is out of usage, when it resets (ms since the epoch); otherwise None.
pub fn usage_limit(error: &str, now: u64) -> Option<u64> {
    let lower = error.to_lowercase();
    const SIGNS: [&str; 16] = [
        "usage limit",
        "rate limit",
        "rate_limit",
        "ratelimit",
        "rate-limit",
        "quota",
        "resource_exhausted",
        "resource exhausted",
        "too many requests",
        "hit your limit",
        "limit reached",
        "limit exceeded",
        "out of credits",
        "credit balance is too low",
        "insufficient_quota",
        "weekly limit",
    ];
    if !SIGNS.iter().any(|s| lower.contains(s)) && !has_status_429(&lower) {
        return None;
    }
    let wait = reset_in_ms(&lower, now).unwrap_or(DEFAULT_COOLDOWN_MS).clamp(60_000, MAX_COOLDOWN_MS);
    Some(now + wait)
}

/// Whether `error` itself says when the limit resets (rather than Consilium guessing).
pub fn reset_reported(error: &str, now: u64) -> bool {
    reset_in_ms(&error.to_lowercase(), now).is_some()
}

/// "429" on its own (not inside a longer number), as in "HTTP 429" or "status: 429".
fn has_status_429(s: &str) -> bool {
    let b = s.as_bytes();
    s.match_indices("429").any(|(i, _)| {
        let before = i == 0 || !b[i - 1].is_ascii_digit();
        let after = i + 3 >= b.len() || !b[i + 3].is_ascii_digit();
        before && after
    })
}

/// How long until the limit resets, from the forms the CLIs use: Claude Code's
/// "…usage limit reached|<unix seconds>", or "in 2 hours", "in 45 minutes", "retry after 30 seconds".
fn reset_in_ms(s: &str, now: u64) -> Option<u64> {
    if let Some((_, rest)) = s.rsplit_once('|') {
        let digits: String = rest.trim().chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(secs) = digits.parse::<u64>() {
            let at = if digits.len() >= 13 { secs } else { secs * 1000 };
            if at > now {
                return Some(at - now);
            }
        }
    }
    let words: Vec<&str> = s.split(|c: char| c.is_whitespace() || c == ',' || c == '.' || c == '(' || c == ')').filter(|w| !w.is_empty()).collect();
    let mut total = 0u64;
    for (i, w) in words.iter().enumerate() {
        let (num, unit) = match w.find(|c: char| !c.is_ascii_digit()) {
            Some(0) => continue,
            Some(p) => (&w[..p], &w[p..]),
            None => (*w, words.get(i + 1).copied().unwrap_or("")),
        };
        let Ok(n) = num.parse::<u64>() else { continue };
        let unit_ms = if unit.starts_with("h") {
            3_600_000
        } else if unit.starts_with("min") || unit == "m" {
            60_000
        } else if unit.starts_with("s") {
            1_000
        } else if unit.starts_with("d") {
            86_400_000
        } else {
            continue;
        };
        total += n * unit_ms;
    }
    (total > 0).then_some(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000_000;

    #[test]
    fn recognises_each_clis_usage_limit_messages() {
        for e in [
            "Claude AI usage limit reached|1800003600",
            "You've hit your usage limit. Upgrade to Pro or try again in 2 hours.",
            "exit 1: ERROR: rate_limit_exceeded",
            "Quota exceeded for quota metric 'Generate Content API requests per minute'",
            "RESOURCE_EXHAUSTED (ERROR)",
            "HTTP 429 Too Many Requests",
            "status: 429",
            "5-hour limit reached ∙ resets 3pm",
        ] {
            assert!(usage_limit(e, NOW).is_some(), "{e}");
        }
    }

    #[test]
    fn ordinary_failures_are_not_usage_limits() {
        for e in ["Not logged in", "no answer within 900 seconds", "exit 1: error at line 1429 of file", "stopped"] {
            assert!(usage_limit(e, NOW).is_none(), "{e}");
        }
    }

    #[test]
    fn reset_time_is_read_when_the_cli_gives_one() {
        assert_eq!(usage_limit("Claude AI usage limit reached|1800003600", NOW), Some(1_800_003_600_000));
        assert_eq!(usage_limit("usage limit: try again in 2 hours", NOW), Some(NOW + 7_200_000));
        assert_eq!(usage_limit("rate limit, retry after 90 seconds", NOW), Some(NOW + 90_000));
        assert_eq!(usage_limit("rate limited; resets in 1h 30m", NOW), Some(NOW + 5_400_000));
        assert_eq!(usage_limit("quota exceeded", NOW), Some(NOW + DEFAULT_COOLDOWN_MS));
        // At least a minute, at most a week.
        assert_eq!(usage_limit("rate limit, retry after 2 seconds", NOW), Some(NOW + 60_000));
    }

    #[test]
    fn bench_expires_and_clears() {
        let b = Bench::new();
        b.bench("claude", NOW + 1000, true);
        assert_eq!(b.until("claude", NOW), Some(NOW + 1000));
        assert_eq!(b.until("claude", NOW + 1000), None, "expired");
        b.bench("gpt", NOW + 1000, true);
        b.clear("gpt");
        assert_eq!(b.until("gpt", NOW), None);
    }

    fn usage(overall_left: u8, weekly_left: u8, weekly_reset: u64) -> crate::usage::Snapshot {
        use crate::agents::{UsageMeter, UsageStatus, UsageWindow};
        let windows = vec![
            UsageWindow { label: "5-hour limit".into(), remaining_percent: 100, resets_at: Some(NOW + 1_800_000) },
            UsageWindow { label: "Weekly limit".into(), remaining_percent: weekly_left, resets_at: Some(weekly_reset) },
        ];
        let status = UsageStatus {
            remaining_percent: 0,
            windows: windows.clone(),
            model: None,
            overall: Some(UsageMeter { label: "ChatGPT".into(), remaining_percent: overall_left, windows, shared: false }),
        };
        let mut s = crate::usage::Snapshot::default();
        s.agents.insert("gpt".into(), status);
        s
    }

    #[test]
    fn a_used_up_weekly_limit_keeps_an_ai_out_until_it_resets_not_the_short_cooldown() {
        let friday = NOW + 3 * 86_400_000;
        assert_eq!(exhausted_until(&usage(0, 0, friday), "gpt", NOW), Some(friday));
        assert_eq!(exhausted_until(&usage(24, 24, friday), "gpt", NOW), None, "plan has usage left");
        assert_eq!(exhausted_until(&usage(0, 0, friday), "claude", NOW), None, "no reading");

        let b = Bench::new();
        assert_eq!(b.bench("gpt", NOW + DEFAULT_COOLDOWN_MS, false), None, "a guessed cooldown isn't a verified time");
        assert!(!b.reset_known("gpt"));
        b.update_from(&usage(0, 0, friday), NOW);
        assert_eq!(b.until("gpt", NOW), Some(friday));
        assert!(b.reset_known("gpt"), "the weekly reset comes from the AI's own reading");
        // Usage back early (an early reset): it's let back in.
        b.update_from(&usage(100, 100, friday), NOW);
        assert_eq!(b.until("gpt", NOW + 1), None);
    }
}
