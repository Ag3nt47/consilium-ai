//! Local CLI agents: ChatGPT through OpenAI's Codex CLI, Claude through Anthropic's Claude Code CLI,
//! and Gemini through Google's Antigravity CLI (`agy`). Google stopped serving the Gemini CLI to
//! personal accounts in June 2026; it is still used for Gemini when `agy` isn't installed, for
//! company plans and API keys that keep access.
//!
//! All use the person's own sign-in to those tools, so Consilium never handles an API key or a
//! credential. Rules every run follows:
//! - the executable is started directly, never through a shell, so no text is interpreted as a command;
//! - only documented non-interactive output is read (Codex `-o FILE`, Claude and Gemini JSON);
//! - Stop, or the time limit, kills the whole process group.
//!
//! What the AI may touch depends on its access level (see `config::Access`). At "chat", or when the
//! conversation has no project folder, it runs in a fresh empty temporary folder with tools off.
//! Higher levels run it in the project folder with the matching permissions.

use crate::{config::Access, store::TokenUsage};
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

pub const RUN_TIMEOUT: Duration = Duration::from_secs(900);
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Codex,
    ClaudeCode,
    GeminiCli,
    Antigravity,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentInfo {
    pub id: String,
    pub name: String,
    pub company: String,
    /// Always "local-cli" for now; API-backed agents will say "api".
    pub transport: String,
    pub tool: String,
    pub member: bool,
    pub access: Access,
    pub available: bool,
    pub signed_in: Option<bool>,
    pub version: Option<String>,
    pub path: Option<String>,
    /// What to do if it isn't usable yet.
    pub hint: String,
    /// When it ran out of usage, the time (ms since the epoch) its limit resets.
    pub benched_until: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UsageStatus {
    /// The lowest remaining percentage, because this is the limit that will stop the agent first.
    pub remaining_percent: u8,
    /// Every limit behind the two meters.
    pub windows: Vec<UsageWindow>,
    /// The model in use, named after it (it changes when the AI is switched to another model).
    pub model: Option<UsageMeter>,
    /// The AI's whole plan, across its models.
    pub overall: Option<UsageMeter>,
}

/// One meter: the limits of the model in use, or of the AI's whole plan.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UsageMeter {
    /// What it measures: the model's name, or the AI's.
    pub label: String,
    /// The lowest of `windows`.
    pub remaining_percent: u8,
    pub windows: Vec<UsageWindow>,
    /// The model has no limit of its own and draws on the overall one (the model meter only).
    pub shared: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UsageWindow {
    pub label: String,
    pub remaining_percent: u8,
    /// When this limit resets (ms since the epoch), as the provider reports it.
    #[serde(default)]
    pub resets_at: Option<u64>,
}

pub struct Agent {
    pub id: &'static str,
    pub name: &'static str,
    pub company: &'static str,
    pub kind: Kind,
}

pub const AGENTS: [Agent; 3] = [
    Agent { id: "gpt", name: "ChatGPT", company: "OpenAI", kind: Kind::Codex },
    Agent { id: "claude", name: "Claude", company: "Anthropic", kind: Kind::ClaudeCode },
    Agent { id: "gemini", name: "Gemini", company: "Google", kind: Kind::Antigravity },
];

pub fn by_id(id: &str) -> Option<&'static Agent> {
    AGENTS.iter().find(|a| a.id == id)
}

fn base_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Codex => "codex",
        Kind::ClaudeCode => "claude",
        Kind::GeminiCli => "gemini",
        Kind::Antigravity => "agy",
    }
}

fn exe_names(base: &str) -> Vec<String> {
    if cfg!(windows) {
        vec![format!("{base}.exe"), format!("{base}.cmd"), base.to_string()]
    } else {
        vec![base.to_string()]
    }
}

/// Apps started from the Dock or Start menu don't get the shell's PATH, so well-known install
/// locations are checked as well as PATH.
fn candidates(kind: Kind) -> Vec<PathBuf> {
    let home = dirs::home_dir().unwrap_or_default();
    let mut dirs_: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    dirs_.extend(
        [".local/bin", ".local/node/bin", ".npm-global/bin", ".volta/bin", "bin"]
            .iter()
            .map(|d| home.join(d)),
    );
    dirs_.extend(["/usr/local/bin", "/opt/homebrew/bin", "/usr/bin"].iter().map(PathBuf::from));
    if let Some(appdata) = std::env::var_os("APPDATA") {
        dirs_.push(PathBuf::from(appdata).join("npm"));
    }
    let mut out: Vec<PathBuf> = Vec::new();
    match kind {
        Kind::Codex => {
            // The ChatGPT desktop app ships a copy of Codex.
            for app in ["/Applications/ChatGPT.app", "/Applications/Codex.app"] {
                out.push(PathBuf::from(app).join("Contents/Resources/codex-cli/bin/codex"));
                out.push(home.join(app.trim_start_matches('/')).join("Contents/Resources/codex-cli/bin/codex"));
            }
            out.push(PathBuf::from("/Applications/Codex.app/Contents/Resources/codex"));
        }
        Kind::ClaudeCode => out.push(home.join(".claude/local/claude")),
        Kind::GeminiCli => {}
        Kind::Antigravity => {
            out.push(home.join(".local/bin/agy"));
            if let Some(local) = std::env::var_os("LOCALAPPDATA") {
                out.push(PathBuf::from(local).join("agy").join("bin").join("agy.exe"));
            }
        }
    }
    for d in dirs_ {
        for n in exe_names(base_name(kind)) {
            out.push(d.join(n));
        }
    }
    out
}

pub fn find(kind: Kind) -> Option<PathBuf> {
    candidates(kind).into_iter().find(|p| is_executable(p))
}

/// The tool an agent actually uses: Gemini runs through Antigravity when it's installed, and
/// falls back to the Gemini CLI (still served to company plans and API keys) when it isn't.
pub fn effective_kind(agent: &Agent) -> Kind {
    if agent.kind == Kind::Antigravity && find(Kind::Antigravity).is_none() && find(Kind::GeminiCli).is_some() {
        Kind::GeminiCli
    } else {
        agent.kind
    }
}

fn is_executable(p: &Path) -> bool {
    match std::fs::metadata(p) {
        Ok(m) if m.is_file() => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                m.permissions().mode() & 0o111 != 0
            }
            #[cfg(not(unix))]
            {
                true
            }
        }
        _ => false,
    }
}

