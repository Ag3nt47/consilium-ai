//! What each AI has left of its plan, and when its limits reset, shared with the AIs so they know
//! when they and the others will be able to answer.
//!
//! The latest reading is kept in `usage.json` in the data folder, so the app and the terminal
//! command share it. Each AI's numbers come from its own CLI (see `agents::usage_of`). With the
//! setting on, it also holds codex-reset.com's unofficial forecast of the next global Codex reset.

use crate::agents::{self, UsageStatus};
use crate::engine::Engine;
use crate::store::now_ms;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::future::Future;
use std::path::Path;

/// A reading older than this is refreshed before a turn (in the background).
pub const STALE_MS: u64 = 10 * 60_000;
/// codex-reset.com asks for at most one request a minute; this asks far less often.
const FORECAST_EVERY_MS: u64 = 15 * 60_000;
const FORECAST_URL: &str = "https://codex-reset.com/api/forecast?tz=UTC";
pub const FORECAST_SITE: &str = "https://codex-reset.com/";

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Snapshot {
    /// When the AIs' usage was read (ms since the epoch).
    pub at: u64,
    /// By AI id; AIs whose CLI doesn't report usage are absent.
    pub agents: HashMap<String, UsageStatus>,
    #[serde(default)]
    pub forecast: Option<Forecast>,
    /// claude-resets.com's history of Claude Code resets (no forecast), when the setting is on.
    #[serde(default)]
    pub claude_resets: Option<crate::claude_resets::ClaudeResets>,
}

/// codex-reset.com's forecast of the next global Codex usage reset (a goodwill reset OpenAI grants
/// every account). Unofficial and, by the site's own account, low-confidence.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Forecast {
    /// Chance of a global reset within 24 and 48 hours, in percent.
    pub chance_24h: Option<u32>,
    pub chance_48h: Option<u32>,
    pub confidence: Option<String>,
    /// The last verified global reset (ms since the epoch).
    pub last_reset_at: Option<u64>,
    /// An official announcement of an upcoming reset, if the site has one.
    pub official: Option<String>,
    pub fetched_at: u64,
}

/// Public forecasts of an unscheduled, provider-wide usage reset, by AI id, where one is known to
/// exist. Each AI's own reset times already come from its CLI; these are the extra "an early reset
/// may come" estimates. Only Codex (ChatGPT) has one so far, from codex-reset.com.
pub fn forecast_source(agent: &str) -> Option<&'static str> {
    match agent {
        "gpt" => Some(FORECAST_SITE),
        _ => None,
    }
}

/// The note posted when an AI joins the council. For an AI with no known reset forecast it asks
/// the council to check once on the web, so a forecast is used if one exists.
pub fn join_note(agent: &str, name: &str) -> String {
    match forecast_source(agent) {
        Some(site) => format!(
            "{name} joined the council. Its plan usage and reset times come from its own CLI, and {site} \
forecasts the next early, provider-wide reset."
        ),
        None => format!(
            "{name} joined the council. Its plan usage and reset times come from its own CLI where it reports \
them. One AI that can search the web, please run one quick search for a public forecast or tracker of \
early, provider-wide usage resets for {name} (like codex-reset.com is for Codex), and say what you find in \
one or two sentences. If a reliable one exists, save it with a REMEMBER: line naming the site; if not, say \
so plainly. The others don't need to search."
        ),
    }
}

