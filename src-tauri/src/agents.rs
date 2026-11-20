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
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub const RUN_TIMEOUT: Duration = Duration::from_secs(900);
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Codex,
    ClaudeCode,
    GeminiCli,
    Antigravity,
    /// Grok through xAI's OpenAI-compatible API, with the person's own key (see `apiagent`).
    XaiApi,
    /// DeepSeek through its OpenAI-compatible API, with the person's own key.
    DeepSeekApi,
    /// An AI the person added: any OpenAI-compatible API or a local model (see `customai`).
    CustomApi,
}

/// Whether the agent is reached over an HTTP API rather than a command-line tool.
pub fn is_api(kind: Kind) -> bool {
    matches!(kind, Kind::XaiApi | Kind::DeepSeekApi | Kind::CustomApi)
}

/// The HTTP provider behind an API agent, or None for the CLI agents.
pub fn provider_of(agent: &Agent) -> Option<&'static crate::apiagent::Provider> {
    agent.provider.or_else(|| api_provider(agent.kind))
}

/// The HTTP provider behind an API-key agent, or None for the CLI agents.
pub fn api_provider(kind: Kind) -> Option<&'static crate::apiagent::Provider> {
    match kind {
        Kind::XaiApi => Some(&crate::apiagent::XAI),
        Kind::DeepSeekApi => Some(&crate::apiagent::DEEPSEEK),
        _ => None,
    }
}

/// Whether the agent can be started at all: its CLI is installed, or its API key is set.
pub fn installed(agent: &Agent) -> bool {
    match provider_of(agent) {
        Some(p) => crate::apiagent::has_key(agent.id, p),
        None => find(effective_kind(agent)).is_some(),
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentInfo {
    pub id: String,
    pub name: String,
    pub company: String,
    /// Always "local-cli" for now; API-backed agents will say "api".
    pub transport: String,
    /// An AI the person added themselves (it can be deleted).
    pub custom: bool,
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
    /// Whether `benched_until` came from the AI itself (not Consilium's retry guess).
    #[serde(default)]
    pub reset_known: bool,
    /// The model chosen in its council box (#29), or None for the CLI's own default.
    pub model: Option<String>,
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
    /// Set only for AIs the person added; the built-in API AIs get theirs from `kind`.
    pub provider: Option<&'static crate::apiagent::Provider>,
}

pub const AGENTS: [Agent; 5] = [
    Agent { id: "gpt", name: "ChatGPT", company: "OpenAI", kind: Kind::Codex, provider: None },
    Agent { id: "claude", name: "Claude", company: "Anthropic", kind: Kind::ClaudeCode, provider: None },
    Agent { id: "gemini", name: "Gemini", company: "Google", kind: Kind::Antigravity, provider: None },
    Agent { id: "grok", name: "Grok", company: "xAI", kind: Kind::XaiApi, provider: None },
    Agent { id: "deepseek", name: "DeepSeek", company: "DeepSeek", kind: Kind::DeepSeekApi, provider: None },
];

/// A built-in AI only.
pub fn builtin(id: &str) -> Option<&'static Agent> {
    AGENTS.iter().find(|a| a.id == id)
}

/// Every AI Consilium knows: the built-in ones, then the ones the person added.
pub fn all() -> Vec<&'static Agent> {
    AGENTS.iter().chain(crate::customai::list()).collect()
}

pub fn by_id(id: &str) -> Option<&'static Agent> {
    builtin(id).or_else(|| crate::customai::by_id(id))
}

/// Whether this agent is a local offline model (Ollama, llama.cpp, etc.) rather than a cloud model.
pub fn is_local(agent: &Agent) -> bool {
    agent.provider.is_some_and(|p| p.local)
}

/// Whether the agent identified by `id` is a local offline model.
pub fn is_local_id(id: &str) -> bool {
    by_id(id).is_some_and(is_local)
}

fn base_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Codex => "codex",
        Kind::ClaudeCode => "claude",
        Kind::GeminiCli => "gemini",
        Kind::Antigravity => "agy",
        Kind::XaiApi | Kind::DeepSeekApi | Kind::CustomApi => "",
    }
}

fn exe_names(base: &str) -> Vec<String> {
    if cfg!(windows) {
        vec![
            format!("{base}.exe"),
            format!("{base}.cmd"),
            format!("{base}.bat"),
            base.to_string(),
        ]
    } else {
        vec![base.to_string()]
    }
}