fn command(path: &Path) -> Command {
    let mut cmd = Command::new(path);
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    // Node-based CLIs (Gemini, npm installs) start with `#!/usr/bin/env node`, and apps launched from
    // the Dock have a minimal PATH; putting the tool's own folder first lets it find its runtime.
    if let Some(dir) = path.parent() {
        let mut parts: Vec<PathBuf> = vec![dir.to_path_buf()];
        if let Some(existing) = std::env::var_os("PATH") {
            parts.extend(std::env::split_paths(&existing));
        }
        if let Ok(joined) = std::env::join_paths(parts) {
            cmd.env("PATH", joined);
        }
    }
    // Claude Code finds its saved sign-in by user name; some launchers start apps without USER.
    #[cfg(unix)]
    if std::env::var_os("USER").is_none() {
        if let Some(name) = dirs::home_dir().and_then(|h| h.file_name().map(|n| n.to_os_string())) {
            cmd.env("USER", &name).env("LOGNAME", &name);
        }
    }
    #[cfg(unix)]
    cmd.process_group(0); // its own group, so Stop can kill everything it started
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Runs a short status command and returns its combined output, or None.
async fn probe(path: &Path, args: &[&str]) -> Option<String> {
    let mut cmd = command(path);
    cmd.args(args);
    let out = tokio::time::timeout(PROBE_TIMEOUT, cmd.output()).await.ok()?.ok()?;
    let mut s = String::from_utf8_lossy(&out.stdout).to_string();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    Some(s.trim().to_string())
}

/// Runs a short machine-readable command whose request is supplied on stdin.
async fn probe_input(path: &Path, args: &[&str], input: &str) -> Option<String> {
    let mut cmd = command(path);
    cmd.args(args).stdin(Stdio::piped());
    let mut child = cmd.spawn().ok()?;
    let mut stdin = child.stdin.take()?;
    stdin.write_all(input.as_bytes()).await.ok()?;
    stdin.shutdown().await.ok()?;
    drop(stdin);
    let out = tokio::time::timeout(PROBE_TIMEOUT, child.wait_with_output())
        .await
        .ok()?
        .ok()?;
    let mut s = String::from_utf8_lossy(&out.stdout).to_string();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    Some(s.trim().to_string())
}

fn percent(n: f64) -> Option<u8> {
    n.is_finite().then(|| n.clamp(0.0, 100.0).round() as u8)
}

fn window_from_used(label: impl Into<String>, used: f64, resets_at: Option<u64>) -> Option<UsageWindow> {
    Some(UsageWindow {
        label: label.into(),
        remaining_percent: 100_u8.saturating_sub(percent(used)?),
        resets_at,
    })
}

/// An ISO 8601 time such as "2026-11-17T19:00:00.389+00:00" or "2026-11-22T00:24:48Z", in ms
/// since the epoch.
pub fn iso_ms(s: &str) -> Option<u64> {
    let s = s.trim();
    let num = |a: usize, b: usize| s.get(a..b)?.parse::<i64>().ok();
    let (y, mo, d, h, mi, sec) = (num(0, 4)?, num(5, 7)?, num(8, 10)?, num(11, 13)?, num(14, 16)?, num(17, 19)?);
    let mut rest = &s[19..];
    let mut ms = 0i64;
    if let Some(frac) = rest.strip_prefix('.') {
        let digits: String = frac.chars().take_while(|c| c.is_ascii_digit()).collect();
        ms = format!("{:0<3}", &digits[..digits.len().min(3)]).parse().ok()?;
        rest = &frac[digits.len()..];
    }
    let offset_min = match rest {
        "" | "Z" | "z" => 0,
        o => {
            let sign = if o.starts_with('-') { -1 } else { 1 };
            let hh: i64 = o.get(1..3)?.parse().ok()?;
            let mm: i64 = o.get(4..6).or(o.get(3..5)).and_then(|m| m.parse().ok()).unwrap_or(0);
            sign * (hh * 60 + mm)
        }
    };
    // Days from civil (Howard Hinnant).
    let (y2, m2) = if mo <= 2 { (y - 1, mo + 9) } else { (y, mo - 3) };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let doy = (153 * m2 + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + h * 3600 + mi * 60 + sec - offset_min * 60;
    u64::try_from(secs * 1000 + ms).ok()
}

fn meter(label: impl Into<String>, windows: Vec<UsageWindow>) -> Option<UsageMeter> {
    let remaining_percent = windows.iter().map(|w| w.remaining_percent).min()?;
    Some(UsageMeter { label: label.into(), remaining_percent, windows, shared: false })
}

/// Puts the two meters together. A model without limits of its own (`model` is None) draws on
/// the overall ones, so its meter shows those, under its name, when the name is known.
fn usage_from(model_name: Option<String>, model: Option<UsageMeter>, overall: Option<UsageMeter>) -> Option<UsageStatus> {
    let model = model.or_else(|| {
        let o = overall.as_ref()?;
        Some(UsageMeter { label: model_name?, remaining_percent: o.remaining_percent, windows: o.windows.clone(), shared: true })
    });
    let mut windows = Vec::new();
    for m in model.iter().filter(|m| !m.shared).chain(overall.iter()) {
        windows.extend(m.windows.iter().cloned());
    }
    let remaining_percent = windows.iter().map(|w| w.remaining_percent).min()?;
    Some(UsageStatus { remaining_percent, windows, model, overall })
}

/// "claude-opus-5-5" or "opus" → "Opus 5.5" / "Opus", for model ids that carry no display name.
fn model_name(id: &str) -> String {
    let base = id.split('[').next().unwrap_or(id).trim();
    let base = base.strip_prefix("claude-").unwrap_or(base);
    let mut parts = base.split('-').filter(|p| !p.is_empty());
    let Some(first) = parts.next() else { return id.to_string() };
    let mut name: String = first.chars().take(1).flat_map(char::to_uppercase).chain(first.chars().skip(1)).collect();
    // Version numbers, without a trailing date (claude-sonnet-4-5-20250929).
    let version: Vec<&str> = parts.take_while(|p| p.len() <= 2 && p.chars().all(|c| c.is_ascii_digit())).collect();
    if !version.is_empty() {
        name.push(' ');
        name.push_str(&version.join("."));
    }
    name
}

fn codex_window(label: &str, v: &serde_json::Value) -> Option<UsageWindow> {
    let used = v["usedPercent"].as_f64()?;
    let resets_at = v["resetsAt"].as_u64().map(|secs| secs * 1000);
    let label = match v["windowDurationMins"].as_u64() {
        Some(300) => "5-hour limit".to_string(),
        Some(10_080) => "Weekly limit".to_string(),
        Some(minutes) if minutes % 1_440 == 0 => format!("{}-day limit", minutes / 1_440),
        Some(minutes) if minutes % 60 == 0 => format!("{}-hour limit", minutes / 60),
        _ => label.to_string(),
    };
    window_from_used(label, used, resets_at)
}

/// The limits in one of Codex's rate-limit buckets.
fn codex_windows(limits: &serde_json::Value) -> Vec<UsageWindow> {
    let mut windows = Vec::new();
    if let Some(w) = codex_window("Primary limit", &limits["primary"]) {
        windows.push(w);
    }
    if let Some(w) = codex_window("Secondary limit", &limits["secondary"]) {
        windows.push(w);
    }
    if let Some(remaining) = limits["individualLimit"]["remainingPercent"].as_f64().and_then(percent) {
        windows.push(UsageWindow { label: "Individual spend limit".into(), remaining_percent: remaining, resets_at: None });
    }
    windows
}

/// Codex's `account/rateLimits/read` reply, with its `model/list` reply (for the default model and
/// display names) and the model set in Codex's config, if any.
fn codex_usage_from(limits: &serde_json::Value, models: &serde_json::Value, configured: Option<&str>) -> Option<UsageStatus> {
    let r = &limits["result"];
    let by_id = r["rateLimitsByLimitId"].as_object();
    let overall_bucket = by_id.and_then(|m| m.get("codex")).unwrap_or(&r["rateLimits"]);
    let overall = meter("ChatGPT", codex_windows(overall_bucket));
    let list = models["result"]["data"].as_array().cloned().unwrap_or_default();
    let id = configured.map(str::to_string).or_else(|| {
        list.iter().find(|m| m["isDefault"].as_bool() == Some(true)).and_then(|m| m["model"].as_str().or(m["id"].as_str())).map(str::to_string)
    });
    let name = id.as_ref().map(|id| {
        list.iter()
            .find(|m| m["model"].as_str() == Some(id) || m["id"].as_str() == Some(id))
            .and_then(|m| m["displayName"].as_str())
            .map_or_else(|| id.clone(), str::to_string)
    });
    // A bucket of its own for the model in use, when Codex has one.
    let own = by_id.and_then(|m| {
        m.iter().find(|(key, b)| {
            key.as_str() != "codex"
                && id.as_deref().is_some_and(|id| b["normalModelSlug"].as_str() == Some(id) || key.as_str() == id)
        })
    });
    let model = own.and_then(|(_, b)| meter(name.clone().unwrap_or_default(), codex_windows(b)));
    usage_from(name, model, overall)
}

/// The model set in Codex's config.toml (`model = "..."`, before any [section]), if any.
fn codex_configured_model() -> Option<String> {
    let home = std::env::var_os("CODEX_HOME").map(PathBuf::from).or_else(|| dirs::home_dir().map(|h| h.join(".codex")))?;
    let text = std::fs::read_to_string(home.join("config.toml")).ok()?;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            break;
        }
        if let Some(v) = t.strip_prefix("model").map(str::trim_start).and_then(|r| r.strip_prefix('=')) {
            let v = v.split('#').next().unwrap_or("").trim().trim_matches('"').trim_matches('\'');
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// Codex exposes plan limits through its documented newline-delimited app-server protocol.
async fn codex_usage(path: &Path) -> Option<UsageStatus> {
    let mut cmd = command(path);
    cmd.args(["app-server"]).stdin(Stdio::piped()).stderr(Stdio::null());
    let mut child = cmd.spawn().ok()?;
    let mut stdin = child.stdin.take()?;
    let stdout = child.stdout.take()?;
    let mut lines = BufReader::new(stdout).lines();
    let exchange = async {
        stdin
            .write_all(b"{\"id\":1,\"method\":\"initialize\",\"params\":{\"clientInfo\":{\"name\":\"consilium\",\"version\":\"0.1.0\"}}}\n")
            .await
            .ok()?;
        loop {
            let line = lines.next_line().await.ok()??;
            let v: serde_json::Value = serde_json::from_str(&line).ok()?;
            if v["id"].as_i64() == Some(1) {
                break;
            }
        }
        stdin
            .write_all(b"{\"method\":\"initialized\"}\n{\"id\":2,\"method\":\"account/rateLimits/read\"}\n{\"id\":3,\"method\":\"model/list\",\"params\":{}}\n")
            .await
            .ok()?;
        let (mut limits, mut models) = (None, None);
        while limits.is_none() || models.is_none() {
            let Ok(Some(line)) = lines.next_line().await else { break };
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
            match v["id"].as_i64() {
                Some(2) => limits = Some(v),
                Some(3) => models = Some(v),
                _ => {}
            }
        }
        let limits = limits?;
        codex_usage_from(&limits, &models.unwrap_or_default(), codex_configured_model().as_deref())
    };
    let result = tokio::time::timeout(PROBE_TIMEOUT, exchange).await.ok().flatten();
    // `kill` also waits for the process to exit, so repeated refreshes cannot leave app-server
    // children behind as zombies.
    let _ = child.kill().await;
    result
}

/// Claude Code's `get_usage` reply. `model` names the model in use when no model-specific limit is
/// active (Claude Code's settings, or the model of Claude's latest reply).
fn claude_usage_from_output(output: &str, model: Option<String>) -> Option<UsageStatus> {
    for line in output.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let body = &v["response"]["response"];
        if body["rate_limits_available"].as_bool() != Some(true) {
            continue;
        }
        let limits = &body["rate_limits"];
        let (mut overall, mut own, mut active) = (Vec::new(), Vec::new(), None);
        if let Some(rows) = limits["limits"].as_array() {
            for row in rows {
                let Some(used) = row["percent"].as_f64() else {
                    continue;
                };
                let kind = row["kind"].as_str().unwrap_or("Plan limit");
                let resets = row["resets_at"].as_str().and_then(iso_ms);
                if row["scope"].is_null() {
                    let label = match kind {
                        "session" => "5-hour limit".to_string(),
                        "weekly_all" => "Weekly limit".to_string(),
                        other => other.replace('_', " "),
                    };
                    overall.extend(window_from_used(label, used, resets));
                } else if row["is_active"].as_bool() == Some(true) {
                    // A model's own limit counts only for the model currently selected.
                    active = row["scope"]["model"]["display_name"].as_str().map(str::to_string);
                    let label = if kind.starts_with("weekly") { "Weekly limit".to_string() } else { kind.replace('_', " ") };
                    own.extend(window_from_used(label, used, resets));
                }
            }
        }
        // Older CLIs have the named windows but not the unified limits array.
        if overall.is_empty() {
            for (key, label) in [("five_hour", "5-hour limit"), ("seven_day", "Weekly limit")] {
                if let Some(used) = limits[key]["utilization"].as_f64() {
                    overall.extend(window_from_used(label, used, limits[key]["resets_at"].as_str().and_then(iso_ms)));
                }
            }
        }
        let name = active.or_else(|| model.clone());
        let own = meter(name.clone().unwrap_or_default(), own);
        if let Some(usage) = usage_from(name, own, meter("Claude", overall)) {
            return Some(usage);
        }
    }
    None
}

/// The model chosen in Claude Code's settings, if any.
fn claude_configured_model() -> Option<String> {
    let dir = std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from).or_else(|| dirs::home_dir().map(|h| h.join(".claude")))?;
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.join("settings.json")).ok()?).ok()?;
    v["model"].as_str().filter(|m| !m.is_empty() && *m != "default").map(model_name)
}