pub fn load(data: &Path) -> Snapshot {
    std::fs::read_to_string(data.join("usage.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

fn save(data: &Path, s: &Snapshot) {
    let tmp = data.join("usage.json.tmp");
    if let Ok(bytes) = serde_json::to_vec_pretty(s) {
        if std::fs::write(&tmp, bytes).is_ok() {
            let _ = crate::store::replace_file(&tmp, &data.join("usage.json"));
        }
    }
}

/// Reads every council member's usage from its CLI (at the same time), and the forecast when it's
/// turned on and due, then saves and returns the reading.
pub async fn refresh(engine: &Engine) -> Snapshot {
    share_refresh(&engine.usage_refresh, sample(engine, false)).await
}

/// Concurrent callers share the reading produced by the in-flight caller. A canceled caller
/// leaves the slot empty so a waiter can retry. A later explicit request starts a fresh read.
async fn share_refresh(
    gate: &tokio::sync::Mutex<Option<Snapshot>>,
    sample: impl Future<Output = Snapshot>,
) -> Snapshot {
    let mut result = match gate.try_lock() {
        Ok(result) => result,
        Err(_) => {
            let result = gate.lock().await;
            if let Some(snapshot) = &*result {
                return snapshot.clone();
            }
            result
        }
    };
    *result = None;
    let snapshot = sample.await;
    *result = Some(snapshot.clone());
    snapshot
}

async fn sample(engine: &Engine, skip_if_fresh: bool) -> Snapshot {
    let before = load(&engine.data);
    if skip_if_fresh && now_ms().saturating_sub(before.at) < STALE_MS {
        return before;
    }
    let last = engine.last_models();
    let mut tasks = tokio::task::JoinSet::new();
    for agent in agents::all().into_iter().filter(|a| engine.config.is_member(a.id)) {
        let model = last.get(agent.id).cloned();
        tasks.spawn(async move { (agent.id.to_string(), agents::usage_of(agent, model.as_deref()).await) });
    }
    let mut snapshot = Snapshot { at: now_ms(), ..Snapshot::default() };
    while let Some(r) = tasks.join_next().await {
        if let Ok((id, Some(usage))) = r {
            snapshot.agents.insert(id, usage);
        }
    }
    snapshot.forecast = if engine.config.get().reset_forecast {
        match before.forecast {
            Some(f) if now_ms().saturating_sub(f.fetched_at) < FORECAST_EVERY_MS => Some(f),
            old => fetch_forecast().await.or(old),
        }
    } else {
        None
    };
    snapshot.claude_resets = if engine.config.get().reset_forecast {
        match before.claude_resets {
            Some(r) if now_ms().saturating_sub(r.fetched_at) < crate::claude_resets::EVERY_MS => Some(r),
            old => crate::claude_resets::fetch().await.or(old),
        }
    } else {
        None
    };
    save(&engine.data, &snapshot);
    engine.bench.update_from(&snapshot, now_ms());
    snapshot
}

/// Refreshes in the background if the saved reading is old, so the next turn's AIs see fresh numbers.
pub fn refresh_if_stale(engine: &std::sync::Arc<Engine>) {
    if now_ms().saturating_sub(load(&engine.data).at) < STALE_MS {
        return;
    }
    let e = engine.clone();
    engine.spawn_task(Box::pin(async move {
        share_refresh(&e.usage_refresh, sample(&e, true)).await;
    }));
}

/// Asks codex-reset.com's documented forecast API, with the User-Agent its terms ask for (the
/// project and where to reach it). Uses the system's curl, so Consilium needs no HTTP library.
async fn fetch_forecast() -> Option<Forecast> {
    let ua = format!("Consilium/{} (+https://github.com/Ag3nt47/consilium-ai)", env!("CARGO_PKG_VERSION"));
    let mut cmd = tokio::process::Command::new("curl");
    cmd.args(["-fsS", "--max-time", "10", "-A", &ua, FORECAST_URL]).stdin(std::process::Stdio::null()).kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let out = tokio::time::timeout(std::time::Duration::from_secs(15), cmd.output()).await.ok()?.ok()?;
    if !out.status.success() {
        return None;
    }
    parse_forecast(&String::from_utf8_lossy(&out.stdout), now_ms())
}

/// Only the fields the site documents as stable.
fn parse_forecast(json: &str, now: u64) -> Option<Forecast> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let p = &v["probabilities"];
    let chance = |k: &str| p[k].as_u64().map(|n| n.min(100) as u32);
    let official = match &v["official_signal"] {
        serde_json::Value::Null => None,
        serde_json::Value::String(s) => Some(s.clone()),
        other => Some(other.to_string()),
    };
    Some(Forecast {
        chance_24h: chance("rounded_24h"),
        chance_48h: chance("rounded_48h"),
        confidence: v["confidence"].as_str().map(str::to_string),
        last_reset_at: v["last_reset_at"].as_str().and_then(agents::iso_ms),
        official,
        fetched_at: now,
    })
}

/// "Tue 2026-11-17 19:00 UTC (in 2 d 3 h)".
pub fn when(ms: u64, now: u64) -> String {
    let secs = ms / 1000;
    let days = (secs / 86_400) as i64;
    let (h, m) = ((secs % 86_400) / 3600, (secs % 3600) / 60);
    // Civil-from-days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(mo <= 2);
    let weekday = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"][days.rem_euclid(7) as usize];
    let rel = if ms > now {
        let mins = (ms - now).div_ceil(60_000);
        match (mins / 1440, (mins % 1440) / 60, mins % 60) {
            (0, 0, m) => format!("in {m} min"),
            (0, h, m) => format!("in {h} h {m} min"),
            (d, h, _) => format!("in {d} d {h} h"),
        }
    } else {
        "already passed".into()
    };
    format!("{weekday} {y:04}-{mo:02}-{d:02} {h:02}:{m:02} UTC ({rel})")
}

fn describe(m: &agents::UsageMeter, now: u64) -> String {
    let resets: Vec<String> = m
        .windows
        .iter()
        .map(|w| match w.resets_at {
            Some(t) => format!("{} {}% left, resets {}", w.label.to_lowercase(), w.remaining_percent, when(t, now)),
            None => format!("{} {}% left", w.label.to_lowercase(), w.remaining_percent),
        })
        .collect();
    format!("{}% left ({})", m.remaining_percent, resets.join("; "))
}

/// The usage section of an AI's instructions: every council member's model and overall usage with
/// reset times, `me` marked as "you", and the forecast if there is one.
pub fn for_prompt(s: &Snapshot, members: &[String], me: &str, now: u64) -> Option<String> {
    let mut lines = Vec::new();
    for id in members {
        let Some(u) = s.agents.get(id) else { continue };
        let name = agents::by_id(id).map_or(id.as_str(), |a| a.name);
        let who = if id == me { format!("{name} (you)") } else { name.to_string() };
        let mut parts = Vec::new();
        if let Some(m) = &u.model {
            if m.shared {
                parts.push(format!("current model {} has no separate limit and draws on the overall one", m.label));
            } else {
                parts.push(format!("current model {}: {}", m.label, describe(m, now)));
            }
        }
        if let Some(o) = &u.overall {
            parts.push(format!("overall: {}", describe(o, now)));
        }
        if !parts.is_empty() {
            lines.push(format!("- {who}: {}", parts.join("; ")));
        }
    }
    if lines.is_empty() && s.forecast.is_none() && s.claude_resets.is_none() {
        return None;
    }
    let age = now.saturating_sub(s.at) / 60_000;
    let mut out = format!(
        "\n\nPlan usage, from each AI's own CLI ({}):\n{}",
        if age == 0 { "just now".to_string() } else { format!("{age} min ago") },
        lines.join("\n")
    );
    out.push_str(
        "\nAn AI at 0% can't answer until its limit resets; the others carry on. Keep this in mind when \
splitting work, and tell the human when someone will be back if it matters.",
    );
    if let Some(f) = &s.forecast {
        let mut line = String::from("\nGlobal Codex reset forecast (unofficial, codex-reset.com): ");
        if let Some(o) = &f.official {
            line.push_str(&format!("official signal: {o}; "));
        }
        match (f.chance_24h, f.chance_48h) {
            (Some(a), Some(b)) => line.push_str(&format!("{a}% chance within 24 h, {b}% within 48 h")),
            (Some(a), None) => line.push_str(&format!("{a}% chance within 24 h")),
            _ => line.push_str("no estimate"),
        }
        if let Some(c) = &f.confidence {
            line.push_str(&format!(" ({c} confidence)"));
        }
        if let Some(t) = f.last_reset_at {
            line.push_str(&format!("; last global reset {}", when(t, now)));
        }
        line.push_str(". It's an estimate, not a promise.");
        out.push_str(&line);
    }
    if let Some(r) = &s.claude_resets {
        out.push_str(&crate::claude_resets::prompt_line(r, now));
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{UsageMeter, UsageWindow};

    #[tokio::test]
    async fn concurrent_usage_reads_share_one_sample_and_later_refresh_is_fresh() {
        let gate = tokio::sync::Mutex::new(None);
        let (started, waiting) = tokio::sync::oneshot::channel();
        let (release, finish) = tokio::sync::oneshot::channel();
        let first = share_refresh(&gate, async {
            started.send(()).unwrap();
            finish.await.unwrap();
            Snapshot { at: 11, ..Snapshot::default() }
        });
        let second = async {
            waiting.await.unwrap();
            let follower = share_refresh(&gate, async { panic!("duplicate provider usage read") });
            tokio::pin!(follower);
            tokio::select! {
                biased;
                _ = &mut follower => panic!("returned before the current sample finished"),
                _ = std::future::ready(()) => release.send(()).unwrap(),
            }
            follower.await
        };
        let (first, second) = tokio::join!(first, second);
        assert_eq!(first, second);
        let later = share_refresh(&gate, async { Snapshot { at: 12, ..Snapshot::default() } }).await;
        assert_eq!(later.at, 12);
    }

    #[tokio::test]
    async fn waiting_usage_read_retries_if_the_first_is_canceled() {
        let gate = tokio::sync::Mutex::new(Some(Snapshot { at: 10, ..Snapshot::default() }));
        let (started, waiting) = tokio::sync::oneshot::channel();
        let (cancel, canceled) = tokio::sync::oneshot::channel();
        let owner = async {
            tokio::select! {
                _ = share_refresh(&gate, async {
                    started.send(()).unwrap();
                    std::future::pending::<Snapshot>().await
                }) => panic!("usage read ended unexpectedly"),
                _ = canceled => {}
            }
        };
        let waiter = async {
            waiting.await.unwrap();
            let follower = share_refresh(&gate, async { Snapshot { at: 11, ..Snapshot::default() } });
            tokio::pin!(follower);
            tokio::select! {
                biased;
                _ = &mut follower => panic!("returned before cancellation"),
                _ = std::future::ready(()) => cancel.send(()).unwrap(),
            }
            follower.await
        };
        let (_, result) = tokio::join!(owner, waiter);
        assert_eq!(result.at, 11);
    }

    fn meter(label: &str, pct: u8, resets: Option<u64>, shared: bool) -> UsageMeter {
        UsageMeter {
            label: label.into(),
            remaining_percent: pct,
            windows: vec![UsageWindow { label: "Weekly limit".into(), remaining_percent: pct, resets_at: resets }],
            shared,
        }
    }

    #[test]
    fn times_are_given_in_utc_with_how_long_until() {
        let t = 1_794_942_000_000; // Tue 2026-11-17 19:00 UTC
        assert_eq!(when(t, t - 2 * 86_400_000 - 3 * 3_600_000), "Tue 2026-11-17 19:00 UTC (in 2 d 3 h)");
        assert_eq!(when(t, t - 90 * 60_000), "Tue 2026-11-17 19:00 UTC (in 1 h 30 min)");
        assert!(when(t, t + 1).ends_with("(already passed)"));
    }

    #[test]
    fn every_ai_sees_everyones_model_and_overall_usage_with_resets() {
        let now = 1_794_942_000_000 - 3_600_000;
        let mut s = Snapshot { at: now - 120_000, ..Snapshot::default() };
        s.agents.insert(
            "claude".into(),
            UsageStatus {
                remaining_percent: 0,
                windows: vec![],
                model: Some(meter("Fable", 0, Some(1_794_942_000_000), false)),
                overall: Some(meter("Claude", 41, None, false)),
            },
        );
        s.agents.insert(
            "gpt".into(),
            UsageStatus {
                remaining_percent: 14,
                windows: vec![],
                model: Some(meter("GPT-6.1-Sol", 14, None, true)),
                overall: Some(meter("ChatGPT", 14, None, false)),
            },
        );
        let members = vec!["gpt".to_string(), "claude".to_string()];
        let text = for_prompt(&s, &members, "claude", now).unwrap();
        assert!(text.contains("(2 min ago)"), "{text}");
        assert!(text.contains("- ChatGPT: current model GPT-6.1-Sol has no separate limit"), "{text}");
        assert!(text.contains("- Claude (you): current model Fable: 0% left (weekly limit 0% left, resets Tue 2026-11-17 19:00 UTC (in 1 h 0 min))"), "{text}");
        assert!(text.contains("overall: 41% left"), "{text}");
        assert!(!text.contains("codex-reset"), "no forecast unless it's on");
        assert_eq!(for_prompt(&Snapshot::default(), &members, "gpt", now), None);
    }

    #[test]
    fn a_new_ai_without_a_known_forecast_asks_for_one_search() {
        assert!(join_note("gpt", "ChatGPT").contains("codex-reset.com"));
        let n = join_note("gemini", "Gemini");
        assert!(n.contains("quick search") && n.contains("REMEMBER:"));
    }

    #[test]
    fn the_forecast_uses_only_documented_fields() {
        let json = r#"{"probabilities":{"rounded_24h":17,"rounded_48h":31,"raw_24h":0.167},"confidence":"low","last_reset_at":"2026-10-28T06:35:38.000Z","official_signal":null,"updated_at":"2026-11-15T15:47:48.948Z"}"#;
        let f = parse_forecast(json, 5).unwrap();
        assert_eq!((f.chance_24h, f.chance_48h, f.confidence.as_deref(), f.official.as_deref(), f.fetched_at), (Some(17), Some(31), Some("low"), None, 5));
        assert!(f.last_reset_at.is_some());
        let s = Snapshot { at: 0, agents: HashMap::new(), forecast: Some(f), claude_resets: None };
        let text = for_prompt(&s, &[], "gpt", 1_795_000_000_000).unwrap();
        assert!(text.contains("17% chance within 24 h, 31% within 48 h (low confidence)"), "{text}");
        assert!(parse_forecast("not json", 0).is_none());
    }
}