/// Apps started from the Dock or Start menu don't get the shell's PATH, so well-known install
/// locations are checked as well as PATH.
fn candidates(kind: Kind) -> Vec<PathBuf> {
    if is_api(kind) {
        return Vec::new(); // reached over HTTP, there is no program to find
    }
    let home = dirs::home_dir().unwrap_or_default();
    // The human's current search path and the usual tool folders, not just the PATH this copy
    // was started with, so an AI installed or added to PATH since is still found after an update.
    let dirs_: Vec<PathBuf> = crate::searchpath::dirs();
    let mut out: Vec<PathBuf> = Vec::new();
    match kind {
        Kind::Codex => {
            // The ChatGPT desktop app ships a copy of Codex.
            for app in ["/Applications/ChatGPT.app", "/Applications/Codex.app"] {
                out.push(PathBuf::from(app).join("Contents/Resources/codex-cli/bin/codex"));
                out.push(home.join(app.trim_start_matches('/')).join("Contents/Resources/codex-cli/bin/codex"));
            }
            out.push(PathBuf::from("/Applications/Codex.app/Contents/Resources/codex"));
            for n in exe_names("codex") {
                out.push(home.join(".codex/bin").join(&n));
            }
            let mut win_bases = Vec::new();
            if let Some(local) = std::env::var_os("LOCALAPPDATA") {
                let p = PathBuf::from(local);
                win_bases.push(p.join("Programs").join("ChatGPT"));
                win_bases.push(p.join("ChatGPT"));
                win_bases.push(p.join("Programs").join("OpenAI").join("ChatGPT"));
                win_bases.push(p.join("OpenAI").join("ChatGPT"));
                win_bases.push(p.join("Programs").join("Codex"));
                win_bases.push(p.join("Programs").join("OpenAI").join("Codex"));
                win_bases.push(p.join("OpenAI").join("Codex"));
            }
            if let Some(pf) = std::env::var_os("ProgramFiles") {
                let p = PathBuf::from(pf);
                win_bases.push(p.join("ChatGPT"));
                win_bases.push(p.join("OpenAI").join("ChatGPT"));
                win_bases.push(p.join("Codex"));
            }
            if let Some(pfx86) = std::env::var_os("ProgramFiles(x86)") {
                let p = PathBuf::from(pfx86);
                win_bases.push(p.join("ChatGPT"));
                win_bases.push(p.join("OpenAI").join("ChatGPT"));
                win_bases.push(p.join("Codex"));
            }
            for base in win_bases {
                for n in exe_names("codex") {
                    out.push(base.join("resources").join("codex-cli").join("bin").join(&n));
                    out.push(base.join("resources").join("codex").join("bin").join(&n));
                    out.push(base.join("resources").join("codex").join(&n));
                    out.push(base.join("bin").join(&n));
                    out.push(base.join(&n));
                }
            }
        }
        Kind::ClaudeCode => {
            for n in exe_names("claude") {
                out.push(home.join(".claude/local").join(&n));
            }
        }
        Kind::GeminiCli | Kind::XaiApi | Kind::DeepSeekApi | Kind::CustomApi => {}
        Kind::Antigravity => {
            for n in exe_names("agy") {
                out.push(home.join(".local/bin").join(&n));
            }
            if let Some(local) = std::env::var_os("LOCALAPPDATA") {
                let p = PathBuf::from(local);
                out.push(p.join("agy").join("bin").join("agy.exe"));
                out.push(p.join("Programs").join("Antigravity").join("bin").join("agy.exe"));
            }
            if let Some(pf) = std::env::var_os("ProgramFiles") {
                out.push(PathBuf::from(pf).join("Antigravity").join("bin").join("agy.exe"));
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
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // Keep programs launched by an agent from holding output in a language-runtime buffer. The
    // provider CLIs already use their machine-readable streaming modes; these variables cover
    // Python and common color-aware tools they start without changing the JSON protocol with a
    // pseudo-terminal.
    cmd.env("PYTHONUNBUFFERED", "1")
        .env("NO_COLOR", "1")
        .env("FORCE_COLOR", "0")
        .env("CLICOLOR", "0");
    // Node-based CLIs (Gemini, npm installs) start with `#!/usr/bin/env node`, and apps launched from
    // the Dock have a minimal PATH; putting the tool's own folder first lets it find its runtime.
    if let Some(dir) = path.parent() {
        let mut parts: Vec<PathBuf> = vec![dir.to_path_buf()];
        // The human's current search path, so the tools an AI uses are there after an update too.
        parts.extend(crate::searchpath::dirs());
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
        Kind::GeminiCli | Kind::XaiApi | Kind::DeepSeekApi | Kind::CustomApi => None,
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
    // Roaming and local app data on Windows; Application Support on macOS; ~/.config and
    // ~/.local/share on Linux; ~/.antigravity, ~/.gemini, or ~/.config/antigravity fallback.
    let home = dirs::home_dir();
    let dot_antigravity = home.as_ref().map(|h| h.join(".antigravity"));
    let dot_config = home.as_ref().map(|h| h.join(".config").join("antigravity"));
    let dot_gemini = home.as_ref().map(|h| h.join(".gemini"));
    let dot_gemini_cli = home.as_ref().map(|h| h.join(".gemini").join("antigravity-cli"));
    let appdata = std::env::var_os("APPDATA").map(PathBuf::from);
    let localappdata = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    [
        dirs::config_dir(),
        dirs::data_dir(),
        dirs::data_local_dir(),
        appdata,
        localappdata,
        dot_antigravity,
        dot_config,
        dot_gemini,
        dot_gemini_cli,
    ]
    .into_iter()
    .flatten()
    .any(|d| {
        d.join("antigravity").join("credential-user-scope").is_file()
            || d.join("credential-user-scope").is_file()
            || d.join("oauth_creds.json").is_file()
    })
}

/// Whether the AI's CLI reports a saved sign-in, or None when it couldn't say.
async fn signed_in_via(kind: Kind, path: &Path) -> Option<bool> {
    match kind {
        Kind::Codex => probe(path, &["login", "status"]).await.map(|s| s.contains("Logged in")),
        Kind::ClaudeCode => probe(path, &["auth", "status"]).await.and_then(|s| {
            let start = s.find('{')?;
            let end = s.rfind('}')? + 1;
            serde_json::from_str::<serde_json::Value>(&s[start..end]).ok().and_then(|v| v["loggedIn"].as_bool())
        }),
        Kind::GeminiCli => Some(gemini_signed_in()),
        Kind::Antigravity => Some(antigravity_signed_in()),
        Kind::XaiApi | Kind::DeepSeekApi | Kind::CustomApi => None,
    }
}

pub async fn status(agent: &Agent, member: bool, access: Access) -> AgentInfo {
    if let Some(provider) = provider_of(agent) {
        return api_status(agent, provider, member).await;
    }
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
        Kind::XaiApi | Kind::DeepSeekApi | Kind::CustomApi => unreachable!("API agents return above"),
    };
    let mut info = AgentInfo {
        id: agent.id.into(),
        name: agent.name.into(),
        company: agent.company.into(),
        transport: "local-cli".into(),
        custom: false,
        tool: tool.into(),
        member,
        access,
        available: false,
        signed_in: None,
        version: None,
        path: None,
        hint: install_hint.into(),
        benched_until: None,
        reset_known: false,
        model: None,
    };
    let Some(path) = find(kind) else { return info };
    info.path = Some(path.display().to_string());
    // The version and sign-in checks are independent, so they run at the same time.
    let version_probe = probe(&path, &["--version"]);
    let sign_in_probe = signed_in_via(kind, &path);
    let (version, mut signed_in) = tokio::join!(version_probe, sign_in_probe);
    // A CLI that couldn't answer (slow start while several launch together, a busy keychain) is
    // asked once more instead of being reported as signed out, which sent people to sign in again.
    if signed_in.is_none() && matches!(kind, Kind::Codex | Kind::ClaudeCode) {
        signed_in = signed_in_via(kind, &path).await;
    }
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
/// Status of an API-key agent. "path" carries the provider address once a key is set, so the
/// frontend shows it as needing a key rather than an install. Its access is always "chat": an
/// HTTP chat API has no tools. "version" carries the model chosen from the account's list.
async fn api_status(agent: &Agent, provider: &crate::apiagent::Provider, member: bool) -> AgentInfo {
    let mut info = AgentInfo {
        id: agent.id.into(),
        name: agent.name.into(),
        company: agent.company.into(),
        transport: "api".into(),
        custom: matches!(agent.kind, Kind::CustomApi),
        tool: format!("{} API", agent.company),
        member,
        access: Access::Chat,
        available: false,
        signed_in: Some(false),
        version: None,
        path: None,
        hint: if provider.local {
            format!("Start your model server at {} and press Check again.", provider.base_url)
        } else {
            format!("Create an API key at {} and paste it here. It stays on this computer.", provider.key_page)
        },
        benched_until: None,
        reset_known: false,
        model: None,
    };
    if !crate::apiagent::has_key(agent.id, provider) {
        return info;
    }
    info.path = Some(provider.base_url.into());
    match crate::apiagent::check(agent.id, agent.name, provider).await {
        Ok(model) => {
            info.version = Some(model);
            info.signed_in = Some(true);
            info.available = true;
            info.hint = String::new();
        }
        Err(e) => info.hint = e,
    }
    info
}

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

/// One provider-reported description of what an AI is doing. This deliberately exposes only
/// observable tool/phase telemetry, never hidden reasoning text or chain-of-thought.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivitySignal {
    /// Stable semantic category used for colour and icon choices in the UI.
    pub kind: String,
    /// Short provider-native action label (for example "Running command").
    pub label: String,
    /// Optional concise native detail, such as the tool name or search query.
    pub detail: String,
}

pub type ActivitySink = Arc<dyn Fn(ActivitySignal) + Send + Sync>;

/// One provider turn, including token accounting when the CLI reports it.
pub struct AgentReply {
    pub text: String,
    pub usage: Option<TokenUsage>,
    /// The model that answered, when the CLI says (Claude Code does).
    pub model: Option<String>,
}

/// The exact command-line arguments for one turn. `workdir` is the project folder when the AI has
/// access to one, otherwise an empty temporary folder; `access` is already "chat" in that case.
#[cfg(test)]
pub fn plan(kind: Kind, access: Access, workdir: &Path, reply_file: &Path, system: &str, prompt: &str) -> Plan {
    plan_with_files(kind, access, workdir, reply_file, system, prompt, &crate::attach::Files::default())
}

/// `plan`, letting the AI open the conversation's attached files: Codex gets the pictures on its
/// command line; Claude Code, Antigravity and the Gemini CLI may read the files folder. (Claude
/// Code, otherwise without tools in "chat", gets its Read tool for that folder alone.)
pub fn plan_with_files(
    kind: Kind,
    access: Access,
    workdir: &Path,
    reply_file: &Path,
    system: &str,
    prompt: &str,
    files: &crate::attach::Files,
) -> Plan {
    let mut args: Vec<OsString> = Vec::new();
    let dir = files.dir.as_ref();
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
            for image in &files.images {
                // The `=` form: `--image` takes several values and would swallow the prompt's "-".
                let mut a = OsString::from("--image=");
                a.push(image);
                args.push(a);
            }
            let mut push = |a: &str| args.push(a.into());
            push("-C");
            args.push(workdir.into());
            args.push("-o".into());
            args.push(reply_file.into());
            args.push("-".into());
            format!("{system}\n\n{prompt}")
        }
        Kind::ClaudeCode => {
            // Stream JSON preserves Claude Code's native tool-use events so the app can show what
            // it is doing. The final `result` event still contains the reply and usage data.
            for a in ["-p", "--output-format", "stream-json", "--verbose", "--no-session-persistence"] {
                push(a);
            }
            match access {
                Access::Chat => {
                    push("--tools");
                    push(if dir.is_some() { "Read" } else { "" });
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
            if let Some(d) = dir {
                push("--add-dir");
                args.push(d.into());
            }
            args.push("--append-system-prompt".into());
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
            if let Some(d) = dir {
                push("--include-directories");
                args.push(d.into());
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
                // Antigravity's print mode (-p) with --dangerously-skip-permissions denies shell
                // commands and outside-workspace reads ("denied_actions: Bash / ReadFile") on both
                // Windows and macOS. Explicit allow rules (below) run them, so the flag isn't used.
                Access::Unrestricted => {}
            }
            // Print mode can't ask, so Antigravity denies any file outside the workspace that
            // needs a permission ("denied_actions: ReadFile"). Grant reading (and, at Unrestricted,
            // writing) for this run only; `--settings` is never saved to the user's settings.json.
            if let Some(rules) = antigravity_allow_rules(access) {
                push("--settings");
                push(&serde_json::json!({ "permissions": { "allow": rules } }).to_string());
            }
            if let Some(d) = dir {
                push("--add-dir");
                args.push(d.into());
            }
            let mut push = |a: &str| args.push(a.into());
            push("-p");
            push("");
            let message = serde_json::json!({
                "event": "user",
                "message": { "content": format!("{system}\n\n{prompt}") }
            });
            format!("{message}\n")
        }
        Kind::XaiApi | Kind::DeepSeekApi | Kind::CustomApi => String::new(), // not a program; see `apiagent::run`
    };
    Plan { args, input }
}

/// One entry in the council-box model picker (#29).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ModelChoice {
    pub id: String,
    pub name: String,
}

/// The models an AI's CLI offers, for the picker. Empty means only the CLI's default is known
/// (API agents, the Gemini CLI, and Codex until its `model/list` is wired in).
pub async fn list_models(agent: &Agent) -> Vec<ModelChoice> {
    if provider_of(agent).is_some() {
        return Vec::new();
    }
    match effective_kind(agent) {
        Kind::ClaudeCode => [("opus", "Opus"), ("sonnet", "Sonnet"), ("haiku", "Haiku")]
            .map(|(id, name)| ModelChoice { id: id.into(), name: name.into() })
            .to_vec(),
        Kind::Antigravity => match find(Kind::Antigravity) {
            Some(path) => parse_agy_models(&probe(&path, &["models"]).await.unwrap_or_default()),
            None => Vec::new(),
        },
        _ => Vec::new(),
    }
}

/// `agy models` prints a status line, then `id<TAB>name` for every model it serves, including
/// other companies' ones; Gemini's picker keeps only the Gemini models.
pub fn parse_agy_models(out: &str) -> Vec<ModelChoice> {
    out.lines()
        .filter_map(|l| l.split_once('\t'))
        .map(|(id, name)| (id.trim(), name.trim()))
        .filter(|(id, _)| id.starts_with("gemini-"))
        .map(|(id, name)| ModelChoice { id: id.into(), name: if name.is_empty() { id.into() } else { name.into() } })
        .collect()
}
/// Adds the model chosen in the council box (#29) to a turn's arguments: after `exec` for Codex,
/// first for Claude Code, Antigravity and the Gemini CLI. A blank name, or one starting with `-`
/// (which a CLI would read as an option), leaves the plan as it is, as do the API agents.
pub fn with_model(mut plan: Plan, kind: Kind, model: Option<&str>) -> Plan {
    let Some(m) = model.map(str::trim).filter(|m| !m.is_empty() && !m.starts_with('-')) else {
        return plan;
    };
    let at = match kind {
        Kind::Codex => 1,
        Kind::ClaudeCode | Kind::Antigravity | Kind::GeminiCli => 0,
        Kind::XaiApi | Kind::DeepSeekApi | Kind::CustomApi => return plan,
    };
    plan.args.insert(at, "--model".into());
    plan.args.insert(at + 1, m.into());
    plan
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
    run_with_files(agent, access, workspace, system, prompt, &crate::attach::Files::default(), cancel).await
}

/// `run_with_usage`, with the conversation's attached files open to the AI (see `plan_with_files`).
pub async fn run_with_files(
    agent: &Agent,
    access: Access,
    workspace: Option<&Path>,
    system: &str,
    prompt: &str,
    files: &crate::attach::Files,
    cancel: &CancellationToken,
) -> Result<AgentReply, String> {
    run_with_files_observed(agent, access, workspace, system, prompt, files, cancel, None, Vec::new(), None).await
}

/// `run_with_files`, reporting provider-native activity as structured output arrives.
#[allow(clippy::too_many_arguments)]
pub async fn run_with_files_observed(
    agent: &Agent,
    access: Access,
    workspace: Option<&Path>,
    system: &str,
    prompt: &str,
    files: &crate::attach::Files,
    cancel: &CancellationToken,
    activity: Option<ActivitySink>,
    elevated_commands: Vec<String>,
    model: Option<&str>,
) -> Result<AgentReply, String> {
    if let Some(provider) = provider_of(agent) {
        // Attached files aren't sent: the API agents see only the conversation text.
        if let Some(sink) = &activity {
            sink(ActivitySignal {
                kind: "thinking".into(),
                label: format!("Waiting for {}", agent.name),
                detail: "API response".into(),
            });
        }
        let (text, usage, model) =
            crate::apiagent::run(agent.id, agent.name, provider, system, prompt, cancel, RUN_TIMEOUT).await?;
        return Ok(AgentReply { text, usage, model: Some(model) });
    }
    let kind = effective_kind(agent);
    let path = find(kind).ok_or_else(|| format!("{} isn't installed", agent.name))?;
    let first = run_at_with_usage(
        &path,
        kind,
        access,
        workspace,
        system,
        prompt,
        files,
        cancel,
        RUN_TIMEOUT,
        activity.clone(),
        elevated_commands.clone(),
        model,
    )
    .await;
    // Antigravity ends a turn with no reply at all when the one tool it reached for was refused,
    // which leaves the council without its answer. Ask once more, saying so, for a text answer
    // (anything that needs running goes in a command box), instead of failing the turn.
    match first {
        Err(e) if kind == Kind::Antigravity && is_refused_tool(&e) && !cancel.is_cancelled() => {
            let retry = format!("{system}\n\n{}", refused_tool_note(&e));
            run_at_with_usage(&path, kind, access, workspace, &retry, prompt, files, cancel, RUN_TIMEOUT, activity, elevated_commands, model).await
        }
        other => other,
    }
}

fn is_refused_tool(error: &str) -> bool {
    error.contains("its access doesn't allow")
}

fn refused_tool_note(error: &str) -> String {
    let tools = error.split_once('(').and_then(|(_, r)| r.split_once(')')).map_or("a tool", |(t, _)| t);
    format!(
        "NOTE FROM CONSILIUM: your last try at this turn used {tools}, which this access level doesn't allow, so it \
         stopped with no reply. Answer in text this time. Read what you need from the transcript; anything that needs \
         running (reading a file, a build, a test) goes in a command box, which Consilium runs and reports back."
    )
}

/// Starts an Antigravity process for `workspace` at `access` so the first Gemini answer after the
/// app opens skips the startup too (see `take_spare`).
pub async fn prewarm_antigravity(agent: &Agent, access: Access, workspace: &Path, model: Option<&str>) {
    if effective_kind(agent) != Kind::Antigravity || access == Access::Chat || !workspace.is_dir() {
        return;
    }
    let Some(path) = find(Kind::Antigravity) else { return };
    let files = crate::attach::Files { dir: None, images: Vec::new() };
    let plan = with_model(plan_with_files(Kind::Antigravity, access, workspace, &workspace.join("reply.txt"), "", "", &files), Kind::Antigravity, model);
    prepare_spare(&path, workspace, &plan.args);
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
        path,
        kind,
        access,
        workspace,
        system,
        prompt,
        &crate::attach::Files::default(),
        cancel,
        timeout,
        None,
        Vec::new(),
        None,
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
    files: &crate::attach::Files,
    cancel: &CancellationToken,
    timeout: Duration,
    activity: Option<ActivitySink>,
    elevated_commands: Vec<String>,
    model: Option<&str>,
) -> Result<AgentReply, String> {
    let work = std::env::temp_dir().join(format!("consilium-{}", uuid::Uuid::new_v4()));
    let empty = work.join("empty");
    std::fs::create_dir_all(&empty).map_err(|e| format!("couldn't create a work folder: {e}"))?;
    let (dir, access) = match workspace {
        Some(ws) if access != Access::Chat && ws.is_dir() => (ws.to_path_buf(), access),
        _ => (empty.clone(), Access::Chat),
    };
    let reply_file = work.join("reply.txt");
    let plan = with_model(plan_with_files(kind, access, &dir, &reply_file, system, prompt, files), kind, model);
    // Antigravity in a real project folder: the next process starts as soon as this one is done
    // (see `take_spare`). Not for Chat, whose folder is this run's own temporary one.
    let warm_args = (kind == Kind::Antigravity && access != Access::Chat).then(|| plan.args.clone());
    let result = execute(
        path,
        kind,
        access,
        &dir,
        &reply_file,
        plan,
        cancel,
        timeout,
        activity,
        elevated_commands,
    )
    .await;
    let _ = std::fs::remove_dir_all(&work);
    // Only after a run that worked: a sign-in or install problem shouldn't keep spawning spares.
    if let (Some(args), Ok(_)) = (&warm_args, &result) {
        prepare_spare(path, &dir, args);
    }
    result
}

/// Antigravity spends about 3-4 s starting up (signing in, loading tools) before it reads the
/// prompt from stdin, and print mode runs one prompt per process. So after each answer the next
/// process is started straight away with the same arguments and folder, and waits, ready: the
/// next prompt skips the startup. A spare older than `SPARE_MAX_AGE`, or one that has exited, is
/// dropped (killed) and a fresh process started as before. At most `MAX_SPARES` (one per folder
/// and settings combination in use), so idle processes never pile up.
const SPARE_MAX_AGE: Duration = Duration::from_secs(20 * 60);
const MAX_SPARES: usize = 3;

struct Spare {
    key: String,
    child: tokio::process::Child,
    born: std::time::Instant,
}

static SPARES: std::sync::Mutex<Vec<Spare>> = std::sync::Mutex::new(Vec::new());

fn spare_key(path: &Path, dir: &Path, args: &[std::ffi::OsString]) -> String {
    let mut k = format!("{}\u{0}{}", path.display(), dir.display());
    for a in args {
        k.push('\u{0}');
        k.push_str(&a.to_string_lossy());
    }
    k
}

/// A ready Antigravity process started with exactly these arguments in `dir`, if there is one.
fn take_spare(path: &Path, dir: &Path, args: &[std::ffi::OsString]) -> Option<tokio::process::Child> {
    let key = spare_key(path, dir, args);
    let mut spares = SPARES.lock().unwrap();
    let i = spares.iter().position(|s| s.key == key)?;
    let mut s = spares.remove(i);
    drop(spares);
    let alive = matches!(s.child.try_wait(), Ok(None));
    (alive && s.born.elapsed() < SPARE_MAX_AGE).then_some(s.child)
}

/// Starts the next Antigravity process for these arguments, to wait for the next prompt.
fn prepare_spare(path: &Path, dir: &Path, args: &[std::ffi::OsString]) {
    if cfg!(test) || std::env::var_os("CONSILIUM_NO_WARM_AGY").is_some() {
        return;
    }
    let key = spare_key(path, dir, args);
    let mut cmd = command(path);
    cmd.current_dir(dir).stdin(Stdio::piped()).args(args);
    let Ok(child) = cmd.spawn() else { return };
    let mut spares = SPARES.lock().unwrap();
    spares.retain(|s| s.key != key);
    if spares.len() >= MAX_SPARES {
        spares.remove(0); // oldest; dropping it kills it
    }
    spares.push(Spare { key, child, born: std::time::Instant::now() });
}

/// Ends every waiting Antigravity process (quitting, or the person changed Gemini's settings).
pub fn drop_spares() {
    SPARES.lock().unwrap().clear();
}

#[allow(clippy::too_many_arguments)]
async fn execute(
    path: &Path,
    kind: Kind,
    access: Access,
    dir: &Path,
    reply_file: &Path,
    plan: Plan,
    cancel: &CancellationToken,
    timeout: Duration,
    activity: Option<ActivitySink>,
    mut elevated_commands: Vec<String>,
) -> Result<AgentReply, String> {
    let spare = if kind == Kind::Antigravity { take_spare(path, dir, &plan.args) } else { None };
    let mut child = match spare {
        Some(c) => c,
        None => {
            let mut cmd = command(path);
            cmd.current_dir(dir).stdin(Stdio::piped()).args(&plan.args);
            cmd.spawn().map_err(|e| format!("couldn't start {}: {e}", path.display()))?
        }
    };
    let pid = child.id();
    let _registered = crate::reaper::Registered::new(pid);
    let mut stdin = child.stdin.take().expect("stdin is piped");
    let stdout = child.stdout.take().expect("stdout is piped");
    let stderr = child.stderr.take().expect("stderr is piped");
    let input = plan.input;
    let writer = tokio::spawn(async move {
        let _ = stdin.write_all(input.as_bytes()).await;
        let _ = stdin.shutdown().await;
    });
    // Both readers report chunks to the watchdog as soon as they arrive. Structured provider
    // output remains on ordinary pipes (a PTY would corrupt JSON framing), while a surprise
    // interactive prompt is caught before it can leave a turn apparently frozen for 15 minutes.
    let (chunk_tx, mut chunk_rx) = mpsc::unbounded_channel();
    let (out_buf, out_task) = drain(stdout, chunk_tx.clone());
    let (err_buf, err_task) = drain(stderr, chunk_tx.clone());
    drop(chunk_tx);
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);
    let mut recent_output = String::new();
    let mut activity_output = String::new();
    let mut watch_output = true;

    let mut check_stream = |line: &str| -> Result<(), String> {
        // Read access only reads files; plan mode isn't documented to block shell tools, so stop here.
        if access == Access::Read && kind == Kind::Antigravity {
            if let Some(command) = streamed_command(kind, line) {
                return Err(format!(
                    "{} can only read files at this access level, so Consilium stopped its command. Post it as a command box for the human instead: {}",
                    tool_name(kind),
                    crate::redact::scrub(&command)
                ));
            }
        }
        if matches!(access, Access::Own | Access::Unrestricted) {
            if let Some(command) = streamed_command(kind, line) {
                if elevated_command(&command) {
                    if let Some(index) = elevated_commands
                        .iter()
                        .position(|grant| grant.trim() == command.trim())
                    {
                        // Each grant is consumed before execution and exists only for this turn.
                        elevated_commands.remove(index);
                    } else {
                        let scrubbed = crate::redact::scrub(&command);
                        if scrubbed != command {
                            return Err(
                                "administrator approval refused: the command contains a credential-like value. Remove the secret and ask again; never place credentials in commands or REQUESTs."
                                    .into(),
                            );
                        }
                        let encoded =
                            serde_json::to_string(command.trim()).unwrap_or_else(|_| "\"\"".into());
                        return Err(format!(
                            "administrator approval required for {}.\nREQUEST: Approve one elevated command on {} for the stated task, using the normal visible OS authentication prompt? ELEVATED_COMMAND_JSON: {}",
                            tool_name(kind),
                            std::env::consts::OS,
                            encoded,
                        ));
                    }
                }
                if !elevated_command(&command) && crate::toolbox::install_command(&command) {
                    let wanted = format!("{}{}", crate::prompt::INSTALL_GRANT_PREFIX, command.trim());
                    if let Some(index) = elevated_commands.iter().position(|grant| grant.trim() == wanted) {
                        // Consumed before execution, like an elevation grant.
                        elevated_commands.remove(index);
                    } else {
                        if crate::redact::scrub(&command) != command {
                            return Err(
                                "install approval refused: the command contains a credential-like value. Remove the secret and ask again."
                                    .into(),
                            );
                        }
                        let encoded =
                            serde_json::to_string(command.trim()).unwrap_or_else(|_| "\"\"".into());
                        return Err(format!(
                            "install approval required: {} tried to install software onto this computer.\nREQUEST: Install this for the stated task? INSTALL_COMMAND_JSON: {}",
                            tool_name(kind),
                            encoded,
                        ));
                    }
                }
                if destructive_command(&command) {
                    return Err(format!(
                        "safety stop: blocked a high-risk command from {}: {}\nREQUEST: Review the command and run it yourself only if its destructive effect is intended.",
                        tool_name(kind),
                        safe_activity_detail(&command),
                    ));
                }
            }
        }
        if let Some(signal) = activity_from_line(kind, line) {
            if let Some(sink) = &activity {
                sink(signal);
            }
        }
        Ok(())
    };

    let mut process_chunk = |chunk: &[u8]| -> Result<(), String> {
        recent_output.push_str(&String::from_utf8_lossy(chunk));
        keep_tail(&mut recent_output, 8_192);
        if activity.is_some() || matches!(access, Access::Own | Access::Unrestricted) {
            activity_output.push_str(&String::from_utf8_lossy(chunk));
            while let Some(end) = activity_output.find('\n') {
                let line = activity_output[..end].trim_end_matches('\r').to_string();
                activity_output.drain(..=end);
                check_stream(&line)?;
            }
            keep_tail(&mut activity_output, 32_768);
        }
        if let Some(line) = interactive_prompt(&recent_output) {
            return Err(format!(
                "{} is waiting for interactive input: {line}\nREQUEST: Open {} in a terminal, complete the prompt, then press Retry in Consilium.",
                tool_name(kind),
                path.display(),
            ));
        }
        Ok(())
    };

    let outcome = 'watch: loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => break Err("stopped".to_string()),
            _ = &mut deadline => break Err(format!("no answer within {} seconds", timeout.as_secs())),
            chunk = chunk_rx.recv(), if watch_output => {
                let Some(chunk) = chunk else {
                    watch_output = false;
                    continue;
                };
                if let Err(why) = process_chunk(&chunk) {
                    break 'watch Err(why);
                }
            }
            status = child.wait() => break Ok(status),
        }
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
    while let Ok(chunk) = chunk_rx.try_recv() {
        if let Err(why) = process_chunk(&chunk) {
            kill_tree(pid, &mut child).await;
            return Err(why);
        }
    }
    if !activity_output.trim().is_empty() {
        let line = activity_output.trim().to_string();
        activity_output.clear();
        if let Err(why) = check_stream(&line) {
            kill_tree(pid, &mut child).await;
            return Err(why);
        }
    }
    let stdout = String::from_utf8_lossy(&out_buf.lock().unwrap()).to_string();
    let stderr = String::from_utf8_lossy(&err_buf.lock().unwrap()).to_string();

    let usage = token_usage_from_output(kind, &stdout);
    let model = reply_model(kind, &stdout);
    let reply = match kind {
        Kind::Codex => std::fs::read_to_string(reply_file).unwrap_or_default(),
        Kind::ClaudeCode => match claude_result(&stdout) {
            Some(v) => {
                let text = v["result"].as_str().unwrap_or_default().to_string();
                if v["is_error"].as_bool() == Some(true) {
                    return Err(tail(if text.is_empty() { &stderr } else { &text }));
                }
                text
            }
            None => String::new(),
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
        Kind::XaiApi | Kind::DeepSeekApi | Kind::CustomApi => String::new(),
    };
    if matches!(kind, Kind::ClaudeCode) && claude_result(&stdout).is_none() {
        // The stream ended without a result line. Pass on the provider's own error if it sent one,
        // word for word, so a usage limit is still recognised; otherwise say so in a line, never
        // the raw stream (its first line alone lists every tool and runs to kilobytes).
        if let Some(e) = claude_stream_error(&stdout) {
            return Err(e);
        }
        let code = status.code().map_or_else(|| "killed".to_string(), |c| format!("exit {c}"));
        let err = stderr.trim();
        let why = if err.is_empty() { String::new() } else { format!(": {}", last_chars(err, ERROR_TAIL_CHARS)) };
        return Err(format!("Claude CLI ended before reporting a final result ({code}){why}"));
    }
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
    let v = claude_result(stdout)?;
    let models = v["modelUsage"].as_object()?;
    models
        .iter()
        .max_by_key(|(_, u)| u["outputTokens"].as_u64().unwrap_or(0))
        .map(|(id, _)| id.clone())
}

/// Claude Code returns one JSON object in legacy JSON mode and a line-delimited `result` object in
/// stream mode. Accept both so older installations and recorded test fixtures remain readable.
fn claude_result(stdout: &str) -> Option<serde_json::Value> {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(stdout.trim()) {
        if v["type"] == "result" || v.get("result").is_some() {
            return Some(v);
        }
    }
    stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .rfind(|v| v["type"] == "result")
}

/// Most of a stderr tail quoted in an error, in characters.
const ERROR_TAIL_CHARS: usize = 400;

/// The last `n` characters of `s` (never splitting a character).
fn last_chars(s: &str, n: usize) -> String {
    let skip = s.chars().count().saturating_sub(n);
    s.chars().skip(skip).collect()
}

/// The newest error Claude Code's stream reported (an `error` line, or an assistant message
/// flagged with `error`, as a usage limit is), as its text.
fn claude_stream_error(stdout: &str) -> Option<String> {
    stdout.lines().filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok()).rev().find_map(|v| {
        let err = &v["error"];
        if v["type"] != "error" && (err.is_null() || err == false) {
            return None;
        }
        let text: String = v["message"]["content"]
            .as_array()
            .map(|parts| parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join(" "))
            .unwrap_or_default();
        let msg = [err["message"].as_str(), v["message"].as_str(), Some(text.as_str()), err.as_str()]
            .into_iter()
            .flatten()
            .map(str::trim)
            .find(|m| !m.is_empty())?
            .to_string();
        // Keep the error kind beside the text (e.g. "rate_limit") so the usage check sees it too.
        let kind = err.as_str().or(err["type"].as_str()).filter(|k| !msg.contains(k));
        Some(last_chars(&kind.map_or(msg.clone(), |k| format!("{k}: {msg}")), ERROR_TAIL_CHARS))
    })
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
            let v = claude_result(stdout)?;
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
        Kind::GeminiCli | Kind::XaiApi | Kind::DeepSeekApi | Kind::CustomApi => None,
    }
}

