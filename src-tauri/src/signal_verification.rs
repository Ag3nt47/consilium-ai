//! Process-local identities of phone replies consumed by setup, including buffered copies.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

// A bounded listener queue may drain slowly after a transport stall. Exact event identities
// remain safe to suppress without retaining the short code or unrelated message text.
const RETENTION: Duration = Duration::from_secs(60 * 60);
const MAX_RECENT: usize = 64;

struct Reply {
    group: String,
    timestamp: u64,
    received: Instant,
}

pub(super) struct RecentReplies {
    replies: VecDeque<Reply>,
}

impl RecentReplies {
    pub(super) const fn new() -> Self {
        Self { replies: VecDeque::new() }
    }

    fn expire(&mut self, now: Instant) {
        self.replies.retain(|reply| now.checked_duration_since(reply.received).is_some_and(|age| age < RETENTION));
    }

    pub(super) fn remember(&mut self, group: &str, timestamp: u64, now: Instant) {
        self.expire(now);
        if group.is_empty() || timestamp == 0 || self.replies.iter().any(|reply| reply.group == group && reply.timestamp == timestamp) {
            return;
        }
        while self.replies.len() >= MAX_RECENT {
            self.replies.pop_front();
        }
        self.replies.push_back(Reply { group: group.into(), timestamp, received: now });
    }

    pub(super) fn matches(&mut self, group: &str, timestamp: u64, text: &str, active: Option<(&str, &str)>, now: Instant) -> bool {
        self.expire(now);
        active.is_some_and(|(challenge_group, code)| !challenge_group.is_empty() && group == challenge_group && text == code)
            || (timestamp != 0 && self.replies.iter().any(|reply| reply.group == group && reply.timestamp == timestamp))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffered_reply_is_consumed_after_setup_clears_its_code() {
        let now = Instant::now();
        let mut recent = RecentReplies::new();
        assert!(recent.matches("MAIN=", 42, "4821", Some(("MAIN=", "4821")), now));
        // The verifier sees the owner's reply before changing its phase to verified.
        recent.remember("MAIN=", 42, now);
        // The listener only drains its buffered copy after phase/code have cleared.
        assert!(recent.matches("MAIN=", 42, "4821", None, now + Duration::from_secs(120)));
        assert!(!recent.matches("MAIN=", 43, "4821", None, now + Duration::from_secs(120)), "a later numeric prompt is unrelated");
        assert!(!recent.matches("PRIVATE=", 42, "4821", None, now + Duration::from_secs(120)));
    }

    #[test]
    fn active_filter_is_exact_and_missing_event_identity_is_never_remembered() {
        let now = Instant::now();
        let mut recent = RecentReplies::new();
        recent.remember("MAIN=", 0, now);
        recent.remember("", 42, now);
        assert!(recent.replies.is_empty());
        assert!(recent.matches("MAIN=", 0, "4821", Some(("MAIN=", "4821")), now));
        assert!(!recent.matches("MAIN=", 0, "4821", None, now));
        assert!(!recent.matches("MAIN=", 42, "4821 extra", Some(("MAIN=", "4821")), now));
        assert!(!recent.matches("PRIVATE=", 42, "4821", Some(("MAIN=", "4821")), now));
    }

    #[test]
    fn retention_is_monotonic_and_duplicate_copies_do_not_extend_it() {
        let now = Instant::now();
        let mut recent = RecentReplies::new();
        recent.remember("MAIN=", 42, now);
        recent.remember("MAIN=", 42, now + RETENTION / 2);
        assert_eq!(recent.replies.len(), 1);
        assert!(recent.matches("MAIN=", 42, "4821", None, now + RETENTION - Duration::from_nanos(1)));
        assert!(!recent.matches("MAIN=", 42, "4821", None, now + RETENTION));
        recent.remember("MAIN=", 43, now);
        assert!(!recent.matches("MAIN=", 43, "4821", None, now - Duration::from_secs(1)));
    }

    #[test]
    fn a_bounded_cache_keeps_the_latest_event_identities() {
        let now = Instant::now();
        let mut recent = RecentReplies::new();
        for timestamp in 1..=MAX_RECENT as u64 + 1 {
            recent.remember("MAIN=", timestamp, now);
        }
        assert_eq!(recent.replies.len(), MAX_RECENT);
        assert!(!recent.matches("MAIN=", 1, "4821", None, now));
        assert!(recent.matches("MAIN=", MAX_RECENT as u64 + 1, "4821", None, now));
    }
}