async fn claude_usage(path: &Path, last_model: Option<&str>) -> Option<UsageStatus> {
    let request = "{\"type\":\"control_request\",\"request_id\":\"consilium-usage\",\"request\":{\"subtype\":\"get_usage\",\"skip_behaviors\":true}}\n";
    let output = probe_input(
        path,
        &[
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--tools",
            "",
            "--strict-mcp-config",
            "--no-session-persistence",
        ],
        request,
    )
    .await?;
    let model = claude_configured_model().or_else(|| last_model.map(model_name));
    claude_usage_from_output(&output, model)
}

/// Antigravity's `/usage`: limits per group of models. The Gemini group is Gemini's whole plan in
/// Consilium; the model in use is the one the group lists, which draws on the group's limits.
fn antigravity_usage_from_output(output: &str) -> Option<UsageStatus> {
    let start = output.find('{')?;
    let end = output.rfind('}')? + 1;
    let v: serde_json::Value = serde_json::from_str(&output[start..end]).ok()?;
    let groups = v["command"]["data"]["groups"].as_array()?;
    let group = groups.iter().find(|g| g["name"].as_str().is_some_and(|n| n.to_ascii_lowercase().contains("gemini")))?;
    let mut windows = Vec::new();
    for bucket in group["buckets"].as_array()? {
        let Some(fraction) = bucket["remaining_fraction"].as_f64() else {
            continue;
        };
        let Some(remaining_percent) = percent(fraction * 100.0) else {
            continue;
        };
        let label = bucket["name"].as_str().unwrap_or("Plan limit").trim_end_matches(" Remaining").to_string();
        let resets_at = bucket["reset_time"].as_str().and_then(iso_ms);
        windows.push(UsageWindow { label, remaining_percent, resets_at });
    }
    let name = group["description"]
        .as_str()
        .and_then(|d| d.split_once("Models within this group:"))
        .map(|(_, models)| models.trim().trim_end_matches('.').to_string())
        .filter(|m| !m.is_empty());
    usage_from(name, None, meter("Gemini", windows))
}

async fn usage(path: &Path, kind: Kind, last_model: Option<&str>) -> Option<UsageStatus> {
    match kind {
        Kind::Codex => codex_usage(path).await,
        Kind::ClaudeCode => claude_usage(path, last_model).await,
        Kind::Antigravity => {
            let output = probe(path, &["-p", "/usage", "--output-format", "json"]).await?;
            antigravity_usage_from_output(&output)
        }
        // The legacy Gemini CLI does not expose subscription quota in supported machine output.
        Kind::GeminiCli => None,
    }
}

/// Gemini CLI records the chosen sign-in method in ~/.gemini/settings.json; for a Google account
/// sign-in it also keeps a credentials file, whose presence (never its contents) is checked here.
fn gemini_signed_in() -> bool {
    let Some(home) = dirs::home_dir() else { return false };
    let dir = home.join(".gemini");
    let Ok(text) = std::fs::read_to_string(dir.join("settings.json")) else { return false };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { return false };
    let method = v["security"]["auth"]["selectedType"]
        .as_str()
        .or_else(|| v["selectedAuthType"].as_str())
        .unwrap_or("");
    match method {
        "" => false,
        "oauth-personal" => dir.join("oauth_creds.json").is_file(),
        "gemini-api-key" => std::env::var_os("GEMINI_API_KEY").is_some(),
        _ => true, // Vertex AI or another method configured in the CLI itself
    }
}

/// Antigravity has no "am I signed in?" command, and starting it while signed out opens a browser
/// sign-in page, so it is never launched just to check. Signing in creates a small marker file
/// (`antigravity/credential-user-scope` in the user config folder) next to the token it keeps in the
/// OS keyring; its presence, never its contents, is the check. Signing out (`/logout`) removes it.
fn antigravity_signed_in() -> bool {
    [dirs::config_dir(), dirs::data_dir()]
        .into_iter()
        .flatten()
        .any(|d| d.join("antigravity").join("credential-user-scope").is_file())
}

