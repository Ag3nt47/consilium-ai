//! AIs that have run out of usage (a plan's message allowance, a rate limit, or a quota).
//!
//! When an AI's CLI reports a usage limit, that AI is "benched" until its limit resets: the
//! council carries on without it, its turns go to the other members, and it isn't asked again
//! until the reset time (when the CLI says it) or a default cooldown has passed. A successful
//! answer, or Retry in the UI, takes it off the bench early.

use std::collections::HashMap;
use std::sync::Mutex;

/// How long an AI sits out when its CLI doesn't say when the limit resets.
pub const DEFAULT_COOLDOWN_MS: u64 = 30 * 60 * 1000;
/// Reset times further away than this are capped, so a misread number can't bench an AI for good.
const MAX_COOLDOWN_MS: u64 = 7 * 24 * 60 * 60 * 1000;

#[derive(Default)]
pub struct Bench {
    /// Agent id → time (ms since the epoch) its usage limit resets.
    until: Mutex<HashMap<String, u64>>,
}

impl Bench {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn bench(&self, agent: &str, until_ms: u64) {
        self.until.lock().unwrap().insert(agent.to_string(), until_ms);
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
            None => None,
        }
    }

    pub fn clear(&self, agent: &str) {
        self.until.lock().unwrap().remove(agent);
    }
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
        b.bench("claude", NOW + 1000);
        assert_eq!(b.until("claude", NOW), Some(NOW + 1000));
        assert_eq!(b.until("claude", NOW + 1000), None, "expired");
        b.bench("gpt", NOW + 1000);
        b.clear("gpt");
        assert_eq!(b.until("gpt", NOW), None);
    }
}