/// How long to keep reading output after the CLI exits (see `execute`).
const PIPE_GRACE: Duration = Duration::from_secs(3);

/// Reads a pipe into a shared buffer in the background, so what has arrived can be used even if
/// the pipe never closes.
fn drain<R: tokio::io::AsyncRead + Unpin + Send + 'static>(
    mut r: R,
    chunks: mpsc::UnboundedSender<Vec<u8>>,
) -> (
    std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    tokio::task::JoinHandle<()>,
) {
    let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let b = buf.clone();
    let task = tokio::spawn(async move {
        let mut chunk = [0u8; 8192];
        loop {
            match r.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    b.lock().unwrap().extend_from_slice(&chunk[..n]);
                    let _ = chunks.send(chunk[..n].to_vec());
                }
            }
        }
    });
    (buf, task)
}

fn tool_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Codex => "Codex CLI",
        Kind::ClaudeCode => "Claude Code CLI",
        Kind::GeminiCli => "Gemini CLI",
        Kind::Antigravity => "Antigravity CLI",
        Kind::XaiApi => "xAI API",
        Kind::DeepSeekApi => "DeepSeek API",
        Kind::CustomApi => "API",
    }
}

/// Turns one complete structured-output line into a compact, safe activity description. Provider
/// schemas evolve, so unknown events are ignored rather than guessed at. Reasoning text and model
/// prose are never copied into activity details.
/// The access an AI actually runs with. Gemini gets read-only file tools at every level once the
/// conversation has a project folder: at Chat it reads that folder in plan mode (no shell, no edits),
/// so its `read_file` finds the project instead of an empty folder.
pub fn effective_access(agent_id: &str, access: Access, has_workspace: bool) -> Access {
    if agent_id == "gemini" && has_workspace && access == Access::Chat {
        Access::Read
    } else {
        access
    }
}

