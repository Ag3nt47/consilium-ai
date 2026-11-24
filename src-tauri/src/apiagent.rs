//! AIs reached over an OpenAI-compatible HTTP API with the person's own API key: Grok (xAI) and
//! DeepSeek. Neither offers a command-line tool that signs in with a personal account, so this is
//! the only way in.
//!
//! Rules:
//! - Keys are typed into Consilium's own Add AI window, never into the chat. They are stored in
//!   `api-keys.json` in the app's data folder, readable only by this user (0600 on Unix), or read
//!   from `XAI_API_KEY` / `DEEPSEEK_API_KEY` when those are set.
//! - Keys are never returned to the frontend, logged or put into an error message.
//! - These AIs only see the conversation: an HTTP chat API has no tools, so they never touch the
//!   project folder whatever access level is set.

use crate::store::TokenUsage;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const CHECK_TIMEOUT: Duration = Duration::from_secs(10);

pub struct Provider {
    pub base_url: &'static str,
    pub env_var: &'static str,
    /// Where the person creates a key.
    pub key_page: &'static str,
    /// Model ids to prefer, in order, matched as substrings of what `/models` lists. The first id
    /// the account actually has wins, so no outdated id is pinned.
    pub prefer: &'static [&'static str],
    /// Only ids containing this are considered at all.
    pub family: &'static str,
    /// A model on this computer or the home network: a key is optional.
    pub local: bool,
}

pub const XAI: Provider = Provider {
    base_url: "https://api.x.ai/v1",
    env_var: "XAI_API_KEY",
    key_page: "console.x.ai",
    // Cheapest capable models first: the person is on xAI's free allowance.
    prefer: &["grok-4-fast", "grok-3-mini", "mini", "fast"],
    family: "grok",
    local: false,
};

pub const DEEPSEEK: Provider = Provider {
    base_url: "https://api.deepseek.com",
    env_var: "DEEPSEEK_API_KEY",
    key_page: "platform.deepseek.com",
    prefer: &["deepseek-chat"],
    family: "deepseek",
    local: false,
};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Entry {
    #[serde(default)]
    key: String,
    /// The model chosen from the account's own list, remembered so each turn needn't ask again.
    #[serde(default)]
    model: Option<String>,
}

static PATH: OnceLock<PathBuf> = OnceLock::new();
static LOCK: Mutex<()> = Mutex::new(());

/// Called once at startup with the app data folder's `api-keys.json`.
pub fn init(path: PathBuf) {
    let _ = PATH.set(path);
}

fn load() -> HashMap<String, Entry> {
    PATH.get()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save(all: &HashMap<String, Entry>) -> Result<(), String> {
    let path = PATH.get().ok_or("key storage isn't ready")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("json.tmp");
    let body = serde_json::to_string_pretty(all).map_err(|e| e.to_string())?;
    {
        use std::io::Write;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp).map_err(|e| e.to_string())?;
        f.write_all(body.as_bytes()).map_err(|e| e.to_string())?;
        f.sync_all().map_err(|e| e.to_string())?;
    }
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

fn key(agent: &str, provider: &Provider) -> Option<String> {
    let stored = load().get(agent).map(|e| e.key.trim().to_string()).filter(|k| !k.is_empty());
    let env = || {
        (!provider.env_var.is_empty())
            .then(|| std::env::var(provider.env_var).ok())
            .flatten()
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty())
    };
    // A local model server usually wants no key at all; an empty one means "send none".
    stored.or_else(env).or_else(|| provider.local.then(String::new))
}

/// Adds the key to a request, unless there is none (a local model server).
fn auth(req: reqwest::RequestBuilder, key: &str) -> reqwest::RequestBuilder {
    if key.is_empty() { req } else { req.bearer_auth(key) }
}

pub fn has_key(agent: &str, provider: &Provider) -> bool {
    key(agent, provider).is_some()
}

/// Saves (or, with an empty key, removes) an agent's key. The model is re-chosen on next use.
pub fn set_key(agent: &str, value: &str) -> Result<(), String> {
    let value = value.trim();
    if value.chars().any(char::is_whitespace) || value.len() > 512 {
        return Err("that doesn't look like an API key".into());
    }
    let _g = LOCK.lock().unwrap();
    let mut all = load();
    if value.is_empty() {
        all.remove(agent);
    } else {
        all.insert(agent.to_string(), Entry { key: value.to_string(), model: None });
    }
    save(&all)
}