pub async fn status(agent: &Agent, member: bool, access: Access) -> AgentInfo {
    let kind = effective_kind(agent);
    let (tool, install_hint, login_hint) = match kind {
        Kind::Codex => (
            "Codex CLI",
            "Install the ChatGPT desktop app (it includes Codex), or Codex from developers.openai.com/codex.",
            "Sign in to the ChatGPT desktop app, or run `codex login` in a terminal.",
        ),
        Kind::ClaudeCode => (
            "Claude Code CLI",
            "Install Claude Code from claude.com/claude-code.",
            "Run `claude auth login` in a terminal and sign in with your Claude account.",
        ),
        Kind::GeminiCli => (
            "Gemini CLI",
            "Install the Gemini CLI from github.com/google-gemini/gemini-cli (needs Node.js).",
            "Run `gemini` in a terminal, choose Sign in with Google, then type /quit. (Personal Google accounts now need the Antigravity CLI instead.)",
        ),
        Kind::Antigravity => (
            "Antigravity CLI",
            "Install Google's Antigravity CLI from antigravity.google.",
            "Run `agy` in a terminal, choose Google OAuth and sign in in your browser, then type /quit.",
        ),
    };
    let mut info = AgentInfo {
        id: agent.id.into(),
        name: agent.name.into(),
        company: agent.company.into(),
        transport: "local-cli".into(),
        tool: tool.into(),
        member,
        access,
        available: false,
        signed_in: None,
        version: None,
        path: None,
        hint: install_hint.into(),
        benched_until: None,
    };
    let Some(path) = find(kind) else { return info };
    info.path = Some(path.display().to_string());
    // The version and sign-in checks are independent, so they run at the same time.
    let version_probe = probe(&path, &["--version"]);
    let sign_in_probe = async {
        match kind {
            Kind::Codex => probe(&path, &["login", "status"]).await.map(|s| s.contains("Logged in")),
            Kind::ClaudeCode => probe(&path, &["auth", "status"]).await.and_then(|s| {
                serde_json::from_str::<serde_json::Value>(&s).ok().and_then(|v| v["loggedIn"].as_bool())
            }),
            Kind::GeminiCli => Some(gemini_signed_in()),
            Kind::Antigravity => Some(antigravity_signed_in()),
        }
    };
    let (version, signed_in) = tokio::join!(version_probe, sign_in_probe);
    info.version = version.and_then(|v| v.lines().next().map(str::to_string));
    info.signed_in = signed_in;
    info.available = signed_in == Some(true);
    info.hint = if info.available { String::new() } else { login_hint.into() };
    info
}

/// Plan usage for one AI, or None when its CLI is missing or doesn't report it. Kept out of
/// `status` because these probes start a full CLI session (several seconds each), and sign-in
/// state shouldn't wait on them.
/// `last_model` is the model of this AI's latest reply, when known: Claude names its model this way
/// when neither its settings nor a model-specific limit do.
pub async fn usage_of(agent: &Agent, last_model: Option<&str>) -> Option<UsageStatus> {
    let kind = effective_kind(agent);
    let path = find(kind)?;
    usage(&path, kind, last_model).await
}

/// Where and how an agent runs for one turn.
pub struct Plan {
    pub args: Vec<OsString>,
    /// What is written to the tool's standard input.
    pub input: String,
}

/// One provider turn, including token accounting when the CLI reports it.
pub struct AgentReply {
    pub text: String,
    pub usage: Option<TokenUsage>,
    /// The model that answered, when the CLI says (Claude Code does).
    pub model: Option<String>,
}

/// The exact command-line arguments for one turn. `workdir` is the project folder when the AI has
/// access to one, otherwise an empty temporary folder; `access` is already "chat" in that case.
pub fn plan(kind: Kind, access: Access, workdir: &Path, reply_file: &Path, system: &str, prompt: &str) -> Plan {
    let mut args: Vec<OsString> = Vec::new();
    let mut push = |a: &str| args.push(a.into());
    let input = match kind {
        Kind::Codex => {
            push("exec");
            push("--ephemeral");
            push("--skip-git-repo-check");
            push("--json");
            match access {
                Access::Chat | Access::Read => {
                    push("-s");
                    push("read-only");
                }
                Access::Edit => {
                    push("-s");
                    push("workspace-write");
                }
                Access::Own => {} // the person's own Codex configuration decides
                Access::Unrestricted => push("--dangerously-bypass-approvals-and-sandbox"),
            }
            push("--color");
            push("never");
            push("-C");
            args.push(workdir.into());
            args.push("-o".into());
            args.push(reply_file.into());
            args.push("-".into());
            format!("{system}\n\n{prompt}")
        }
        Kind::ClaudeCode => {
            for a in ["-p", "--output-format", "json", "--no-session-persistence"] {
                push(a);
            }
            match access {
                Access::Chat => {
                    push("--tools");
                    push("");
                    push("--strict-mcp-config");
                }
                Access::Read => {
                    push("--tools");
                    push("Read,Grep,Glob");
                    push("--strict-mcp-config");
                }
                Access::Edit => {
                    push("--tools");
                    push("Read,Grep,Glob,Edit,Write");
                    push("--permission-mode");
                    push("acceptEdits");
                    push("--strict-mcp-config");
                }
                Access::Own => {} // the person's own Claude Code settings and allow-lists decide
                Access::Unrestricted => push("--dangerously-skip-permissions"),
            }
            push("--append-system-prompt");
            args.push(system.into());
            prompt.to_string()
        }
        Kind::GeminiCli => {
            push("-p");
            push("");
            push("-o");
            push("json");
            push("--skip-trust");
            match access {
                Access::Chat | Access::Read => {
                    push("--approval-mode");
                    push("plan");
                }
                Access::Edit => {
                    push("--approval-mode");
                    push("auto_edit");
                }
                Access::Own => {} // the Gemini CLI's own approval settings decide
                Access::Unrestricted => {
                    push("--approval-mode");
                    push("yolo");
                }
            }
            format!("{system}\n\n{prompt}")
        }
        Kind::Antigravity => {
            // The conversation goes over stdin as one stream-json message, not as an argument:
            // transcripts can exceed Linux's 128 KB argument and Windows' 32 K command-line limits.
            for a in ["--input-format", "stream-json", "--output-format", "stream-json", "--print-timeout", "0s"] {
                push(a);
            }
            match access {
                Access::Chat | Access::Read => {
                    push("--mode");
                    push("plan");
                }
                Access::Edit => {
                    push("--mode");
                    push("accept-edits");
                }
                Access::Own => {} // Antigravity's own permission settings decide
                Access::Unrestricted => push("--dangerously-skip-permissions"),
            }
            push("-p");
            push("");
            let message = serde_json::json!({
                "event": "user",
                "message": { "content": format!("{system}\n\n{prompt}") }
            });
            format!("{message}\n")
        }
    };
    Plan { args, input }
}

/// Asks one agent for its next message. `workspace` is the conversation's project folder, used
/// only when the agent's access is above "chat".
pub async fn run(
    agent: &Agent,
    access: Access,
    workspace: Option<&Path>,
    system: &str,
    prompt: &str,
    cancel: &CancellationToken,
) -> Result<String, String> {
    run_with_usage(agent, access, workspace, system, prompt, cancel).await.map(|reply| reply.text)
}

/// The normal council turn path. Unlike `run`, this keeps the provider's token counts so they can
/// be stored beside the reply. `run` remains the lightweight text-only helper for self-checks.
pub async fn run_with_usage(
    agent: &Agent,
    access: Access,
    workspace: Option<&Path>,
    system: &str,
    prompt: &str,
    cancel: &CancellationToken,
) -> Result<AgentReply, String> {
    let kind = effective_kind(agent);
    let path = find(kind).ok_or_else(|| format!("{} isn't installed", agent.name))?;
    run_at_with_usage(&path, kind, access, workspace, system, prompt, cancel, RUN_TIMEOUT).await
}