fn activity_from_line(kind: Kind, line: &str) -> Option<ActivitySignal> {
    let v: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    match kind {
        Kind::Codex => codex_activity(&v),
        Kind::ClaudeCode => claude_activity(&v),
        Kind::Antigravity => antigravity_activity(&v),
        Kind::GeminiCli | Kind::XaiApi | Kind::DeepSeekApi | Kind::CustomApi => None,
    }
}

/// Extracts a command at the provider's pre-execution activity boundary. This is intentionally
/// separate from the display telemetry: the guard examines the complete command before redaction
/// and truncation, while the UI continues to receive only a short scrubbed description.
fn streamed_command(kind: Kind, line: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    match kind {
        Kind::Codex if v["type"] == "item.started" && v["item"]["type"] == "command_execution" => {
            v["item"]["command"].as_str().map(str::to_string)
        }
        Kind::ClaudeCode if v["type"] == "assistant" => v["message"]["content"]
            .as_array()?
            .iter()
            .find(|block| {
                block["type"] == "tool_use"
                    && block["name"].as_str().is_some_and(|n| {
                        matches!(
                            n.to_ascii_lowercase().as_str(),
                            "bash" | "powershell" | "pwsh" | "cmd" | "terminal" | "run_command" | "exec"
                        )
                    })
            })
            .and_then(|block| {
                block["input"]["command"]
                    .as_str()
                    .or_else(|| block["input"]["cmd"].as_str())
            })
            .map(str::to_string),
        Kind::Antigravity if v["event"] == "step_update" => {
            let step = &v["step_update"];
            let tool = step["tool_name"]
                .as_str()
                .or_else(|| step["display_name"].as_str())
                .or_else(|| step["tool"]["name"].as_str())?
                .to_ascii_lowercase();
            (tool.contains("command") || tool.contains("terminal"))
                .then(|| step["command"].as_str().map(str::to_string))
                .flatten()
        }
        _ => None,
    }
}

