//! claude-resets.com's record of Claude Code usage-limit resets announced by Anthropic
//! (@ClaudeDevs and Claude Code team members). Unlike codex-reset.com it publishes history, not a
//! forecast, so Consilium shows that history and makes no guess at the odds of the next reset.
//! The data is CC BY 4.0; wherever it's shown, claude-resets.com is credited.

use crate::agents;
use crate::usage::when;
use serde::{Deserialize, Serialize};

/// The same pace as the Codex forecast, far below what one small static file needs.
pub const EVERY_MS: u64 = 15 * 60_000;
const SUMMARY_URL: &str = "https://claude-resets.com/data/summary.json";
pub const SITE: &str = "https://claude-resets.com/";

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ClaudeResets {
    /// The last announced reset (ms since the epoch).
    pub last_reset_at: Option<u64>,
    pub last_reset_scope: Option<String>,
    pub last_reset_url: Option<String>,
    pub reset_count: Option<u32>,
    pub mean_gap_days: Option<f64>,
    #[serde(default)]
    pub banked: Vec<BankedReset>,
    pub fetched_at: u64,
}

/// A reset users apply themselves, before `usable_until` if the announcement set a deadline.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct BankedReset {
    pub date: Option<u64>,
    pub scope: Option<String>,
    pub usable_until: Option<u64>,
}

/// Reads the site's documented machine summary with the system's curl, like the Codex forecast.
pub async fn fetch() -> Option<ClaudeResets> {
    let ua = format!("Consilium/{} (+https://github.com/Ag3nt47/consilium-ai)", env!("CARGO_PKG_VERSION"));
    let mut cmd = tokio::process::Command::new("curl");
    cmd.args(["-fsS", "--max-time", "10", "-A", &ua, SUMMARY_URL]).stdin(std::process::Stdio::null()).kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let out = tokio::time::timeout(std::time::Duration::from_secs(15), cmd.output()).await.ok()?.ok()?;
    if !out.status.success() {
        return None;
    }
    parse(&String::from_utf8_lossy(&out.stdout), crate::store::now_ms())
}

/// Only the fields `llms.txt` documents for `/data/summary.json`.
pub fn parse(json: &str, now: u64) -> Option<ClaudeResets> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    if !v.is_object() {
        return None;
    }
    let text = |x: &serde_json::Value| x.as_str().map(str::to_string);
    let time = |x: &serde_json::Value| x.as_str().and_then(agents::iso_ms);
    let banked = v["bankedResets"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|b| BankedReset { date: time(&b["date"]), scope: text(&b["scope"]), usable_until: time(&b["usableUntil"]) })
                .collect()
        })
        .unwrap_or_default();
    Some(ClaudeResets {
        last_reset_at: time(&v["lastResetAt"]),
        last_reset_scope: text(&v["lastResetScope"]),
        last_reset_url: text(&v["lastResetUrl"]),
        reset_count: v["resetCount"].as_u64().and_then(|n| u32::try_from(n).ok()),
        mean_gap_days: v["meanGapDays"].as_f64(),
        banked,
        fetched_at: now,
    })
}

/// Banked resets whose stated deadline hasn't passed. Ones with no stated deadline are left out,
/// since the site says it can't tell whether they still apply.
pub fn usable_banked(r: &ClaudeResets, now: u64) -> Vec<&BankedReset> {
    r.banked.iter().filter(|b| b.usable_until.is_some_and(|u| u > now)).collect()
}

/// "Tue 2026-11-10 21:59 UTC", without the relative part.
fn date(ms: u64) -> String {
    when(ms, ms).trim_end_matches(" (already passed)").to_string()
}

/// The line added to the council prompt under the Codex forecast.
pub fn prompt_line(r: &ClaudeResets, now: u64) -> String {
    let mut line = format!("\nClaude reset history (unofficial, {SITE}, CC BY 4.0): ");
    match r.last_reset_at {
        Some(t) => {
            let days = now.saturating_sub(t) as f64 / 86_400_000.0;
            line.push_str(&format!("last announced reset {} ({days:.1} days ago", date(t)));
            if let Some(scope) = &r.last_reset_scope {
                line.push_str(&format!(", {scope}"));
            }
            line.push(')');
        }
        None => line.push_str("no reset recorded"),
    }
    if let Some(gap) = r.mean_gap_days {
        line.push_str(&format!("; average gap {gap:.1} days"));
        if let Some(n) = r.reset_count {
            line.push_str(&format!(" over {n} resets"));
        }
    }
    let live = usable_banked(r, now);
    if !live.is_empty() {
        let plural = if live.len() == 1 { "" } else { "s" };
        line.push_str(&format!("; {} banked reset{plural} still usable", live.len()));
        if let Some(u) = live.iter().filter_map(|b| b.usable_until).min() {
            line.push_str(&format!(", soonest deadline {}", when(u, now)));
        }
    }
    line.push_str(". History only, not a forecast.");
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{"lastResetAt":"2026-11-10T21:59:58Z","lastResetScope":"Pro, Max, Ultra + Team","resetCount":16,"meanGapDays":13.9,"bankedResets":[{"date":"2026-09-22T16:44:06Z","scope":"Pro, Max + Team","usableUntil":"2026-10-22T23:59:59Z"},{"date":"2026-11-09T19:21:45Z","scope":"paid plans","usableUntil":null},{"date":"2026-11-10T21:59:58Z","scope":"Pro, Max, Ultra + Team","usableUntil":"2026-12-10T22:00:00Z"}]}"#;

    fn ms(s: &str) -> u64 {
        agents::iso_ms(s).expect("valid ISO time")
    }

    #[test]
    fn parses_summary() {
        let r = parse(SAMPLE, 1).unwrap();
        assert_eq!(r.reset_count, Some(16));
        assert_eq!(r.last_reset_at, Some(ms("2026-11-10T21:59:58Z")));
        assert_eq!(r.banked.len(), 3);
        assert_eq!(r.banked[1].usable_until, None);
    }

    #[test]
    fn line_shows_history_and_live_banked_only() {
        let now = ms("2026-11-17T20:00:00Z");
        let r = parse(SAMPLE, now).unwrap();
        assert_eq!(usable_banked(&r, now).len(), 1);
        let line = prompt_line(&r, now);
        assert!(line.contains("2026-11-10 21:59 UTC (6.9 days ago"), "{line}");
        assert!(line.contains("average gap 13.9 days over 16 resets"), "{line}");
        assert!(line.contains("1 banked reset still usable"), "{line}");
        assert!(!line.contains('%'), "no made-up odds: {line}");
    }

    #[test]
    fn expired_banked_resets_are_dropped() {
        let now = ms("2027-01-01T00:00:00Z");
        let r = parse(SAMPLE, now).unwrap();
        assert!(!prompt_line(&r, now).contains("banked"));
    }

    #[test]
    fn missing_or_bad_data() {
        let r = parse("{}", 5).unwrap();
        assert!(prompt_line(&r, 5).contains("no reset recorded"));
        assert!(parse("not json", 5).is_none());
        assert!(parse("[]", 5).is_none());
    }
}