fn remember_model(agent: &str, model: &str) {
    let _g = LOCK.lock().unwrap();
    let mut all = load();
    // A key from the environment has no entry; don't create one holding an empty key.
    if let Some(e) = all.get_mut(agent) {
        if e.model.as_deref() != Some(model) {
            e.model = Some(model.to_string());
            let _ = save(&all);
        }
    }
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent("Consilium")
        .build()
        .map_err(|e| format!("couldn't start an HTTP client: {e}"))
}

/// Turns a failed response into a message that never contains the key.
fn http_error(name: &str, status: reqwest::StatusCode, body: &str) -> String {
    let detail = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            v["error"]["message"]
                .as_str()
                .or_else(|| v["error"].as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| body.chars().take(300).collect());
    let what = match status.as_u16() {
        401 | 403 => "the API key was rejected",
        402 => "the account has no credit left",
        429 => "rate limit or quota reached",
        _ => "request failed",
    };
    crate::redact::scrub(&format!("{name}: {what} (HTTP {status}): {detail}"))
}

/// The account's model ids, from the provider's `/models` list.
async fn models(name: &str, provider: &Provider, key: &str) -> Result<Vec<String>, String> {
    let resp = auth(client()?.get(format!("{}/models", provider.base_url)), key)
        .timeout(CHECK_TIMEOUT)
        .send()
        .await
        .map_err(|e| format!("{name}: couldn't reach {}: {}", provider.base_url, e.without_url()))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(http_error(name, status, &body));
    }
    let v: serde_json::Value = serde_json::from_str(&body).map_err(|_| format!("{name}: unreadable model list"))?;
    Ok(v["data"]
        .as_array()
        .map(|a| a.iter().filter_map(|m| m["id"].as_str().map(str::to_string)).collect())
        .unwrap_or_default())
}

/// Picks the model to use from the account's list: the first preferred match, else the first model
/// of the family.
pub fn choose_model(provider: &Provider, ids: &[String]) -> Option<String> {
    let family: Vec<&String> = ids.iter().filter(|id| id.contains(provider.family)).collect();
    provider
        .prefer
        .iter()
        .find_map(|p| family.iter().find(|id| id.contains(p)))
        .or_else(|| family.first())
        .map(|s| s.to_string())
}

/// Sign-in check: the key is present and the provider accepts it. Returns the model in use.
pub async fn check(agent: &str, name: &str, provider: &Provider) -> Result<String, String> {
    let key = key(agent, provider).ok_or_else(|| format!("No {name} API key yet."))?;
    let ids = models(name, provider, &key).await?;
    if provider.family.is_empty() {
        if let Some(m) = provider.prefer.first() {
            return Ok(m.to_string()); // reachable, and the person named the model
        }
    }
    let model = choose_model(provider, &ids).ok_or_else(|| format!("{name}: no usable model on this account"))?;
    remember_model(agent, &model);
    Ok(model)
}

async fn model_for(agent: &str, name: &str, provider: &Provider, key: &str) -> Result<String, String> {
    if let Some(m) = load().get(agent).and_then(|e| e.model.clone()) {
        return Ok(m);
    }
    if provider.family.is_empty() {
        if let Some(m) = provider.prefer.first() {
            return Ok(m.to_string()); // the person named the model when adding this AI
        }
    }
    let ids = models(name, provider, key).await?;
    let model = choose_model(provider, &ids).ok_or_else(|| format!("{name}: no usable model on this account"))?;
    remember_model(agent, &model);
    Ok(model)
}

fn usage_of(v: &serde_json::Value) -> Option<TokenUsage> {
    let u = &v["usage"];
    let n = |k: &str| u[k].as_u64().unwrap_or(0);
    let input_tokens = n("prompt_tokens");
    let output_tokens = n("completion_tokens");
    let thinking_tokens = u["completion_tokens_details"]["reasoning_tokens"].as_u64().unwrap_or(0);
    let cache_read_tokens = u["prompt_tokens_details"]["cached_tokens"]
        .as_u64()
        .or_else(|| u["prompt_cache_hit_tokens"].as_u64())
        .unwrap_or(0);
    let total_tokens = n("total_tokens").max(input_tokens.saturating_add(output_tokens));
    // llama.cpp includes measured generation timing in some compatible chat replies.
    let output_rate_milli = v.pointer("/timings/predicted_per_second")
        .and_then(serde_json::Value::as_f64)
        .filter(|rate| rate.is_finite() && *rate > 0.0)
        .map(|rate| (rate * 1000.0).round() as u64)
        .filter(|rate| *rate > 0);
    (total_tokens > 0).then_some(TokenUsage {
        input_tokens,
        output_tokens,
        thinking_tokens,
        cache_read_tokens,
        total_tokens,
        output_rate_milli,
        ..TokenUsage::default()
    })
}