/// Whether a command crosses the OS administrator/root boundary. Full capacity deliberately does
/// not imply elevation: a matching, one-turn grant from an exact REQUEST is required separately.
pub(crate) fn elevated_command(command: &str) -> bool {
    shell_segments(command).into_iter().any(elevated_segment)
}

fn elevated_segment(segment: String) -> bool {
    let words: Vec<String> = segment
        .split_whitespace()
        .map(|word| {
            word.trim_matches(|ch: char| matches!(ch, '\'' | '"' | ',' | '(' | ')'))
                .to_ascii_lowercase()
        })
        .collect();
    if words.is_empty() {
        return false;
    }
    let program = words[0].rsplit(['/', '\\']).next().unwrap_or(&words[0]);
    let args = &words[1..];
    if matches!(program, "sudo" | "sudoedit" | "doas" | "pkexec" | "runas" | "runas.exe" | "gsudo" | "gsudo.exe") {
        return true;
    }
    let run_as_verb = words.windows(2).any(|pair| pair[0] == "-verb" && pair[1] == "runas");
    if program == "start-process" && run_as_verb {
        return true;
    }
    if matches!(program, "powershell" | "powershell.exe" | "pwsh" | "pwsh.exe") {
        let joined = args.join(" ");
        if joined.contains("start-process") && run_as_verb {
            return true;
        }
    }
    if matches!(program, "osascript" | "osascript.exe") {
        return args.join(" ").contains("with administrator privileges");
    }
    if matches!(program, "sh" | "bash" | "zsh" | "cmd" | "cmd.exe") {
        return args
            .iter()
            .position(|arg| matches!(arg.as_str(), "-c" | "/c"))
            .is_some_and(|index| elevated_command(&args[index + 1..].join(" ")));
    }
    false
}

/// Emergency brake for commands whose ordinary meaning is broad, difficult-to-recover data loss
/// or an OS/disk shutdown. It is deliberately narrow: provider-native permission modes remain the
/// primary guard, and ambiguous commands are not guessed at. The stream event normally arrives
/// before execution, but a provider can change that ordering; this is defense in depth, not an OS
/// sandbox.
fn destructive_command(command: &str) -> bool {
    shell_segments(command).into_iter().any(destructive_segment)
}