/// Text-only `run_at_with_usage` at a given CLI path and time limit; the tests drive fake CLIs
/// (shell scripts, so Unix only) with it.
#[cfg(all(test, unix))]
#[allow(clippy::too_many_arguments)]
pub async fn run_at(
    path: &Path,
    kind: Kind,
    access: Access,
    workspace: Option<&Path>,
    system: &str,
    prompt: &str,
    cancel: &CancellationToken,
    timeout: Duration,
) -> Result<String, String> {
    run_at_with_usage(
        path, kind, access, workspace, system, prompt, cancel, timeout,
    )
    .await
    .map(|reply| reply.text)
}

#[allow(clippy::too_many_arguments)]
async fn run_at_with_usage(
    path: &Path,
    kind: Kind,
    access: Access,
    workspace: Option<&Path>,
    system: &str,
    prompt: &str,
    cancel: &CancellationToken,
    timeout: Duration,
) -> Result<AgentReply, String> {
    let work = std::env::temp_dir().join(format!("consilium-{}", uuid::Uuid::new_v4()));
    let empty = work.join("empty");
    std::fs::create_dir_all(&empty).map_err(|e| format!("couldn't create a work folder: {e}"))?;
    let (dir, access) = match workspace {
        Some(ws) if access != Access::Chat && ws.is_dir() => (ws.to_path_buf(), access),
        _ => (empty.clone(), Access::Chat),
    };
    let reply_file = work.join("reply.txt");
    let plan = plan(kind, access, &dir, &reply_file, system, prompt);
    let result = execute(path, kind, &dir, &reply_file, plan, cancel, timeout).await;
    let _ = std::fs::remove_dir_all(&work);
    result
}

async fn execute(
    path: &Path,
    kind: Kind,
    dir: &Path,
    reply_file: &Path,
    plan: Plan,
    cancel: &CancellationToken,
    timeout: Duration,
) -> Result<AgentReply, String> {
    let mut cmd = command(path);
    cmd.current_dir(dir).stdin(Stdio::piped()).args(&plan.args);
    let mut child = cmd.spawn().map_err(|e| format!("couldn't start {}: {e}", path.display()))?;
    let pid = child.id();
    let mut stdin = child.stdin.take().expect("stdin is piped");
    let stdout = child.stdout.take().expect("stdout is piped");
    let stderr = child.stderr.take().expect("stderr is piped");
    let input = plan.input;
    let writer = tokio::spawn(async move {
        let _ = stdin.write_all(input.as_bytes()).await;
        let _ = stdin.shutdown().await;
    });
    let (out_buf, out_task) = drain(stdout);
    let (err_buf, err_task) = drain(stderr);

    let outcome = tokio::select! {
        status = child.wait() => Ok(status),
        _ = cancel.cancelled() => Err("stopped".to_string()),
        _ = tokio::time::sleep(timeout) => Err(format!("no answer within {} seconds", timeout.as_secs())),
    };
    let status = match outcome {
        Ok(s) => s.map_err(|e| e.to_string())?,
        Err(why) => {
            kill_tree(pid, &mut child).await;
            writer.abort();
            out_task.abort();
            err_task.abort();
            return Err(why);
        }
    };
    writer.abort();
    // A program the AI started in the background (a dev server, a watcher) can keep the output
    // pipes open after the CLI itself has exited. Waiting for them to close would hang the turn,
    // and Stop with it, so whatever has arrived shortly after the exit is the output.
    let pipes_closed = async {
        let _ = out_task.await;
        let _ = err_task.await;
    };
    tokio::select! {
        _ = pipes_closed => {}
        _ = tokio::time::sleep(PIPE_GRACE) => {}
        _ = cancel.cancelled() => return Err("stopped".to_string()),
    }
    let stdout = String::from_utf8_lossy(&out_buf.lock().unwrap()).to_string();
    let stderr = String::from_utf8_lossy(&err_buf.lock().unwrap()).to_string();

    let usage = token_usage_from_output(kind, &stdout);
    let model = reply_model(kind, &stdout);
    let reply = match kind {
        Kind::Codex => std::fs::read_to_string(reply_file).unwrap_or_default(),
        Kind::ClaudeCode => match serde_json::from_str::<serde_json::Value>(stdout.trim()) {
            Ok(v) => {
                let text = v["result"].as_str().unwrap_or_default().to_string();
                if v["is_error"].as_bool() == Some(true) {
                    return Err(tail(if text.is_empty() { &stderr } else { &text }));
                }
                text
            }
            Err(_) => String::new(),
        },
        Kind::GeminiCli => match serde_json::from_str::<serde_json::Value>(stdout.trim()) {
            Ok(v) => {
                if let Some(msg) = v["error"]["message"].as_str() {
                    return Err(tail(msg));
                }
                v["response"].as_str().unwrap_or_default().to_string()
            }
            Err(_) => String::new(),
        },
        Kind::Antigravity => antigravity_reply(&stdout)?,
    };
    if !status.success() || reply.trim().is_empty() {
        let detail = if stderr.trim().is_empty() { stdout } else { stderr };
        return Err(format!("exit {}: {}", status.code().unwrap_or(-1), tail(&detail)));
    }
    Ok(AgentReply {
        text: reply.trim().to_string(),
        usage,
        model,
    })
}

/// The model that wrote a reply, when the CLI's output says: Claude Code's JSON result lists the
/// models a turn used (the main one did most of the writing).
fn reply_model(kind: Kind, stdout: &str) -> Option<String> {
    if kind != Kind::ClaudeCode {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).ok()?;
    let models = v["modelUsage"].as_object()?;
    models
        .iter()
        .max_by_key(|(_, u)| u["outputTokens"].as_u64().unwrap_or(0))
        .map(|(id, _)| id.clone())
}

fn json_u64(value: &serde_json::Value, key: &str) -> u64 {
    value[key]
        .as_u64()
        .or_else(|| value[key].as_i64().and_then(|n| u64::try_from(n).ok()))
        .unwrap_or(0)
}

fn token_usage_from_output(kind: Kind, stdout: &str) -> Option<TokenUsage> {
    match kind {
        Kind::Codex => stdout
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .find_map(|v| {
                (v["type"] == "turn.completed").then(|| {
                    let usage = &v["usage"];
                    let input_tokens = json_u64(usage, "input_tokens");
                    let output_tokens = json_u64(usage, "output_tokens");
                    let parsed = TokenUsage {
                        input_tokens,
                        output_tokens,
                        cache_read_tokens: json_u64(usage, "cached_input_tokens"),
                        total_tokens: json_u64(usage, "total_tokens")
                            .max(input_tokens.saturating_add(output_tokens)),
                        ..TokenUsage::default()
                    };
                    (parsed.total_tokens > 0).then_some(parsed)
                })?
            }),
        Kind::ClaudeCode => {
            let v: serde_json::Value = serde_json::from_str(stdout.trim()).ok()?;
            let usage = &v["usage"];
            let input_tokens = json_u64(usage, "input_tokens");
            let output_tokens = json_u64(usage, "output_tokens");
            let cache_read_tokens = json_u64(usage, "cache_read_input_tokens");
            let cache_creation_tokens = json_u64(usage, "cache_creation_input_tokens");
            let reported = json_u64(usage, "total_tokens");
            let calculated = input_tokens
                .saturating_add(output_tokens)
                .saturating_add(cache_read_tokens)
                .saturating_add(cache_creation_tokens);
            (reported > 0 || calculated > 0).then_some(TokenUsage {
                input_tokens,
                output_tokens,
                cache_read_tokens,
                cache_creation_tokens,
                total_tokens: reported.max(calculated),
                ..TokenUsage::default()
            })
        }
        Kind::Antigravity => stdout
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .find_map(|v| {
                if v["event"] != "result" {
                    return None;
                }
                let usage = if v["result"]["usage"].is_object() {
                    &v["result"]["usage"]
                } else {
                    &v["usage"]
                };
                let input_tokens = json_u64(usage, "input_tokens");
                let output_tokens = json_u64(usage, "output_tokens");
                let thinking_tokens = json_u64(usage, "thinking_tokens");
                let cache_read_tokens = json_u64(usage, "cache_read_tokens");
                let reported = json_u64(usage, "total_tokens");
                let calculated = input_tokens
                    .saturating_add(output_tokens)
                    .saturating_add(thinking_tokens);
                (reported > 0 || calculated > 0).then_some(TokenUsage {
                    input_tokens,
                    output_tokens,
                    thinking_tokens,
                    cache_read_tokens,
                    total_tokens: reported.max(calculated),
                    ..TokenUsage::default()
                })
            }),
        // The fallback Gemini CLI's JSON token statistics have changed between releases. Do not
        // guess at fields: record them only after a stable machine-readable schema is documented.
        Kind::GeminiCli => None,
    }
}