/// `text` without `<think>…</think>` blocks; an unclosed one runs to the end.
pub fn strip_think(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(i) = rest.find("<think>") {
        out.push_str(&rest[..i]);
        match rest[i..].find("</think>") {
            Some(j) => rest = &rest[i + j + "</think>".len()..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// Pulls the reply text and token counts out of a chat-completions response body.
pub fn parse_reply(body: &str) -> Result<(String, Option<TokenUsage>), String> {
    let v: serde_json::Value = serde_json::from_str(body).map_err(|_| "unreadable reply".to_string())?;
    let raw = v["choices"][0]["message"]["content"].as_str().unwrap_or_default();
    // Reasoning models (DeepSeek-R1, Qwen3) put their hidden reasoning in the reply as
    // <think>…</think>; the council should see only the answer.
    let text = strip_think(raw).trim().to_string();
    if text.is_empty() {
        return Err("empty reply".into());
    }
    Ok((text, usage_of(&v)))
}

/// One council turn.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    agent: &str,
    name: &str,
    provider: &Provider,
    system: &str,
    prompt: &str,
    cancel: &CancellationToken,
    timeout: Duration,
    window: Option<usize>,
) -> Result<(String, Option<TokenUsage>, String), String> {
    let key = key(agent, provider).ok_or_else(|| format!("No {name} API key. Add one in Add AI."))?;
    let model = model_for(agent, name, provider, &key).await?;
    // A local model's window is small: fit the prompt to the size its server reports.
    let (system, prompt) = match window {
        Some(ctx) => fit_to_window(system, prompt, ctx),
        None => (system.to_string(), prompt.to_string()),
    };
    let (system, prompt) = (system.as_str(), prompt.as_str());
    match ask_once(name, provider, &key, &model, system, prompt, cancel, timeout).await {
        Ok(r) => Ok((r.0, r.1, model)),
        // The server named its window in the refusal: fit to that and ask once more.
        Err(e) if provider.local && !cancel.is_cancelled() && context_from_error(&e).is_some() => {
            let ctx = context_from_error(&e).unwrap_or(4096);
            // The refusal also says how many tokens this prompt really was: measure the ratio,
            // so a prompt the estimate let through is cut by enough the second time.
            let chars = system.chars().count() + prompt.chars().count();
            let cpt = request_tokens(&e)
                .map(|n| (chars as f64 / n as f64 * 0.9).min(CHARS_PER_TOKEN as f64))
                .unwrap_or(CHARS_PER_TOKEN as f64 / 2.0);
            let (system, prompt) = fit_to_window_at(system, prompt, ctx, cpt);
            let r = ask_once(name, provider, &key, &model, &system, &prompt, cancel, timeout).await?;
            Ok((r.0, r.1, model))
        }
        Err(e) if e != "stopped" && provider.local && !cancel.is_cancelled() => {
            // A local model's turn shouldn't be lost to a hiccup: start Ollama if it is down on
            // this computer, and ask once more, saying why when the answer came back empty.
            if is_unreachable(&e) && !start_local_ollama(provider.base_url).await {
                return Err(e);
            }
            let system = if e.ends_with("empty reply") {
                format!("{system}\n\nNOTE FROM CONSILIUM: your last answer to this came back empty (or only hidden reasoning). Reply in plain text now.")
            } else {
                system.to_string()
            };
            let r = ask_once(name, provider, &key, &model, &system, prompt, cancel, timeout).await?;
            Ok((r.0, r.1, model))
        }
        Err(e) => Err(e),
    }
}

/// Characters per token when budgeting a local window: low on purpose (code, paths and
/// non-English text run denser than prose), so the estimate errs toward fitting.
pub(crate) const CHARS_PER_TOKEN: usize = 3;

/// The context window (tokens) a local server has loaded its model with: llama.cpp's `/props`.
/// None when the server doesn't say (Ollama), so the prompt goes as it is.
pub async fn local_window(provider: &Provider) -> Option<usize> {
    if !provider.local {
        return None;
    }
    let base_url = provider.base_url;
    let root = base_url.trim_end_matches('/').trim_end_matches("/v1");
    let resp = client().ok()?.get(format!("{root}/props")).timeout(Duration::from_secs(3)).send().await.ok()?;
    let v: serde_json::Value = resp.json().await.ok()?;
    let n = v.pointer("/default_generation_settings/n_ctx").or_else(|| v.get("n_ctx"))?.as_u64()?;
    usize::try_from(n).ok().filter(|n| *n >= 512)
}

/// The window named in a refusal like llama.cpp's "exceeds the available context size (8192 tokens)".
fn context_from_error(error: &str) -> Option<usize> {
    const MARK: &str = "context size (";
    let rest = &error[error.find(MARK)? + MARK.len()..];
    rest.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok().filter(|n| *n >= 512)
}

/// The token count a refusal says the prompt was: "request (21362 tokens) exceeds …".
fn request_tokens(error: &str) -> Option<usize> {
    const MARK: &str = "request (";
    let rest = &error[error.find(MARK)? + MARK.len()..];
    rest.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok().filter(|n| *n > 0)
}

/// The first `n` characters of `text`.
fn first_chars(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}

/// `system` and `prompt` cut to fit a `ctx`-token window, leaving room for the reply. The system
/// prompt keeps its start (who the AI is, the rules) and takes at most half; the conversation
/// keeps its opening lines and its newest part, where the question is.
pub fn fit_to_window(system: &str, prompt: &str, ctx: usize) -> (String, String) {
    fit_to_window_at(system, prompt, ctx, CHARS_PER_TOKEN as f64)
}

/// `fit_to_window` with a given characters-per-token ratio.
fn fit_to_window_at(system: &str, prompt: &str, ctx: usize, chars_per_token: f64) -> (String, String) {
    let budget = (ctx.saturating_sub((ctx / 4).min(1024)) as f64 * chars_per_token.max(1.0)) as usize;
    let (s_len, p_len) = (system.chars().count(), prompt.chars().count());
    if s_len + p_len <= budget {
        return (system.to_string(), prompt.to_string());
    }
    let system = if s_len > budget / 2 {
        format!("{}\n\n[… the rest of these instructions was left out to fit your context window]", first_chars(system, budget / 2))
    } else {
        system.to_string()
    };
    let room = budget.saturating_sub(system.chars().count());
    if p_len <= room {
        return (system, prompt.to_string());
    }
    let note = format!("\n\n[… {} characters of older conversation left out to fit your context window …]\n\n", p_len.saturating_sub(room));
    let head = (room / 10).min(400);
    let tail_len = room.saturating_sub(head + note.chars().count());
    let tail: String = prompt.chars().skip(p_len - tail_len).collect();
    (system, format!("{}{note}{tail}", first_chars(prompt, head)))
}

fn is_unreachable(error: &str) -> bool {
    let e = error.to_lowercase();
    e.contains("connect") || e.contains("refused") || e.contains("unreachable")
}

/// Starts `ollama serve` when a local AI's endpoint is this computer and nothing answers there,
/// then waits up to 20 s for it. Whether it answers now.
async fn start_local_ollama(base_url: &str) -> bool {
    let lower = base_url.to_lowercase();
    if !(lower.contains("127.0.0.1") || lower.contains("localhost")) {
        return false;
    }
    let mut cmd = tokio::process::Command::new("ollama");
    cmd.arg("serve").stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    if cmd.spawn().is_err() {
        return false;
    }
    let tags = format!("{}/api/tags", base_url.trim_end_matches('/').trim_end_matches("/v1"));
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        if let Ok(c) = client() {
            if c.get(&tags).timeout(Duration::from_secs(2)).send().await.is_ok_and(|r| r.status().is_success()) {
                return true;
            }
        }
    }
    false
}

#[allow(clippy::too_many_arguments)]
async fn ask_once(
    name: &str,
    provider: &Provider,
    key: &str,
    model: &str,
    system: &str,
    prompt: &str,
    cancel: &CancellationToken,
    timeout: Duration,
) -> Result<(String, Option<TokenUsage>), String> {
    let body = serde_json::json!({
        "model": model,
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": prompt },
        ],
        "stream": false,
    });
    let request = auth(client()?.post(format!("{}/chat/completions", provider.base_url)), key)
        .json(&body)
        .timeout(timeout)
        .send();
    let resp = tokio::select! {
        r = request => r.map_err(|e| format!("{name}: {}", e.without_url()))?,
        _ = cancel.cancelled() => return Err("stopped".into()),
    };
    let status = resp.status();
    let text = tokio::select! {
        t = resp.text() => t.unwrap_or_default(),
        _ = cancel.cancelled() => return Err("stopped".into()),
    };
    if !status.is_success() {
        return Err(http_error(name, status, &text));
    }
    parse_reply(&text).map_err(|e| format!("{name}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_reasoning_is_dropped_and_a_reply_of_only_reasoning_counts_as_empty() {
        assert_eq!(strip_think("<think>plan it</think>\nThe answer."), "\nThe answer.");
        assert_eq!(strip_think("a<think>x</think>b<think>y</think>c"), "abc");
        assert_eq!(strip_think("ok <think>never closed"), "ok ");
        let only = r#"{"choices":[{"message":{"content":"<think>hmm</think>  "}}]}"#;
        assert_eq!(parse_reply(only).unwrap_err(), "empty reply");
        assert!(is_unreachable("Qwen: error sending request: tcp connect error: connection refused"));
        assert!(!is_unreachable("Qwen: HTTP 500"));
    }

    #[test]
    fn local_prompt_is_fitted_to_the_window_keeping_rules_and_newest_messages() {
        let system = format!("RULES{}", "s".repeat(30_000));
        let prompt = format!("CONVERSATION SO FAR{}END OF CONVERSATION. Write now.", "m".repeat(60_000));
        let (s, p) = fit_to_window(&system, &prompt, 8192);
        let budget = (8192 - 1024) * CHARS_PER_TOKEN;
        assert!(s.chars().count() + p.chars().count() <= budget + 200, "{} + {}", s.len(), p.len());
        assert!(s.starts_with("RULES") && p.starts_with("CONVERSATION SO FAR"));
        assert!(p.ends_with("END OF CONVERSATION. Write now."));
        assert!(p.contains("left out to fit your context window"));
        // A prompt that fits is left alone.
        assert_eq!(fit_to_window("a", "b", 8192), ("a".to_string(), "b".to_string()));
        let e = "Qwen: request failed (HTTP 400 Bad Request): request (21051 tokens) exceeds the available context size (8192 tokens), try increasing it";
        assert_eq!(context_from_error(e), Some(8192));
        assert_eq!(context_from_error("Qwen: HTTP 500"), None);
        assert_eq!(request_tokens(e), Some(21051));
        // The retry at a measured, denser ratio cuts harder than the estimate.
        let (s2, p2) = fit_to_window_at(&system, &prompt, 8192, 1.5);
        assert!(s2.chars().count() + p2.chars().count() < s.chars().count() + p.chars().count());
        assert!(p2.ends_with("END OF CONVERSATION. Write now."));
    }

    #[test]
    fn chooses_preferred_model_from_account_list() {
        let ids: Vec<String> = ["grok-2-vision", "grok-3-mini", "grok-4", "embed-x"].iter().map(|s| s.to_string()).collect();
        assert_eq!(choose_model(&XAI, &ids).as_deref(), Some("grok-3-mini"));
        let ids: Vec<String> = ["grok-4"].iter().map(|s| s.to_string()).collect();
        assert_eq!(choose_model(&XAI, &ids).as_deref(), Some("grok-4"));
        let ids: Vec<String> = ["deepseek-reasoner", "deepseek-chat"].iter().map(|s| s.to_string()).collect();
        assert_eq!(choose_model(&DEEPSEEK, &ids).as_deref(), Some("deepseek-chat"));
        assert_eq!(choose_model(&DEEPSEEK, &[]), None);
    }

    #[test]
    fn parses_chat_completion_with_usage() {
        let body = r#"{"choices":[{"message":{"role":"assistant","content":" Hello "}}],
            "usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15,
            "completion_tokens_details":{"reasoning_tokens":3},"prompt_cache_hit_tokens":4}}"#;
        let (text, usage) = parse_reply(body).unwrap();
        assert_eq!(text, "Hello");
        let u = usage.unwrap();
        assert_eq!((u.input_tokens, u.output_tokens, u.thinking_tokens, u.cache_read_tokens, u.total_tokens), (10, 5, 3, 4, 15));
        assert_eq!(u.output_rate_milli, None);
        assert!(parse_reply(r#"{"choices":[{"message":{"content":""}}]}"#).is_err());
    }

    #[test]
    fn generation_throughput_uses_only_the_providers_reported_rate() {
        let body = r#"{"choices":[{"message":{"content":"A reply"}}],"usage":{"completion_tokens":35},"timings":{"prompt_per_second":300.3,"predicted_per_second":52.944949}}"#;
        let (_, usage) = parse_reply(body).unwrap();
        assert_eq!(usage.unwrap().output_rate_milli, Some(52_945));
        for invalid in ["0", "-7", "null", "\"48.2\""] {
            let body = body.replace("52.944949", invalid);
            assert_eq!(parse_reply(&body).unwrap().1.unwrap().output_rate_milli, None);
        }
    }

    #[test]
    fn errors_never_echo_the_key() {
        let msg = http_error(
            "Grok",
            reqwest::StatusCode::UNAUTHORIZED,
            r#"{"error":{"message":"Incorrect API key provided: xai-ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"}}"#,
        );
        assert!(msg.contains("rejected"));
        assert!(!msg.contains("ABCDEFGHIJKLMNOPQRSTUVWXYZ"), "{msg}");
    }
}
