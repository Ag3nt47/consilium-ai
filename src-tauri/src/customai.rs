//! AIs the person adds themselves: any service with an OpenAI-compatible chat API (OpenRouter,
//! Mistral, Groq, the OpenAI or Anthropic APIs, …) or a model running locally (Ollama, LM Studio,
//! llama.cpp). They join exactly like Grok and DeepSeek: they see the conversation and answer it,
//! and never touch the project folder.
//!
//! Rules:
//! - The list lives in `custom-ais.json` in the app's data folder. Keys are kept apart, in
//!   `api-keys.json`, by `apiagent`, and never written here.
//! - A remote service must use https, so a key never crosses the network in the clear. Plain http
//!   is only allowed to this computer or a private home-network address, which is where local
//!   model servers listen.
//! - Entries live for the whole run once loaded (agents are handed out as `&'static`), so an AI
//!   that is removed simply stops being listed. The few bytes it held are not reclaimed.

use crate::agents::{Agent, Kind};
use crate::apiagent::Provider;
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock, RwLock};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomAi {
    pub id: String,
    pub name: String,
    pub base_url: String,
    /// The model to use; empty means the first one the service lists.
    #[serde(default)]
    pub model: String,
    /// Runs on this computer or the home network, so no key is needed.
    #[serde(default)]
    pub local: bool,
}

static PATH: OnceLock<PathBuf> = OnceLock::new();
static LIST: RwLock<Vec<&'static Agent>> = RwLock::new(Vec::new());
static WRITE: Mutex<()> = Mutex::new(());

/// Names an added AI can't take: the built-in AIs and words with another meaning in a mention.
const RESERVED: &[&str] = &["human", "all", "everyone", "consilium", "you", "me", "system", "user", "assistant"];
const MAX_AIS: usize = 16;

/// Called once at startup, before the council settings are read (they drop unknown members).
pub fn init(path: PathBuf) {
    if PATH.set(path).is_err() {
        return; // the app and the terminal command share one process only in tests
    }
    let mut list = LIST.write().unwrap();
    for c in load() {
        if crate::agents::builtin(&c.id).is_none() && validate(&c).is_ok() && !list.iter().any(|a| a.id == c.id) {
            list.push(leak(&c));
        }
    }
}

fn load() -> Vec<CustomAi> {
    PATH.get()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save(all: &[CustomAi]) -> Result<(), String> {
    let path = PATH.get().ok_or("settings storage isn't ready")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let data = serde_json::to_vec_pretty(all).map_err(|e| e.to_string())?;
    crate::store::write_atomic(path, &data).map_err(|e| e.to_string())
}

pub fn list() -> Vec<&'static Agent> {
    LIST.read().unwrap().clone()
}

pub fn by_id(id: &str) -> Option<&'static Agent> {
    LIST.read().unwrap().iter().copied().find(|a| a.id == id)
}

fn leak_str(s: &str) -> &'static str {
    Box::leak(s.to_string().into_boxed_str())
}

fn leak(c: &CustomAi) -> &'static Agent {
    let host = host_of(&c.base_url).unwrap_or_default();
    let prefer: &'static [&'static str] =
        if c.model.is_empty() { &[] } else { Box::leak(vec![leak_str(&c.model)].into_boxed_slice()) };
    let provider: &'static Provider = Box::leak(Box::new(Provider {
        base_url: leak_str(&c.base_url),
        env_var: "",
        key_page: leak_str(&host),
        prefer,
        family: "",
        local: c.local,
    }));
    let company = if c.local { "Local model".to_string() } else { host };
    Box::leak(Box::new(Agent {
        id: leak_str(&c.id),
        name: leak_str(&c.name),
        company: leak_str(&company),
        kind: Kind::CustomApi,
        provider: Some(provider),
    }))
}

/// The @mention id for a name: lowercase letters and digits only.
pub fn slug(name: &str) -> String {
    name.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_lowercase()
}

fn host_of(url: &str) -> Option<String> {
    reqwest::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_string))
}

/// Whether `host` is this computer or a private home-network address.
fn is_near(host: &str) -> bool {
    let h = host.trim_start_matches('[').trim_end_matches(']');
    if h.eq_ignore_ascii_case("localhost") || h.ends_with(".local") {
        return true;
    }
    match h.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => ip.is_loopback() || ip.is_private() || (ip.is_link_local() && ip.octets()[2..] != [169, 254]),
        Ok(IpAddr::V6(ip)) => ip.is_loopback() || (ip.segments()[0] & 0xfe00) == 0xfc00,
        Err(_) => false,
    }
}