/// How long to keep reading output after the CLI exits (see `execute`).
const PIPE_GRACE: Duration = Duration::from_secs(3);

/// Reads a pipe into a shared buffer in the background, so what has arrived can be used even if
/// the pipe never closes.
fn drain<R: tokio::io::AsyncRead + Unpin + Send + 'static>(
    mut r: R,
) -> (std::sync::Arc<std::sync::Mutex<Vec<u8>>>, tokio::task::JoinHandle<()>) {
    let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let b = buf.clone();
    let task = tokio::spawn(async move {
        let mut chunk = [0u8; 8192];
        loop {
            match r.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => b.lock().unwrap().extend_from_slice(&chunk[..n]),
            }
        }
    });
    (buf, task)
}

/// The reply from Antigravity's stream-json output: the final `result` event's response, or the
/// streamed text if that is empty. A run that ended without text because a tool it reached for was
/// blocked is reported as such, rather than as an empty answer.
fn antigravity_reply(stdout: &str) -> Result<String, String> {
    let mut streamed = String::new();
    let mut result = None;
    for line in stdout.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        if v["event"] == "step_update" {
            if let Some(t) = v["step_update"]["text_delta"].as_str() {
                streamed.push_str(t);
            }
        } else if v["event"] == "result" {
            result = Some(v["result"].clone());
        }
    }
    let Some(r) = result else { return Ok(streamed) };
    if r["status"] != "SUCCESS" {
        let why = r["error"].as_str().filter(|e| !e.is_empty()).unwrap_or("no response");
        return Err(tail(&format!("{} ({})", why, r["status"].as_str().unwrap_or("ERROR"))));
    }
    let text = r["response"].as_str().filter(|t| !t.trim().is_empty()).map(str::to_string).unwrap_or(streamed);
    if text.trim().is_empty() {
        if let Some(denied) = r["denied_actions"].as_array().filter(|d| !d.is_empty()) {
            let names: Vec<&str> = denied.iter().filter_map(|d| d["display_name"].as_str()).collect();
            return Err(format!(
                "Gemini tried to use a tool its access doesn't allow ({}) and wrote no reply. Ask again, or raise its access in Settings.",
                names.join(", ")
            ));
        }
    }
    Ok(text)
}

/// The last few lines of a tool's output, for error messages.
fn tail(s: &str) -> String {
    let lines: Vec<&str> = s.trim().lines().collect();
    let start = lines.len().saturating_sub(6);
    let t = lines[start..].join("\n");
    if t.is_empty() {
        "(no output)".into()
    } else {
        t
    }
}

async fn kill_tree(pid: Option<u32>, child: &mut tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pid) = pid {
        // The child leads its own process group (process_group(0)), so this reaches its children too.
        unsafe {
            libc::killpg(pid as i32, libc::SIGTERM);
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
        unsafe {
            libc::killpg(pid as i32, libc::SIGKILL);
        }
    }
    #[cfg(windows)]
    if let Some(pid) = pid {
        // Async, and time-limited, so a slow taskkill can't hold up Stop.
        let mut tk = Command::new("taskkill");
        tk.args(["/PID", &pid.to_string(), "/T", "/F"]).stdout(Stdio::null()).stderr(Stdio::null());
        {
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            tk.creation_flags(CREATE_NO_WINDOW);
        }
        let _ = tokio::time::timeout(Duration::from_secs(5), tk.status()).await;
    }
    let _ = tokio::time::timeout(Duration::from_secs(2), child.kill()).await;
}

#[cfg(test)]
mod plan_tests {
    use super::*;

    fn args(kind: Kind, access: Access) -> Vec<String> {
        plan(kind, access, Path::new("/ws"), Path::new("/r.txt"), "SYS", "P")
            .args
            .iter()
            .map(|a| a.to_string_lossy().to_string())
            .collect()
    }

    fn has(v: &[String], pair: &[&str]) -> bool {
        v.windows(pair.len()).any(|w| w.iter().zip(pair).all(|(a, b)| a == b))
    }

    fn pct(m: &Option<UsageMeter>) -> (String, u8, bool) {
        let m = m.as_ref().expect("a meter");
        (m.label.clone(), m.remaining_percent, m.shared)
    }