fn shell_segments(command: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut chars = command.chars().peekable();
    while let Some(ch) = chars.next() {
        if let Some(q) = quote {
            current.push(ch);
            if ch == q {
                quote = None;
            } else if ch == '\\' && q == '"' {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            continue;
        }
        match ch {
            '\'' | '"' => {
                quote = Some(ch);
                current.push(ch);
            }
            ';' | '\n' => {
                if !current.trim().is_empty() {
                    out.push(current.trim().to_string());
                }
                current.clear();
            }
            '&' | '|' if chars.peek() == Some(&ch) => {
                chars.next();
                if !current.trim().is_empty() {
                    out.push(current.trim().to_string());
                }
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    if !current.trim().is_empty() {
        out.push(current.trim().to_string());
    }
    out
}

fn destructive_segment(segment: String) -> bool {
    let words: Vec<String> = segment
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| matches!(c, '\'' | '"' | ',' | '(' | ')')).to_ascii_lowercase())
        .collect();
    if words.is_empty() {
        return false;
    }
    let mut start = 0;
    while start < words.len() && (words[start] == "sudo" || words[start] == "command" || words[start] == "env") {
        start += 1;
        while start < words.len() && words[start].starts_with('-') {
            start += 1;
        }
    }
    let Some(program) = words.get(start).map(|s| s.rsplit(['/', '\\']).next().unwrap_or(s)) else { return false };
    let args = &words[start + 1..];
    match program {
        "rm" => {
            let recursive = args.iter().any(|a| a == "--recursive" || (a.starts_with('-') && a.contains('r')));
            let force = args.iter().any(|a| a == "--force" || (a.starts_with('-') && a.contains('f')));
            recursive && force && args.iter().any(|a| dangerous_target(a))
        }
        "git" => {
            (args.first().is_some_and(|a| a == "reset") && args.iter().any(|a| a == "--hard"))
                || (args.first().is_some_and(|a| a == "clean")
                    && args.iter().any(|a| a.starts_with('-') && a.contains('f')))
        }
        "diskutil" => args.first().is_some_and(|a| a.starts_with("erase")),
        "mkfs" | "fdisk" | "format" | "shutdown" | "reboot" | "halt" | "poweroff" => true,
        "dd" => args.iter().any(|a| a.starts_with("of=/dev/")),
        "find" => args.iter().any(|a| a == "-delete") && args.first().is_some_and(|a| dangerous_target(a)),
        "remove-item" => {
            args.iter().any(|a| a == "-recurse")
                && args.iter().any(|a| a == "-force")
                && args.iter().any(|a| dangerous_target(a))
        }
        "sh" | "bash" | "zsh" | "powershell" | "pwsh" | "cmd" => args
            .iter()
            .position(|a| matches!(a.as_str(), "-c" | "-command" | "/c"))
            .is_some_and(|i| destructive_command(&args[i + 1..].join(" "))),
        "rmdir" | "rd" => {
            args.iter().any(|a| a == "/s")
                && args.iter().any(|a| a == "/q")
                && args.iter().any(|a| dangerous_target(a))
        }
        _ => false,
    }
}

fn dangerous_target(target: &str) -> bool {
    matches!(
        target.trim_end_matches(['/', '\\']),
        "" | "." | ".." | "*" | "/*" | "/.*" | "./*" | "../*" | "~" | "$home" | "${home}" | "$env:userprofile" | "%userprofile%" | "c:"
    )
}

fn signal(kind: &str, label: &str, detail: impl AsRef<str>) -> ActivitySignal {
    ActivitySignal {
        kind: kind.into(),
        label: label.into(),
        detail: safe_activity_detail(detail.as_ref()),
    }
}

fn safe_activity_detail(detail: &str) -> String {
    let scrubbed = crate::redact::scrub(detail);
    let one_line = scrubbed.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out: String = one_line.chars().take(140).collect();
    if one_line.chars().count() > 140 {
        out.push('…');
    }
    out
}

fn codex_activity(v: &serde_json::Value) -> Option<ActivitySignal> {
    match v["type"].as_str()? {
        "thread.started" => Some(signal("preparing", "Starting Codex", "")),
        "turn.started" => Some(signal("thinking", "Planning", "")),
        "turn.completed" => Some(signal("reporting", "Finishing report", "")),
        "item.started" | "item.completed" => {
            let item = &v["item"];
            match item["type"].as_str()? {
                // Never surface the reasoning text itself.
                "reasoning" => Some(signal("thinking", "Reasoning", "")),
                "command_execution" => Some(signal(
                    "running",
                    "Running command",
                    item["command"].as_str().unwrap_or_default(),
                )),
                "file_change" => Some(signal(
                    "coding",
                    "Editing files",
                    item["path"]
                        .as_str()
                        .or_else(|| item["file_path"].as_str())
                        .unwrap_or_default(),
                )),
                "mcp_tool_call" | "dynamic_tool_call" => {
                    let server = item["server"].as_str().unwrap_or_default();
                    let tool = item["tool"]
                        .as_str()
                        .or_else(|| item["name"].as_str())
                        .unwrap_or_default();
                    Some(signal("tool", "Using tool", format!("{server} {tool}")))
                }
                "web_search" => Some(signal(
                    "researching",
                    "Searching the web",
                    item["query"].as_str().unwrap_or_default(),
                )),
                "todo_list" | "plan" => Some(signal("planning", "Updating plan", "")),
                "agent_message" => Some(signal("reporting", "Writing report", "")),
                _ => None,
            }
        }
        _ => None,
    }
}

fn claude_activity(v: &serde_json::Value) -> Option<ActivitySignal> {
    match v["type"].as_str()? {
        "system" if v["subtype"] == "init" => Some(signal("preparing", "Starting Claude Code", "")),
        "result" => Some(signal("reporting", "Finishing report", "")),
        "assistant" => {
            let content = v["message"]["content"].as_array()?;
            for block in content.iter().rev() {
                match block["type"].as_str() {
                    Some("tool_use") => return Some(claude_tool_activity(block)),
                    Some("text") => return Some(signal("reporting", "Writing report", "")),
                    Some("thinking") => return Some(signal("thinking", "Thinking", "")),
                    _ => {}
                }
            }
            None
        }
        "user" => Some(signal("tool", "Checking tool result", "")),
        _ => None,
    }
}

fn claude_tool_activity(block: &serde_json::Value) -> ActivitySignal {
    let name = block["name"].as_str().unwrap_or("tool");
    let input = &block["input"];
    let detail = input["file_path"]
        .as_str()
        .or_else(|| input["path"].as_str())
        .or_else(|| input["query"].as_str())
        .or_else(|| input["pattern"].as_str())
        .or_else(|| input["description"].as_str())
        .or_else(|| input["command"].as_str())
        .unwrap_or(name);
    match name.to_ascii_lowercase().as_str() {
        "read" | "glob" => signal("reading", &format!("Using {name}"), detail),
        "grep" | "websearch" | "webfetch" => signal("researching", &format!("Using {name}"), detail),
        "edit" | "write" | "multiedit" | "notebookedit" => signal("coding", &format!("Using {name}"), detail),
        "bash" | "powershell" | "pwsh" | "cmd" | "terminal" | "run_command" | "exec" => {
            signal("running", "Running command", detail)
        }
        "task" | "agent" => signal("delegating", "Delegating task", detail),
        _ => signal("tool", &format!("Using {name}"), detail),
    }
}

fn antigravity_activity(v: &serde_json::Value) -> Option<ActivitySignal> {
    match v["event"].as_str()? {
        "init" => Some(signal("preparing", "Starting Antigravity", "")),
        "result" => Some(signal("reporting", "Finishing report", "")),
        "step_update" => {
            let step = &v["step_update"];
            let step_type = step["step_type"].as_str().unwrap_or_default();
            let tool = step["tool_name"]
                .as_str()
                .or_else(|| step["display_name"].as_str())
                .or_else(|| step["tool"]["name"].as_str());
            if let Some(tool) = tool {
                let detail = step["description"]
                    .as_str()
                    .or_else(|| step["command"].as_str())
                    .or_else(|| step["query"].as_str())
                    .unwrap_or_default();
                let lower = tool.to_ascii_lowercase();
                return Some(if lower.contains("search") || lower.contains("browser") || lower.contains("read") {
                    signal("researching", &format!("Using {tool}"), detail)
                } else if lower.contains("write") || lower.contains("replace") || lower.contains("edit") {
                    signal("coding", &format!("Using {tool}"), detail)
                } else if lower.contains("command") || lower.contains("terminal") || lower.contains("powershell") || lower.contains("bash") {
                    signal("running", &format!("Using {tool}"), detail)
                } else {
                    signal("tool", &format!("Using {tool}"), detail)
                });
            }
            match step_type {
                "agent_response" => Some(signal("reporting", "Writing report", "")),
                "thinking" | "reasoning" => Some(signal("thinking", "Thinking", "")),
                "tool_use" | "tool_call" => Some(signal("tool", "Using tool", "")),
                "user_input" => None,
                other if !other.is_empty() => Some(signal("working", "Working", other.replace('_', " "))),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Bounds the prompt watchdog's memory without cutting in the middle of a UTF-8 code point.
fn keep_tail(text: &mut String, max_bytes: usize) {
    if text.len() <= max_bytes {
        return;
    }
    let mut start = text.len() - max_bytes;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    text.drain(..start);
}

/// Recognises prompts that a supposedly non-interactive provider unexpectedly surfaced. We do
/// not auto-answer credential, trust, or destructive confirmations: the human gets a REQUEST
/// instead. Restricting matches to the last visible lines avoids treating quoted prompt examples
/// inside a model's JSON reply as a live prompt.
fn interactive_prompt(output: &str) -> Option<String> {
    output
        .replace('\r', "\n")
        .lines()
        .rev()
        .take(4)
        .map(str::trim)
        .find_map(|line| {
            if line.is_empty() || line.len() > 500 || line.starts_with('{') || line.starts_with('[')
            {
                return None;
            }
            let lower = line.to_ascii_lowercase();
            let yes_no = ["(y/n)", "[y/n]", "(yes/no)", "[yes/no]"]
                .iter()
                .any(|marker| lower.contains(marker));
            let trust = lower.contains("trust this directory")
                || lower.contains("trust this folder")
                || lower.contains("do you trust");
            let credential = lower.ends_with("password:")
                || lower.ends_with("passphrase:")
                || lower.ends_with("verification code:")
                || lower.ends_with("one-time code:");
            let continue_prompt = lower.contains("press enter to continue")
                || lower.contains("press return to continue");
            let confirmation =
                yes_no && (line.contains('?') || trust || lower.contains("continue"));
            (trust || credential || continue_prompt || confirmation).then(|| line.to_string())
        })
}

/// The reply from Antigravity's stream-json output: the final `result` event's response, or the
/// streamed text if that is empty. A run that ended without text because a tool it reached for was
/// blocked is reported as such, rather than as an empty answer.
/// Antigravity allow-rules for an access level, or None when no file tools are wanted (Chat),
/// the person's own settings decide (Own), or the bypass switch already covers them.
fn antigravity_allow_rules(access: Access) -> Option<Vec<&'static str>> {
    match access {
        Access::Chat | Access::Own => None,
        Access::Read => Some(vec!["read_file(*)"]),
        Access::Edit => Some(vec!["read_file(*)", "edit_file(*)", "write_file(*)"]),
        Access::Unrestricted => Some(vec!["read_file(*)", "write_file(*)", "edit_file(*)", "run_command(*)"]),
    }
}

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
    fn native_activity_is_classified_without_exposing_reasoning_text() {
        let reasoning = activity_from_line(
            Kind::Codex,
            r#"{"type":"item.started","item":{"type":"reasoning","text":"private chain of thought"}}"#,
        )
        .unwrap();
        assert_eq!(reasoning, signal("thinking", "Reasoning", ""));
        assert!(!reasoning.detail.contains("private"));

        let command_line = serde_json::json!({
            "type": "item.started",
            "item": {"type": "command_execution", "command": format!("npm test API_KEY={}", "a".repeat(24))}
        })
        .to_string();
        let command = activity_from_line(Kind::Codex, &command_line).unwrap();
        assert_eq!(command.kind, "running");
        assert!(command.detail.contains("[REDACTED"), "{}", command.detail);

        let claude = activity_from_line(
            Kind::ClaudeCode,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Edit","input":{"file_path":"src/App.tsx"}}]}}"#,
        )
        .unwrap();
        assert_eq!((claude.kind.as_str(), claude.label.as_str(), claude.detail.as_str()), ("coding", "Using Edit", "src/App.tsx"));

        let gemini = activity_from_line(
            Kind::Antigravity,
            r#"{"event":"step_update","step_update":{"step_type":"tool_call","tool_name":"search_web","query":"Tauri events"}}"#,
        )
        .unwrap();
        assert_eq!((gemini.kind.as_str(), gemini.label.as_str()), ("researching", "Using search_web"));
    }

    #[test]
    fn unrestricted_stream_guard_extracts_provider_commands() {
        assert_eq!(
            streamed_command(
                Kind::Codex,
                r#"{"type":"item.started","item":{"type":"command_execution","command":"git reset --hard"}}"#,
            )
            .as_deref(),
            Some("git reset --hard")
        );
        assert_eq!(
            streamed_command(
                Kind::ClaudeCode,
                r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"rm -rf /"}}]}}"#,
            )
            .as_deref(),
            Some("rm -rf /")
        );
        assert_eq!(
            streamed_command(
                Kind::ClaudeCode,
                r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"PowerShell","input":{"command":"Stop-Computer"}}]}}"#,
            )
            .as_deref(),
            Some("Stop-Computer")
        );
        assert_eq!(
            streamed_command(
                Kind::Antigravity,
                r#"{"event":"step_update","step_update":{"tool_name":"run_terminal_command","command":"shutdown -h now"}}"#,
            )
            .as_deref(),
            Some("shutdown -h now")
        );
    }

    #[test]
    fn destructive_guard_is_narrow_and_cross_platform() {
        for command in [
            "rm -rf /",
            "sudo rm -fr ~",
            "git reset --hard HEAD",
            "git clean -fdx",
            "diskutil eraseDisk APFS Empty /dev/disk4",
            "dd if=image.iso of=/dev/sda",
            "Remove-Item -Recurse -Force $env:USERPROFILE",
            r#"rd /s /q C:\"#,
            "npm test && shutdown -h now",
            "bash -c 'rm -rf /'",
            "powershell -Command Remove-Item -Recurse -Force $env:USERPROFILE",
        ] {
            assert!(destructive_command(command), "should block: {command}");
        }
        for command in [
            "rm -rf /tmp/consilium-build-123",
            "git status --short",
            "git restore --staged src/App.tsx",
            "rg 'git reset --hard' docs",
            "printf 'rm -rf /'",
            "cargo test",
            "Remove-Item -Recurse ./dist",
        ] {
            assert!(!destructive_command(command), "should allow: {command}");
        }
    }

    #[test]
    fn elevation_guard_recognises_native_admin_paths_without_blocking_plain_shells() {
        for command in [
            "sudo apt-get install widget",
            "doas pkg_add widget",
            "pkexec systemctl restart widget",
            "runas.exe /user:Administrator cmd.exe",
            "Start-Process PowerShell -Verb RunAs",
            "powershell.exe -Command Start-Process cmd.exe -Verb RunAs",
            "osascript -e 'do shell script \"installer\" with administrator privileges'",
            "cmd.exe /c gsudo.exe winget install Widget",
        ] {
            assert!(elevated_command(command), "should require approval: {command}");
        }
        for command in [
            "powershell.exe -NoProfile -Command Get-ChildItem",
            "cmd.exe /d /s /c npm test",
            "bash -lc 'cargo test'",
            "winget search Widget",
            "systemctl --user restart widget",
        ] {
            assert!(!elevated_command(command), "ordinary shell use should be allowed: {command}");
        }
    }

    #[test]
    fn claude_stream_result_remains_the_reply_and_usage_source() {
        let out = concat!(
            "{\"type\":\"system\",\"subtype\":\"init\"}\n",
            "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"hello\"}]}}\n",
            "{\"type\":\"result\",\"is_error\":false,\"result\":\"hello\",\"usage\":{\"input_tokens\":10,\"output_tokens\":5},\"modelUsage\":{\"claude-opus-5-5\":{\"outputTokens\":5}}}\n"
        );
        assert_eq!(claude_result(out).unwrap()["result"], "hello");
        assert_eq!(token_usage_from_output(Kind::ClaudeCode, out).unwrap().total_tokens, 15);
        assert_eq!(reply_model(Kind::ClaudeCode, out).as_deref(), Some("claude-opus-5-5"));
    }

    #[test]
    fn chat_access_turns_tools_off_everywhere() {
        assert!(args(Kind::Codex, Access::Chat).contains(&"--json".to_string()));
        assert!(has(&args(Kind::Codex, Access::Chat), &["-s", "read-only"]));
        assert!(has(&args(Kind::ClaudeCode, Access::Chat), &["--tools", ""]));
        assert!(has(&args(Kind::ClaudeCode, Access::Chat), &["--output-format", "stream-json"]));
        assert!(args(Kind::ClaudeCode, Access::Chat).contains(&"--verbose".to_string()));
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
    fn a_turn_lost_to_a_refused_tool_is_asked_again_with_the_reason() {
        let e = "Gemini tried to use a tool its access doesn't allow (Bash, ReadFile) and wrote no reply. Ask again, or raise its access in Settings.";
        assert!(is_refused_tool(e) && !is_refused_tool("stopped"));
        let note = refused_tool_note(e);
        assert!(note.contains("used Bash, ReadFile") && note.contains("command box"), "{note}");
        let k = |d: &str| spare_key(Path::new("agy"), Path::new(d), &["--mode".into(), "plan".into()]);
        assert_eq!(k("/a"), k("/a"));
        assert_ne!(k("/a"), k("/b"), "a spare is only for the same folder and arguments");
    }

    #[test]
    fn unrestricted_access_uses_each_tools_own_bypass_switch() {
        assert!(args(Kind::Codex, Access::Unrestricted).contains(&"--dangerously-bypass-approvals-and-sandbox".to_string()));
        assert!(args(Kind::ClaudeCode, Access::Unrestricted).contains(&"--dangerously-skip-permissions".to_string()));
        // Antigravity's bypass switch denies shell and outside reads in print mode; explicit
        // allow rules run them instead, on every system.
        let agy = args(Kind::Antigravity, Access::Unrestricted);
        assert!(!agy.contains(&"--dangerously-skip-permissions".to_string()));
        assert!(agy.iter().any(|a| a.contains("run_command(*)") && a.contains("read_file(*)")));
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
    fn gemini_reads_the_project_folder_at_chat_but_others_stay_in_chat() {
        assert!(effective_access("gemini", Access::Chat, true) == Access::Read);
        assert!(effective_access("gemini", Access::Chat, false) == Access::Chat);
        assert!(effective_access("claude", Access::Chat, true) == Access::Chat);
        assert!(effective_access("gemini", Access::Edit, true) == Access::Edit);
        assert!(has(&args(Kind::Antigravity, Access::Read), &["--mode", "plan"]));
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
          let read_rule = &serde_json::json!({ "permissions": { "allow": ["read_file(*)"] } }).to_string();
          let edit_rule = &serde_json::json!({ "permissions": { "allow": ["read_file(*)", "edit_file(*)", "write_file(*)"] } }).to_string();
          assert!(has(&args(Kind::Antigravity, Access::Read), &["--settings", read_rule]));
          assert!(has(&args(Kind::Antigravity, Access::Edit), &["--settings", edit_rule]));
          assert!(!args(Kind::Antigravity, Access::Chat).iter().any(|x| x == "--settings"));
        assert!(!args(Kind::Antigravity, Access::Own).iter().any(|x| x == "--settings"));
        assert!(args(Kind::Antigravity, Access::Unrestricted).iter().any(|x| x == "--settings"), "Unrestricted grants by rules on every system");
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

    #[test]
    fn interactive_prompts_are_narrowly_detected() {
        assert_eq!(
            interactive_prompt("Welcome\nDo you trust this directory? (y/n) "),
            Some("Do you trust this directory? (y/n)".into())
        );
        assert_eq!(interactive_prompt("Password:"), Some("Password:".into()));
        assert_eq!(
            interactive_prompt("Press Enter to continue"),
            Some("Press Enter to continue".into())
        );
        assert!(interactive_prompt(r#"{"result":"Example: Continue? (y/n)"}"#).is_none());
        assert!(interactive_prompt("The tests discuss password: handling in prose.").is_none());
    }

    #[test]
    fn watchdog_tail_stays_bounded_on_utf8_boundaries() {
        let mut text = format!("{}end", "🟢".repeat(3_000));
        keep_tail(&mut text, 8_192);
        assert!(text.len() <= 8_192 && text.ends_with("end"));
    }
}

#[cfg(all(test, unix))]
mod tests {
    #[test]
    fn agy_model_list_keeps_only_gemini_models() {
        let out = "Fetching available models...\ngemini-3.8-flash-high\tGemini 3.8 Flash (High)\nclaude-sonnet-4-6\tClaude Sonnet 4.6 (Thinking)\ngemini-3.8-flash-low\tGemini 3.8 Flash (Low)\n";
        let ids: Vec<String> = parse_agy_models(out).into_iter().map(|m| m.id).collect();
        assert_eq!(ids, ["gemini-3.8-flash-high", "gemini-3.8-flash-low"]);
    }
    #[test]
    fn chosen_model_goes_where_each_cli_reads_it() {
        use std::ffi::OsString;
        let d = std::path::Path::new(".");
        let base = |k| plan(k, Access::Chat, d, d, "s", "p");
        let codex = with_model(base(Kind::Codex), Kind::Codex, Some("gpt-x"));
        assert_eq!(&codex.args[..3], &["exec", "--model", "gpt-x"].map(OsString::from)[..]);
        let claude = with_model(base(Kind::ClaudeCode), Kind::ClaudeCode, Some("opus"));
        assert_eq!(&claude.args[..2], &["--model", "opus"].map(OsString::from)[..]);
        let agy = with_model(base(Kind::Antigravity), Kind::Antigravity, Some(" gemini-3.8-flash-high "));
        assert_eq!(&agy.args[..2], &["--model", "gemini-3.8-flash-high"].map(OsString::from)[..]);
        // No model, a blank one, or one that looks like an option leaves the arguments alone.
        for m in [None, Some(""), Some("  "), Some("--dangerously-skip-permissions")] {
            assert_eq!(with_model(base(Kind::ClaudeCode), Kind::ClaudeCode, m).args, base(Kind::ClaudeCode).args);
        }
    }

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
    async fn structured_activity_is_streamed_before_the_reply_finishes() {
        let p = fake(
            r#"while [ "$1" != "-o" ]; do shift; done; out="$2"
cat >/dev/null
echo '{"type":"turn.started"}'
echo '{"type":"item.started","item":{"type":"command_execution","command":"npm test"}}'
printf 'done' > "$out""#,
        );
        let seen = Arc::new(std::sync::Mutex::new(Vec::<ActivitySignal>::new()));
        let captured = seen.clone();
        let sink: ActivitySink = Arc::new(move |activity| captured.lock().unwrap().push(activity));
        let reply = run_at_with_usage(
            &p,
            Kind::Codex,
            Access::Chat,
            None,
            "system",
            "prompt",
            &crate::attach::Files::default(),
            &CancellationToken::new(),
            RUN_TIMEOUT,
            Some(sink),
            Vec::new(),
            None,
        )
        .await
        .unwrap();
        assert_eq!(reply.text, "done");
        assert_eq!(
            *seen.lock().unwrap(),
            vec![signal("thinking", "Planning", ""), signal("running", "Running command", "npm test")]
        );
    }

    #[tokio::test]
    async fn unrestricted_turn_stops_on_a_streamed_destructive_command() {
        let p = fake(
            r#"while [ "$1" != "-o" ]; do shift; done; cat >/dev/null
echo '{"type":"item.started","item":{"type":"command_execution","command":"git reset --hard"}}'
sleep 30"#,
        );
        let ws = std::env::temp_dir().join(format!("consilium-guard-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&ws).unwrap();
        let started = std::time::Instant::now();
        let error = go(&p, Kind::Codex, Access::Unrestricted, Some(&ws), "system")
            .await
            .unwrap_err();
        let _ = std::fs::remove_dir_all(ws);
        assert!(error.contains("safety stop") && error.contains("REQUEST:"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[tokio::test]
    async fn elevated_command_needs_an_exact_grant_and_consumes_it_once() {
        let command = "sudo apt-get install widget";
        let script = format!(
            r#"while [ "$1" != "-o" ]; do shift; done; out="$2"; cat >/dev/null
echo '{{"type":"item.started","item":{{"type":"command_execution","command":"{command}"}}}}'
printf 'done' > "$out""#
        );
        let ws = std::env::temp_dir().join(format!("consilium-elevation-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&ws).unwrap();
        let p = fake(&script);
        let denied = go(&p, Kind::Codex, Access::Unrestricted, Some(&ws), "system")
            .await
            .unwrap_err();
        assert!(denied.contains("administrator approval required"), "{denied}");
        assert!(denied.contains("ELEVATED_COMMAND_JSON"), "{denied}");

        let approved = run_at_with_usage(
            &p,
            Kind::Codex,
            Access::Unrestricted,
            Some(&ws),
            "system",
            "prompt",
            &crate::attach::Files::default(),
            &CancellationToken::new(),
            RUN_TIMEOUT,
            None,
            vec![command.into()],
            None,
        )
        .await
        .unwrap();
        assert_eq!(approved.text, "done");
        let _ = std::fs::remove_dir_all(ws);
    }

    #[tokio::test]
    async fn install_needs_an_exact_install_grant() {
        let command = "cargo install ripgrep";
        let script = format!(
            r#"while [ "$1" != "-o" ]; do shift; done; out="$2"; cat >/dev/null
echo '{{"type":"item.started","item":{{"type":"command_execution","command":"{command}"}}}}'
printf 'done' > "$out""#
        );
        let ws = std::env::temp_dir().join(format!("consilium-install-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&ws).unwrap();
        let p = fake(&script);
        let denied = go(&p, Kind::Codex, Access::Unrestricted, Some(&ws), "system").await.unwrap_err();
        assert!(denied.contains("INSTALL_COMMAND_JSON"), "{denied}");
        let run = |grants: Vec<String>| {
            let (p, ws) = (p.clone(), ws.clone());
            async move {
                run_at_with_usage(&p, Kind::Codex, Access::Unrestricted, Some(&ws), "system", "prompt",
                    &crate::attach::Files::default(), &CancellationToken::new(), RUN_TIMEOUT, None, grants, None).await
            }
        };
        // A bare (elevation-style) grant must not authorise an install.
        assert!(run(vec![command.into()]).await.is_err());
        let ok = run(vec![format!("{}{command}", crate::prompt::INSTALL_GRANT_PREFIX)]).await.unwrap();
        assert_eq!(ok.text, "done");
        let _ = std::fs::remove_dir_all(ws);
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
    async fn claude_ending_without_a_result_gives_a_short_plain_error() {
        // A huge stream line and no result: one short line, not the raw stream.
        let big = "x".repeat(5000);
        let p = fake(&format!(r#"cat >/dev/null; printf '{{"type":"system","tools":"{big}"}}\n'; exit 1"#));
        let e = go(&p, Kind::ClaudeCode, Access::Chat, None, "s").await.unwrap_err();
        assert_eq!(e, "Claude CLI ended before reporting a final result (exit 1)");
        let p = fake(r#"cat >/dev/null; printf '{"type":"system"}\n'; exit 0"#);
        assert_eq!(go(&p, Kind::ClaudeCode, Access::Chat, None, "s").await.unwrap_err(), "Claude CLI ended before reporting a final result (exit 0)");
        // A long non-ASCII stderr is cut to its last characters, whole.
        let p = fake(r#"cat >/dev/null; i=0; while [ $i -lt 1000 ]; do printf 'é漢字'; i=$((i+1)); done >&2; exit 1"#);
        let e = go(&p, Kind::ClaudeCode, Access::Chat, None, "s").await.unwrap_err();
        assert!(e.starts_with("Claude CLI ended before reporting a final result (exit 1): "), "{e}");
        assert!(e.chars().count() < 500, "{} chars", e.chars().count());
    }

    #[tokio::test]
    async fn a_usage_limit_in_the_stream_without_a_result_still_benches() {
        let p = fake(
            r#"cat >/dev/null; printf '{"type":"assistant","error":"rate_limit","message":{"content":[{"type":"text","text":"Claude AI usage limit reached|1800003600"}]}}\n'; exit 1"#,
        );
        let e = go(&p, Kind::ClaudeCode, Access::Chat, None, "s").await.unwrap_err();
        assert_eq!(e, "rate_limit: Claude AI usage limit reached|1800003600");
        assert_eq!(crate::bench::usage_limit(&e, 1_800_000_000_000), Some(1_800_003_600_000));
        let p = fake(r#"cat >/dev/null; printf '{"type":"error","error":{"type":"api_error","message":"Overloaded"}}\n'; exit 1"#);
        assert_eq!(go(&p, Kind::ClaudeCode, Access::Chat, None, "s").await.unwrap_err(), "api_error: Overloaded");
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
        // Well under the 30 seconds a hang would take; GitHub's busy Mac runners need the slack.
        assert!(started.elapsed() < Duration::from_secs(20));
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

    #[tokio::test]
    async fn an_unexpected_interactive_prompt_fails_fast_with_a_request() {
        let p = fake("cat >/dev/null; printf 'Do you trust this directory? (y/n) ' >&2; sleep 30");
        // Without interception the fake CLI sleeps for 30 seconds. Leave ample headroom for
        // loaded CI hosts and process-group teardown while proving it cannot wait that long.
        let e = tokio::time::timeout(
            Duration::from_secs(20),
            go(&p, Kind::ClaudeCode, Access::Chat, None, "s"),
        )
        .await
        .expect("the prompt watchdog ends the run")
        .unwrap_err();
        assert!(
            e.contains("waiting for interactive input") && e.contains("REQUEST:"),
            "{e}"
        );
    }

    #[tokio::test]
    async fn child_processes_receive_unbuffered_output_environment() {
        let p = fake(
            r#"cat >/dev/null; printf '{"response":"%s/%s/%s"}' "$PYTHONUNBUFFERED" "$NO_COLOR" "$FORCE_COLOR""#,
        );
        assert_eq!(
            go(&p, Kind::GeminiCli, Access::Chat, None, "s")
                .await
                .unwrap(),
            "1/1/0"
        );
    }

    #[test]
    fn built_in_flagship_agents_are_not_local() {
        for id in ["gpt", "claude", "gemini"] {
            let a = by_id(id).expect("builtin agent exists");
            assert!(!is_local(a), "{id} should be cloud/flagship, not local");
            assert!(!is_local_id(id), "{id} id check should not be local");
        }
    }
}