/// Checks a service address and returns it tidied (no trailing slash).
pub fn check_url(raw: &str) -> Result<String, String> {
    let raw = raw.trim().trim_end_matches('/');
    if raw.is_empty() || raw.len() > 200 {
        return Err("Enter the service's API address, such as https://openrouter.ai/api/v1".into());
    }
    let u = reqwest::Url::parse(raw).map_err(|_| "That isn't a web address. It should start with https://".to_string())?;
    if !u.username().is_empty() || u.password().is_some() || u.query().is_some() || u.fragment().is_some() {
        return Err("Leave names, passwords and anything after ? out of the address; the key goes in its own box.".into());
    }
    let host = u.host_str().ok_or("That address has no server name.")?;
    match u.scheme() {
        "https" => {}
        "http" if is_near(host) => {}
        "http" => return Err("A service on the internet must use https://, so your key isn't sent unencrypted. Plain http:// is only for this computer or your home network.".into()),
        _ => return Err("The address must start with https:// (or http:// for a model on this computer).".into()),
    }
    Ok(raw.to_string())
}

fn validate(c: &CustomAi) -> Result<(), String> {
    let n = c.name.trim();
    if n.is_empty() || n.chars().count() > 24 || n.chars().any(|ch| ch.is_control() || "@[]<>`".contains(ch)) {
        return Err("Give it a name of up to 24 characters, without @, brackets or backticks.".into());
    }
    if c.id.len() < 2 || c.id != slug(&c.id) || RESERVED.contains(&c.id.as_str()) {
        return Err("The name needs at least two letters or digits, and can't be a word like \"human\" or \"all\".".into());
    }
    if c.model.len() > 120 || c.model.chars().any(|ch| ch.is_whitespace() || ch.is_control()) {
        return Err("The model name can't contain spaces.".into());
    }
    if check_url(&c.base_url)? != c.base_url {
        return Err("unexpected address form".into());
    }
    Ok(())
}

/// Adds an AI and returns its @mention id.
pub fn add(name: &str, base_url: &str, model: &str, local: bool) -> Result<String, String> {
    let _g = WRITE.lock().unwrap();
    let c = CustomAi {
        id: slug(name),
        name: name.trim().to_string(),
        base_url: check_url(base_url)?,
        model: model.trim().to_string(),
        local,
    };
    validate(&c)?;
    if crate::agents::builtin(&c.id).is_some() || by_id(&c.id).is_some() {
        return Err(format!("There's already an AI called @{}. Pick another name.", c.id));
    }
    let mut all = load();
    if all.len() >= MAX_AIS {
        return Err(format!("You can add up to {MAX_AIS} AIs of your own. Remove one first."));
    }
    all.retain(|x| x.id != c.id);
    all.push(c.clone());
    save(&all)?;
    LIST.write().unwrap().push(leak(&c));
    Ok(c.id)
}

pub fn remove(id: &str) -> Result<(), String> {
    let _g = WRITE.lock().unwrap();
    if by_id(id).is_none() {
        return Err("Only AIs you added yourself can be deleted.".into());
    }
    let mut all = load();
    all.retain(|x| x.id != id);
    save(&all)?;
    LIST.write().unwrap().retain(|a| a.id != id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_services_need_https_and_local_ones_may_use_http() {
        assert_eq!(check_url("https://openrouter.ai/api/v1/").unwrap(), "https://openrouter.ai/api/v1");
        assert!(check_url("http://localhost:11434/v1").is_ok());
        assert!(check_url("http://127.0.0.1:1234/v1").is_ok());
        assert!(check_url("http://192.168.1.157:11434/v1").is_ok());
        assert!(check_url("http://[::1]:8080/v1").is_ok());
        assert!(check_url("http://api.example.com/v1").is_err());
        assert!(check_url("http://169.254.169.254/latest").is_err());
        assert!(check_url("https://user:pw@example.com/v1").is_err());
        assert!(check_url("https://example.com/v1?key=abc").is_err());
        assert!(check_url("ftp://example.com").is_err());
        assert!(check_url("").is_err());
    }

    #[test]
    fn names_become_safe_mention_ids() {
        assert_eq!(slug("Mistral Large"), "mistrallarge");
        assert_eq!(slug("Llama-3 (local)"), "llama3local");
        let ok = |name: &str| CustomAi {
            id: slug(name),
            name: name.into(),
            base_url: "https://api.mistral.ai/v1".into(),
            model: String::new(),
            local: false,
        };
        assert!(validate(&ok("Mistral")).is_ok());
        assert!(validate(&ok("All")).is_err());
        assert!(validate(&ok("@x")).is_err());
        assert!(validate(&ok("Q")).is_err());
        assert!(validate(&CustomAi { model: "two words".into(), ..ok("Mistral") }).is_err());
    }
}