    #[test]
    fn codex_usage_shows_the_model_and_the_whole_plan() {
        let limits: serde_json::Value = serde_json::from_str(r#"{"id":2,"result":{"rateLimits":{"limitId":"codex","primary":{"usedPercent":37,"windowDurationMins":300},"secondary":{"usedPercent":82,"windowDurationMins":10080,"resetsAt":1795140954}}}}"#).unwrap();
        let models: serde_json::Value = serde_json::from_str(r#"{"id":3,"result":{"data":[{"id":"gpt-6.1-sol","model":"gpt-6.1-sol","displayName":"GPT-6.1-Sol","isDefault":true},{"id":"gpt-6-astra","model":"gpt-6-astra","displayName":"GPT-6-Astra"}]}}"#).unwrap();
        // The default model, which has no limit of its own.
        let usage = codex_usage_from(&limits, &models, None).unwrap();
        assert_eq!(usage.remaining_percent, 18);
        assert_eq!(pct(&usage.overall), ("ChatGPT".into(), 18, false));
        assert_eq!(pct(&usage.model), ("GPT-6.1-Sol".into(), 18, true));
        assert_eq!(
            usage.windows,
            vec![
                UsageWindow { label: "5-hour limit".into(), remaining_percent: 63, resets_at: None },
                UsageWindow { label: "Weekly limit".into(), remaining_percent: 18, resets_at: Some(1_795_140_954_000) },
            ]
        );
        // Switched to a model with a bucket of its own.
        let limits: serde_json::Value = serde_json::from_str(r#"{"id":2,"result":{"rateLimitsByLimitId":{"codex":{"primary":{"usedPercent":10,"windowDurationMins":10080}},"astra":{"normalModelSlug":"gpt-6-astra","primary":{"usedPercent":75,"windowDurationMins":10080}}}}}"#).unwrap();
        let usage = codex_usage_from(&limits, &models, Some("gpt-6-astra")).unwrap();
        assert_eq!(pct(&usage.model), ("GPT-6-Astra".into(), 25, false));
        assert_eq!(pct(&usage.overall), ("ChatGPT".into(), 90, false));
        assert_eq!(usage.remaining_percent, 25);
    }

    const CLAUDE: &str = r#"{"type":"control_response","response":{"response":{"rate_limits_available":true,"rate_limits":{"limits":[{"kind":"session","percent":22,"scope":null},{"kind":"weekly_all","percent":59,"scope":null},{"kind":"weekly_scoped","percent":100,"is_active":ACTIVE_FABLE,"scope":{"model":{"display_name":"Fable"}}},{"kind":"weekly_scoped","percent":90,"is_active":false,"scope":{"model":{"display_name":"Sonnet"}}}]}}}}"#;

    #[test]
    fn claude_usage_follows_the_model_in_use() {
        // On Fable, which has a weekly limit of its own: Fable's meter, and Claude's overall one.
        let usage = claude_usage_from_output(&CLAUDE.replace("ACTIVE_FABLE", "true"), None).unwrap();
        assert_eq!(pct(&usage.model), ("Fable".into(), 0, false));
        assert_eq!(pct(&usage.overall), ("Claude".into(), 41, false));
        assert_eq!(usage.remaining_percent, 0);
        // Switched to Opus, which draws on the overall limits: its meter shows those.
        let usage = claude_usage_from_output(&CLAUDE.replace("ACTIVE_FABLE", "false"), Some("Opus 5.5".into())).unwrap();
        assert_eq!(pct(&usage.model), ("Opus 5.5".into(), 41, true));
        assert_eq!(pct(&usage.overall), ("Claude".into(), 41, false));
        assert_eq!(usage.remaining_percent, 41);
        assert_eq!(usage.windows.len(), 2, "an inactive model's limit doesn't count");
        // Model unknown: the overall meter alone.
        let usage = claude_usage_from_output(&CLAUDE.replace("ACTIVE_FABLE", "false"), None).unwrap();
        assert!(usage.model.is_none() && usage.overall.is_some());
    }

    #[test]
    fn reset_times_are_read_in_each_providers_format() {
        assert_eq!(iso_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(iso_ms("2026-11-17T19:00:00.389055+00:00"), Some(1_794_942_000_389));
        assert_eq!(iso_ms("2026-11-17T20:00:00+01:00"), Some(1_794_942_000_000));
        assert_eq!(iso_ms("2026-11-22T00:24:48Z"), Some(1_795_307_088_000));
        assert_eq!(iso_ms("soon"), None);
        let usage = claude_usage_from_output(&CLAUDE.replace("ACTIVE_FABLE", "true").replace(r#""percent":100,"#, r#""percent":100,"resets_at":"2026-11-17T19:00:00Z","#), None).unwrap();
        assert_eq!(usage.model.unwrap().windows[0].resets_at, Some(1_794_942_000_000));
    }

    #[test]
    fn model_ids_get_readable_names() {
        assert_eq!(model_name("claude-opus-5-5"), "Opus 5.5");
        assert_eq!(model_name("claude-fable-5-2[1m]"), "Fable 5.2");
        assert_eq!(model_name("claude-sonnet-4-5-20250929"), "Sonnet 4.5");
        assert_eq!(model_name("opus"), "Opus");
    }

    #[test]
    fn a_claude_reply_names_its_main_model() {
        let out = r#"{"result":"hi","modelUsage":{"claude-haiku-5-5":{"outputTokens":12},"claude-opus-5-5":{"outputTokens":900}}}"#;
        assert_eq!(reply_model(Kind::ClaudeCode, out).as_deref(), Some("claude-opus-5-5"));
        assert_eq!(reply_model(Kind::Codex, out), None);
    }

    #[test]
    fn antigravity_usage_is_the_gemini_group_and_ignores_the_third_party_pool() {
        let output = r#"{"command":{"data":{"groups":[{"name":"Gemini Models","description":"Models within this group: Gemini 3.8 Flash","buckets":[{"name":"Weekly Limit Remaining","remaining_fraction":0.61},{"name":"Five Hour Limit Remaining","remaining_fraction":0.83}]},{"name":"Claude and GPT models","buckets":[{"name":"Weekly Limit Remaining","remaining_fraction":0.1}]}]}}}"#;
        let usage = antigravity_usage_from_output(output).unwrap();
        assert_eq!(usage.remaining_percent, 61);
        assert_eq!(usage.windows.len(), 2);
        assert_eq!(usage.windows[0].label, "Weekly Limit");
        assert_eq!(pct(&usage.overall), ("Gemini".into(), 61, false));
        assert_eq!(pct(&usage.model), ("Gemini 3.8 Flash".into(), 61, true));
    }

    #[test]
    fn antigravity_usage_tolerates_stderr_output_around_json() {
        let output = "info: checking CLI updates\n{\"command\":{\"data\":{\"groups\":[{\"name\":\"Gemini Models\",\"buckets\":[{\"name\":\"Weekly Limit Remaining\",\"remaining_fraction\":0.75}]}]}}}\nwarn: update available\n";
        let usage = antigravity_usage_from_output(output).unwrap();
        assert_eq!(usage.remaining_percent, 75);
    }

    #[test]
    fn turn_token_counts_are_read_from_each_provider_schema() {
        let codex = token_usage_from_output(
            Kind::Codex,
            "{\"type\":\"turn.started\"}\n{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":120,\"cached_input_tokens\":80,\"output_tokens\":30}}",
        )
        .unwrap();
        assert_eq!(codex.total_tokens, 150);
        assert_eq!(codex.cache_read_tokens, 80);

        let claude = token_usage_from_output(
            Kind::ClaudeCode,
            r#"{"type":"result","usage":{"input_tokens":10,"cache_creation_input_tokens":20,"cache_read_input_tokens":30,"output_tokens":40}}"#,
        )
        .unwrap();
        assert_eq!(claude.total_tokens, 100);
        assert_eq!(claude.cache_creation_tokens, 20);

        let antigravity = token_usage_from_output(
            Kind::Antigravity,
            r#"{"event":"result","result":{"usage":{"input_tokens":12,"output_tokens":5,"thinking_tokens":7,"cache_read_tokens":3,"total_tokens":24}}}"#,
        )
        .unwrap();
        assert_eq!(antigravity.total_tokens, 24);
        assert_eq!(antigravity.thinking_tokens, 7);
    }

    #[test]
    fn chat_access_turns_tools_off_everywhere() {
        assert!(args(Kind::Codex, Access::Chat).contains(&"--json".to_string()));
        assert!(has(&args(Kind::Codex, Access::Chat), &["-s", "read-only"]));
        assert!(has(&args(Kind::ClaudeCode, Access::Chat), &["--tools", ""]));
        assert!(has(&args(Kind::GeminiCli, Access::Chat), &["--approval-mode", "plan"]));
    }

    #[test]
    fn edit_access_allows_file_edits_but_not_shell() {
        assert!(has(&args(Kind::Codex, Access::Edit), &["-s", "workspace-write"]));
        let c = args(Kind::ClaudeCode, Access::Edit);
        assert!(has(&c, &["--tools", "Read,Grep,Glob,Edit,Write"]));
        assert!(has(&c, &["--permission-mode", "acceptEdits"]));
        assert!(!c.iter().any(|a| a.contains("Bash")));
        assert!(has(&args(Kind::GeminiCli, Access::Edit), &["--approval-mode", "auto_edit"]));
    }

    #[test]
    fn unrestricted_access_uses_each_tools_own_bypass_switch() {
        assert!(args(Kind::Codex, Access::Unrestricted).contains(&"--dangerously-bypass-approvals-and-sandbox".to_string()));
        assert!(args(Kind::ClaudeCode, Access::Unrestricted).contains(&"--dangerously-skip-permissions".to_string()));
        assert!(args(Kind::Antigravity, Access::Unrestricted).contains(&"--dangerously-skip-permissions".to_string()));
        assert!(has(&args(Kind::GeminiCli, Access::Unrestricted), &["--approval-mode", "yolo"]));
        for k in [Kind::Codex, Kind::ClaudeCode, Kind::Antigravity, Kind::GeminiCli] {
            assert!(!args(k, Access::Edit).iter().any(|a| a.contains("dangerously") || a == "yolo"));
        }
    }

    #[test]
    fn own_access_adds_no_permission_flags() {
        let x = args(Kind::Codex, Access::Own);
        assert!(!x.iter().any(|a| a == "-s"));
        let c = args(Kind::ClaudeCode, Access::Own);
        assert!(!c.iter().any(|a| a == "--tools" || a == "--permission-mode"));
        let g = args(Kind::GeminiCli, Access::Own);
        assert!(!g.iter().any(|a| a == "--approval-mode"));
    }

    #[test]
    fn antigravity_sends_the_conversation_on_stdin_with_matching_modes() {
        let p = plan(Kind::Antigravity, Access::Chat, Path::new("/ws"), Path::new("/r"), "SYS", "P");
        let a: Vec<String> = p.args.iter().map(|x| x.to_string_lossy().to_string()).collect();
        assert!(has(&a, &["--input-format", "stream-json"]) && has(&a, &["--output-format", "stream-json"]));
        assert!(has(&a, &["--mode", "plan"]) && has(&a, &["-p", ""]));
        let line: serde_json::Value = serde_json::from_str(p.input.trim()).unwrap();
        assert_eq!(line["event"], "user");
        assert_eq!(line["message"]["content"], "SYS\n\nP");
        assert!(has(&args(Kind::Antigravity, Access::Edit), &["--mode", "accept-edits"]));
        assert!(!args(Kind::Antigravity, Access::Own).iter().any(|x| x == "--mode"));
    }

    #[test]
    fn antigravity_reply_falls_back_to_streamed_text_and_explains_blocked_tools() {
        let streamed = r#"{"event":"step_update","step_update":{"text_delta":"Hello "}}
{"event":"step_update","step_update":{"text_delta":"council"}}
{"event":"result","result":{"status":"SUCCESS","response":""}}"#;
        assert_eq!(antigravity_reply(streamed).unwrap(), "Hello council");
        let blocked = r#"{"event":"result","result":{"status":"SUCCESS","response":"","denied_actions":[{"action":"command","display_name":"Bash"}]}}"#;
        let e = antigravity_reply(blocked).unwrap_err();
        assert!(e.contains("Bash") && e.contains("access"), "{e}");
        let failed = r#"{"event":"result","result":{"status":"ERROR","response":"","error":"authentication failed or timed out"}}"#;
        assert_eq!(antigravity_reply(failed).unwrap_err(), "authentication failed or timed out (ERROR)");
    }

    #[test]
    fn instructions_reach_every_tool() {
        let p = plan(Kind::ClaudeCode, Access::Chat, Path::new("/ws"), Path::new("/r"), "SYS", "P");
        assert!(p.args.iter().any(|a| a == "SYS") && p.input == "P");
        let p = plan(Kind::GeminiCli, Access::Chat, Path::new("/ws"), Path::new("/r"), "SYS", "P");
        assert!(p.input.starts_with("SYS") && p.input.ends_with("P"));
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// Writes a fake CLI (a shell script) and returns its path.
    fn fake(body: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("consilium-fake-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("tool");
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    async fn go(p: &Path, kind: Kind, access: Access, ws: Option<&Path>, system: &str) -> Result<String, String> {
        run_at(p, kind, access, ws, system, "convo", &CancellationToken::new(), RUN_TIMEOUT).await
    }

    #[tokio::test]
    async fn codex_reply_comes_from_the_output_file_and_runs_in_an_empty_folder() {
        let p = fake(
            r#"while [ "$1" != "-o" ]; do shift; done; out="$2"
input=$(cat)
printf 'cwd=%s files=%s first=%s' "$(basename "$PWD")" "$(ls -A | wc -l | tr -d ' ')" "$(printf '%s' "$input" | head -1)" > "$out""#,
        );
        let r = go(&p, Kind::Codex, Access::Chat, None, "SYSTEM LINE").await.unwrap();
        assert_eq!(r, "cwd=empty files=0 first=SYSTEM LINE");
    }

    #[tokio::test]
    async fn project_folder_is_used_only_above_chat_access() {
        let ws = std::env::temp_dir().join(format!("consilium-ws-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(ws.join("README.md"), "x").unwrap();
        let p = fake(r#"while [ "$1" != "-o" ]; do shift; done; cat >/dev/null; ls -A > "$2""#);
        assert_eq!(go(&p, Kind::Codex, Access::Edit, Some(&ws), "s").await.unwrap(), "README.md");
        assert!(go(&p, Kind::Codex, Access::Chat, Some(&ws), "s").await.is_err(), "chat access must not see the project");
        std::fs::remove_dir_all(ws).unwrap();
    }

    #[tokio::test]
    async fn claude_reply_is_parsed_from_json_and_errors_are_reported() {
        let p = fake(r#"cat >/dev/null; printf '{"type":"result","is_error":false,"result":"hello from claude"}'"#);
        assert_eq!(go(&p, Kind::ClaudeCode, Access::Chat, None, "s").await.unwrap(), "hello from claude");
        let p = fake(r#"cat >/dev/null; printf '{"type":"result","is_error":true,"result":"Not logged in"}'"#);
        assert_eq!(go(&p, Kind::ClaudeCode, Access::Chat, None, "s").await.unwrap_err(), "Not logged in");
    }

    #[tokio::test]
    async fn gemini_reply_and_errors_are_parsed_from_json() {
        let p = fake(r#"cat >/dev/null; printf '{"session_id":"x","response":"hello from gemini","stats":{}}'"#);
        assert_eq!(go(&p, Kind::GeminiCli, Access::Chat, None, "s").await.unwrap(), "hello from gemini");
        let p = fake(r#"cat >/dev/null; printf '{"session_id":"x","error":{"type":"Error","message":"Please set an Auth method","code":41}}'; exit 41"#);
        assert_eq!(go(&p, Kind::GeminiCli, Access::Chat, None, "s").await.unwrap_err(), "Please set an Auth method");
    }

    #[tokio::test]
    async fn antigravity_result_event_is_parsed_and_errors_reported() {
        let p = fake(
            r#"cat >/dev/null
echo '{"event":"init","init":{"cwd":"x"}}'
echo '{"event":"result","result":{"status":"SUCCESS","response":"hello from gemini","error":""}}'"#,
        );
        assert_eq!(go(&p, Kind::Antigravity, Access::Chat, None, "s").await.unwrap(), "hello from gemini");
        let p = fake(
            r#"cat >/dev/null
echo '{"event":"result","result":{"status":"ERROR","response":"","error":"authentication failed or timed out"}}'
exit 1"#,
        );
        assert_eq!(
            go(&p, Kind::Antigravity, Access::Chat, None, "s").await.unwrap_err(),
            "authentication failed or timed out (ERROR)"
        );
    }

    #[tokio::test]
    async fn tool_folder_is_put_on_path_for_node_based_clis() {
        let p = fake(r#"cat >/dev/null; printf '{"response":"%s"}' "$(echo "$PATH" | cut -d: -f1)""#);
        let dir = p.parent().unwrap().display().to_string();
        assert_eq!(go(&p, Kind::GeminiCli, Access::Chat, None, "s").await.unwrap(), dir);
    }

    #[tokio::test]
    async fn arguments_are_passed_without_a_shell() {
        // A system prompt full of shell syntax must arrive as one literal argument.
        let p = fake(r#"cat >/dev/null; for a in "$@"; do last="$a"; done; printf '{"result":"%s"}' "$last""#);
        let tricky = "x; touch /tmp/consilium-pwned $(id) `id`";
        assert_eq!(go(&p, Kind::ClaudeCode, Access::Chat, None, tricky).await.unwrap(), tricky);
        assert!(!Path::new("/tmp/consilium-pwned").exists());
    }

    #[tokio::test]
    async fn stop_kills_the_whole_process_group() {
        let marker = std::env::temp_dir().join(format!("consilium-child-{}", uuid::Uuid::new_v4()));
        // The tool starts a grandchild that would create a file after 2 seconds.
        let p = fake(&format!("(sleep 2; touch '{}') & sleep 30", marker.display()));
        let cancel = CancellationToken::new();
        let c2 = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            c2.cancel();
        });
        let started = std::time::Instant::now();
        let r = run_at(&p, Kind::Codex, Access::Chat, None, "s", "p", &cancel, RUN_TIMEOUT).await;
        assert_eq!(r.unwrap_err(), "stopped");
        assert!(started.elapsed() < Duration::from_secs(3));
        tokio::time::sleep(Duration::from_millis(2500)).await;
        assert!(!marker.exists(), "the grandchild survived Stop");
    }

    #[tokio::test]
    async fn a_background_program_holding_the_output_open_does_not_hang_the_turn() {
        // The CLI exits, but a program it started keeps stdout open for 30 seconds.
        let p = fake(r#"cat >/dev/null; (sleep 30) & printf '{"result":"done"}'"#);
        let started = std::time::Instant::now();
        assert_eq!(go(&p, Kind::ClaudeCode, Access::Chat, None, "s").await.unwrap(), "done");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[tokio::test]
    async fn time_limit_applies() {
        let p = fake("sleep 30");
        let r = run_at(&p, Kind::Codex, Access::Chat, None, "s", "p", &CancellationToken::new(), Duration::from_millis(500)).await;
        assert!(r.unwrap_err().contains("no answer within"));
    }

    #[tokio::test]
    async fn failures_report_the_tool_output() {
        let p = fake("cat >/dev/null; echo 'Error: not logged in' >&2; exit 3");
        let e = go(&p, Kind::Codex, Access::Chat, None, "s").await.unwrap_err();
        assert!(e.contains("exit 3") && e.contains("not logged in"), "{e}");
    }
}
