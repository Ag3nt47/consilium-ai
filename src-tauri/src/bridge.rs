//! Signal and cross-workstation messaging bridge.
//!
//! Provides loopback-only JSON-RPC connectivity to `signal-cli`, strict sender ACI/UUID allowlisting,
//! message deduplication (by sender, device, and timestamp), secret-scrubbed outbound notifications,
//! and automatic translation of mobile "Approve" / "Deny" responses into in-chat REQUEST decisions.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

/// Default loopback endpoint for a local `signal-cli` JSON-RPC daemon.
pub const DEFAULT_ENDPOINT: &str = "127.0.0.1:7583";

/// Local messaging configuration. Kept strictly out of git and restricted to mode 0600 on Unix.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BridgeConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_endpoint")]
    pub endpoint: String,
    #[serde(default)]
    pub account: String,
    #[serde(default)]
    pub allowed_acis: Vec<String>,
    #[serde(default = "default_true")]
    pub auto_forward_decisions: bool,
}

fn default_endpoint() -> String {
    DEFAULT_ENDPOINT.into()
}

fn default_true() -> bool {
    true
}

impl Default for BridgeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            endpoint: DEFAULT_ENDPOINT.into(),
            account: String::new(),
            allowed_acis: Vec::new(),
            auto_forward_decisions: true,
        }
    }
}

impl BridgeConfig {
    /// Loads messaging configuration from the given path or returns default if not present.
    pub fn load(path: &Path) -> Result<Self, String> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let data = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read messaging config {}: {e}", path.display()))?;
        serde_json::from_str(&data)
            .map_err(|e| format!("invalid messaging config in {}: {e}", path.display()))
    }

    /// Saves configuration to disk with mode 0600 permissions on Unix platforms.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create config directory {}: {e}", parent.display()))?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("cannot serialize messaging config: {e}"))?;
        std::fs::write(path, json)
            .map_err(|e| format!("cannot write messaging config {}: {e}", path.display()))?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o600);
            let _ = std::fs::set_permissions(path, perms);
        }
        Ok(())
    }

    /// Validates that the endpoint is strictly bound to local loopback (or local unix socket).
    pub fn validate_endpoint(&self) -> Result<(), String> {
        validate_loopback(&self.endpoint)
    }

    /// Checks if a sender's ACI / UUID is explicitly in the allowlist.
    pub fn is_sender_allowed(&self, sender_aci: &str) -> bool {
        let needle = sender_aci.trim();
        if needle.is_empty() {
            return false;
        }
        self.allowed_acis
            .iter()
            .any(|a| a.trim().eq_ignore_ascii_case(needle))
    }
}

/// Verifies that an endpoint string represents a loopback address or local unix domain socket.
/// Rejects remote hosts and external IP addresses to prevent telemetry or credential leakage.
pub fn validate_loopback(endpoint: &str) -> Result<(), String> {
    let ep = endpoint.trim();
    if ep.is_empty() {
        return Err("endpoint cannot be empty".into());
    }

    // Unix domain sockets are local-only
    if ep.starts_with('/') || ep.starts_with("unix:") {
        return Ok(());
    }

    // Strip scheme if present
    let raw = ep
        .strip_prefix("tcp://")
        .or_else(|| ep.strip_prefix("http://"))
        .unwrap_or(ep);

    // Try parsing as SocketAddr (e.g. "127.0.0.1:7583" or "[::1]:7583")
    if let Ok(addr) = raw.parse::<SocketAddr>() {
        if addr.ip().is_loopback() {
            return Ok(());
        }
        return Err(format!(
            "endpoint must be local loopback only: {} is not loopback",
            addr.ip()
        ));
    }

    // Try host:port split with named localhost
    if let Some((host, _port)) = raw.rsplit_once(':') {
        let h = host.trim_start_matches('[').trim_end_matches(']');
        if h.eq_ignore_ascii_case("localhost") || h == "127.0.0.1" || h == "::1" {
            return Ok(());
        }
    } else if raw.eq_ignore_ascii_case("localhost") || raw == "127.0.0.1" || raw == "::1" {
        return Ok(());
    }

    Err(format!(
        "endpoint must be local loopback only; remote or external host rejected: {endpoint}"
    ))
}

/// Message deduplicator maintaining a bounded ring of recently seen message identifiers.
#[derive(Debug, Default)]
pub struct MessageDeduplicator {
    seen: VecDeque<(String, u32, u64)>,
    capacity: usize,
}

impl MessageDeduplicator {
    pub fn new(capacity: usize) -> Self {
        Self {
            seen: VecDeque::with_capacity(capacity),
            capacity: capacity.max(32),
        }
    }

    /// Returns true if this message was already processed. If not, remembers it.
    pub fn is_duplicate(&mut self, sender: &str, device: u32, timestamp: u64) -> bool {
        let key = (sender.trim().to_ascii_lowercase(), device, timestamp);
        if self.seen.iter().any(|k| *k == key) {
            return true;
        }
        if self.seen.len() >= self.capacity {
            self.seen.pop_front();
        }
        self.seen.push_back(key);
        false
    }
}

/// Translated action from an incoming mobile Signal message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedSignalAction {
    /// An explicit or auto-forwarded decision resolving a pending REQUEST callout.
    Decision {
        approved: bool,
        request: String,
        formatted_message: String,
    },
    /// An allowlisted local launcher trigger (terminal, vscode, dashboard).
    Launch { target: String },
    /// A general user instruction or conversation prompt.
    Prompt { text: String },
}

/// Analyzes incoming text from an allowlisted sender and determines whether it resolves
/// a pending in-chat REQUEST or should be dispatched as a regular conversation prompt.
pub fn resolve_incoming_message(
    incoming_text: &str,
    pending_requests: &[String],
    auto_forward: bool,
) -> ResolvedSignalAction {
    let trimmed = incoming_text.trim();

    // Direct explicit answers must still quote a pending request; a phone message cannot invent
    // or resolve a stale request.
    if let Some(rest) = trimmed.strip_prefix("Approved:") {
        let req = rest.trim();
        if let Some(pending) = pending_requests.iter().find(|pending| pending.trim() == req) {
            return ResolvedSignalAction::Decision {
                approved: true,
                request: pending.clone(),
                formatted_message: format!("Approved: {pending}"),
            };
        }
    }
    if let Some(rest) = trimmed.strip_prefix("Denied:") {
        let req = rest.trim();
        if let Some(pending) = pending_requests.iter().find(|pending| pending.trim() == req) {
            return ResolvedSignalAction::Decision {
                approved: false,
                request: pending.clone(),
                formatted_message: format!("Denied: {pending}"),
            };
        }
    }

    // Case-insensitive short response keywords
    let lower = trimmed.to_ascii_lowercase();
    let is_approve = matches!(
        lower.as_str(),
        "approve" | "approved" | "yes" | "y" | "lgtm" | "ok" | "proceed"
    );
    let is_deny = matches!(
        lower.as_str(),
        "deny" | "denied" | "no" | "n" | "reject" | "cancel" | "stop"
    );

    if auto_forward && (is_approve || is_deny) {
        // A short response is safe only when it is unambiguous. With several pending requests it
        // remains a normal prompt, allowing the council to ask which request the human meant.
        if let [only] = pending_requests {
            let approved = is_approve;
            let formatted_message = if approved {
                format!("Approved: {only}")
            } else {
                format!("Denied: {only}")
            };
            return ResolvedSignalAction::Decision {
                approved,
                request: only.clone(),
                formatted_message,
            };
        }
    }

    // Check for launcher triggers: "action:launch:<target>" or "launch <target>"
    if let Some(target) = trimmed.strip_prefix("action:launch:") {
        let t = target.trim().to_ascii_lowercase();
        if is_launch_target_allowed(&t) {
            return ResolvedSignalAction::Launch { target: t };
        }
    }
    if let Some(target) = trimmed.strip_prefix("launch ").or_else(|| trimmed.strip_prefix("Launch ")) {
        let t = target.trim().to_ascii_lowercase();
        if is_launch_target_allowed(&t) {
            return ResolvedSignalAction::Launch { target: t };
        }
    }

    ResolvedSignalAction::Prompt {
        text: trimmed.to_string(),
    }
}

/// Checks if a requested program launcher target is strictly in the local allowlist.
pub fn is_launch_target_allowed(target: &str) -> bool {
    matches!(target.trim().to_ascii_lowercase().as_str(), "terminal" | "vscode" | "code" | "dashboard")
}

/// Executes an allowlisted local developer tool or opens the dashboard directly without a shell.
pub fn execute_launch_action(target: &str) -> Result<String, String> {
    let t = target.trim().to_ascii_lowercase();
    match t.as_str() {
        "terminal" => {
            #[cfg(target_os = "macos")]
            {
                std::process::Command::new("open")
                    .arg("-a")
                    .arg("Terminal")
                    .spawn()
                    .map_err(|e| format!("failed to launch Terminal: {e}"))?;
                Ok("Launched Terminal".into())
            }
            #[cfg(target_os = "windows")]
            {
                std::process::Command::new("wt.exe")
                    .spawn()
                    .map_err(|e| format!("failed to launch Windows Terminal: {e}"))?;
                Ok("Launched Windows Terminal".into())
            }
            #[cfg(all(unix, not(target_os = "macos")))]
            {
                std::process::Command::new("x-terminal-emulator")
                    .spawn()
                    .map_err(|e| format!("failed to launch terminal: {e}"))?;
                Ok("Launched Terminal".into())
            }
        }
        "vscode" | "code" => {
            std::process::Command::new("code")
                .spawn()
                .map_err(|e| format!("failed to launch VS Code: {e}"))?;
            Ok("Launched VS Code".into())
        }
        "dashboard" => {
            #[cfg(target_os = "macos")]
            {
                std::process::Command::new("open")
                    .arg("http://localhost:1420")
                    .spawn()
                    .map_err(|e| format!("failed to open dashboard: {e}"))?;
                Ok("Opened dashboard".into())
            }
            #[cfg(target_os = "windows")]
            {
                std::process::Command::new("cmd")
                    .args(["/C", "start", "http://localhost:1420"])
                    .spawn()
                    .map_err(|e| format!("failed to open dashboard: {e}"))?;
                Ok("Opened dashboard".into())
            }
            #[cfg(all(unix, not(target_os = "macos")))]
            {
                std::process::Command::new("xdg-open")
                    .arg("http://localhost:1420")
                    .spawn()
                    .map_err(|e| format!("failed to open dashboard: {e}"))?;
                Ok("Opened dashboard".into())
            }
        }
        _ => Err(format!("target '{target}' is not in the launch allowlist (allowed: terminal, vscode, dashboard)")),
    }
}

/// Formats an outbound council update or REQUEST notification for mobile delivery,
/// ensuring all secrets, keys, and tokens are scrubbed with `redact::scrub`.
pub fn format_outbound_notification(sender: &str, text: &str) -> String {
    let clean = crate::redact::scrub(text);
    if clean.lines().any(|l| l.trim_start().to_ascii_lowercase().starts_with("request:")) {
        format!("⚠️ [Consilium REQUEST from {sender}]\n{clean}")
    } else {
        format!("[Consilium - {sender}]\n{clean}")
    }
}

/// Returns the standard path to `messaging.json` inside the application config directory.
pub fn config_path(base_dir: Option<&Path>) -> PathBuf {
    if let Some(dir) = base_dir {
        dir.join("messaging.json")
    } else {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("consilium")
            .join("messaging.json")
    }
}

/// Live state of one third-party messaging connection, shown as a check or a cross in the top bar.
/// `detail` never contains the linked phone number or account identifiers.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ConnectorStatus {
    pub id: &'static str,
    pub name: &'static str,
    /// "off" (not set up or switched off), "ok" (answered just now) or "down" (set up but not answering).
    pub state: &'static str,
    pub detail: String,
}

/// What the messaging settings panel shows. The phone number is only ever returned masked.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SetupView {
    pub enabled: bool,
    pub endpoint: String,
    /// e.g. "•••• 4821", or "" when no number is saved.
    pub account_hint: String,
    pub allowed_acis: Vec<String>,
    pub auto_forward_decisions: bool,
    /// Where signal-cli was found, if it's installed.
    pub signal_cli: Option<String>,
    /// Whether the one-click install is offered (macOS).
    pub can_install: bool,
    /// True while the background install (no Homebrew) is running.
    pub install_running: bool,
    /// Why the last background install stopped, if it failed.
    pub install_error: Option<String>,
    pub config_path: String,
}

/// Where signal-cli is installed, if it is.
pub fn find_signal_cli() -> Option<PathBuf> {
    find_tool("signal-cli")
}

fn find_tool(name: &str) -> Option<PathBuf> {
    let home = dirs::home_dir().unwrap_or_default();
    let mut dirs_: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    dirs_.extend([".local/bin", ".local/node/bin", ".cargo/bin"].iter().map(|d| home.join(d)));
    dirs_.extend(["/opt/homebrew/bin", "/usr/local/bin"].iter().map(PathBuf::from));
    // Windows tools are `signal-cli.bat`, `code.cmd`, `git.exe`: try those names too.
    let names: Vec<String> = if cfg!(windows) {
        ["exe", "bat", "cmd"].iter().map(|e| format!("{name}.{e}")).chain([name.to_string()]).collect()
    } else {
        vec![name.to_string()]
    };
    dirs_.into_iter().flat_map(|d| names.iter().map(move |n| d.join(n))).find(|p| p.is_file())
}

pub fn view(cfg: &BridgeConfig, path: &Path) -> SetupView {
    let digits: String = cfg.account.chars().filter(|c| c.is_ascii_digit()).collect();
    let account_hint = if digits.len() >= 4 { format!("•••• {}", &digits[digits.len() - 4..]) } else { String::new() };
    SetupView {
        enabled: cfg.enabled,
        endpoint: cfg.endpoint.clone(),
        account_hint,
        allowed_acis: cfg.allowed_acis.clone(),
        auto_forward_decisions: cfg.auto_forward_decisions,
        signal_cli: find_tool("signal-cli").map(|p| p.display().to_string()),
        can_install: cfg!(any(target_os = "macos", target_os = "linux")),
        install_running: install_root().join("installing").exists(),
        install_error: std::fs::read_to_string(install_root().join("failed")).ok().map(|e| e.trim().to_string()),
        config_path: path.display().to_string(),
    }
}

/// A phone number in international form: "+" then 8 to 15 digits, nothing else.
pub fn valid_account(s: &str) -> bool {
    let d = s.strip_prefix('+').unwrap_or("");
    (8..=15).contains(&d.len()) && d.chars().all(|c| c.is_ascii_digit())
}

/// Changes from the settings panel. `account: None` keeps the saved number.
#[derive(Debug, Clone, Deserialize)]
pub struct SetupChange {
    pub enabled: bool,
    pub account: Option<String>,
    pub endpoint: String,
    pub allowed_acis: Vec<String>,
    pub auto_forward_decisions: bool,
}

/// Validates and applies a settings change. Nothing is saved if any field is wrong.
pub fn apply_change(cfg: &BridgeConfig, ch: SetupChange) -> Result<BridgeConfig, String> {
    let mut next = cfg.clone();
    if let Some(a) = ch.account {
        let a: String = a.chars().filter(|c| !c.is_whitespace() && *c != '-' && *c != '(' && *c != ')').collect();
        if !a.is_empty() && !valid_account(&a) {
            return Err("Enter the phone number in international form, like +15551234567".into());
        }
        next.account = a;
    }
    let endpoint = ch.endpoint.trim();
    next.endpoint = if endpoint.is_empty() { DEFAULT_ENDPOINT.into() } else { endpoint.into() };
    validate_loopback(&next.endpoint)?;
    let mut acis = Vec::new();
    for a in ch.allowed_acis.iter().map(|a| a.trim()).filter(|a| !a.is_empty()) {
        let id = uuid::Uuid::parse_str(a).map_err(|_| format!("\"{a}\" isn't a Signal account ID (a UUID)"))?;
        acis.push(id.to_string());
    }
    next.allowed_acis = acis;
    next.auto_forward_decisions = ch.auto_forward_decisions;
    if ch.enabled && next.account.is_empty() {
        return Err("Add the phone number linked to Signal before turning it on".into());
    }
    next.enabled = ch.enabled;
    Ok(next)
}

/// A fixed setup command for the terminal. Nothing typed in the UI goes into it except a phone
/// number that passed `valid_account` and a loopback address that passed `validate_loopback`.
pub fn setup_command(action: &str, cfg: &BridgeConfig) -> Result<(&'static str, String), String> {
    // The step runs in a POSIX shell on macOS and Linux and in PowerShell on Windows (see
    // `setup::open_in_terminal`); PowerShell needs `&` to run a quoted program path.
    let quote = |p: &Path| {
        if cfg!(windows) {
            format!("& '{}'", p.display().to_string().replace('\'', "''"))
        } else {
            format!("'{}'", p.display().to_string().replace('\'', r"'\''"))
        }
    };
    match action {
        "install" => {
            if !cfg!(target_os = "macos") {
                return Err("One-click install is for macOS. Otherwise install signal-cli from its GitHub releases page".into());
            }
            let brew = find_tool("brew").ok_or("Homebrew isn't installed; use the background install")?;
            Ok(("Install signal-cli and qrencode", format!("{} install signal-cli qrencode", quote(&brew))))
        }
        "link" => {
            let cli = find_tool("signal-cli").ok_or("Install signal-cli first")?;
            let host = std::env::var("HOSTNAME")
                .or_else(|_| std::env::var("COMPUTERNAME"))
                .unwrap_or_else(|_| {
                    if cfg!(target_os = "macos") {
                        "Mac".into()
                    } else if cfg!(windows) {
                        "Windows".into()
                    } else {
                        "Linux".into()
                    }
                });
            let device_name = format!("Consilium ({host})");
            let qr = find_tool("qrencode").filter(|_| !cfg!(windows)); // PowerShell has no `>(…)`
            let node = find_tool("node");
            let home = dirs::home_dir().unwrap_or_default();
            let qr_script = std::env::current_dir()
                .ok()
                .map(|p| p.join("scripts/qr.mjs"))
                .filter(|p| p.is_file())
                .or_else(|| {
                    let p = home.join("Projects/consilium-ai/scripts/qr.mjs");
                    if p.is_file() {
                        Some(p)
                    } else {
                        None
                    }
                });

            let cmd = match qr {
                Some(q) => format!(
                    "echo 'On your phone: Signal > Settings > Linked devices > Link new device, then scan this code.'; {} link -n '{}' | tee >(xargs -L 1 {} -t ansiutf8)",
                    quote(&cli),
                    device_name,
                    quote(&q)
                ),
                None => match (node, qr_script) {
                    (Some(n), Some(s)) => format!(
                        "echo 'On your phone: Signal > Settings > Linked devices > Link new device, then scan this code.'; {} link -n '{}' | {} {}",
                        quote(&cli),
                        device_name,
                        quote(&n),
                        quote(&s)
                    ),
                    _ => format!(
                        "echo 'Open the sgnl:// link below on your phone, or install qrencode to see a QR code.'; {} link -n '{}'",
                        quote(&cli),
                        device_name
                    ),
                },
            };
            Ok(("Link Consilium to your Signal account", cmd))
        }
        "start" => {
            let cli = find_tool("signal-cli").ok_or("Install signal-cli first")?;
            if !valid_account(&cfg.account) {
                return Err("Save the phone number linked to Signal first".into());
            }
            validate_loopback(&cfg.endpoint)?;
            let raw = cfg.endpoint.trim().trim_start_matches("tcp://").trim_start_matches("http://");
            let addr: SocketAddr = raw
                .parse()
                .map_err(|_| "One-click start needs an address like 127.0.0.1:7583".to_string())?;
            Ok((
                "Start Signal for Consilium (keep this window open)",
                format!("{} -a '{}' daemon --tcp {addr}", quote(&cli), cfg.account),
            ))
        }
        _ => Err("unknown messaging setup step".into()),
    }
}

/// Installs pinned, checksum-verified Java and signal-cli into the user's folder: no Homebrew,
/// no admin password, no Terminal. See `signal_install.sh`.
const SIGNAL_INSTALL_SCRIPT: &str = include_str!("signal_install.sh");

fn install_root() -> PathBuf {
    dirs::home_dir().unwrap_or_default().join(".local/share/consilium-signal")
}

/// One-click install. With Homebrew it opens Terminal as before; without it, it runs the
/// password-free install in the background and the panel follows `install_running`.
pub fn start_install() -> Result<Option<&'static str>, String> {
    if !cfg!(any(target_os = "macos", target_os = "linux")) {
        return Err("One-click install is for macOS and Linux. On Windows, install Java 21 or newer and signal-cli from its GitHub releases page".into());
    }
    if cfg!(target_os = "macos") && find_tool("brew").is_some() {
        return Ok(None); // caller opens Terminal with `setup_command("install")`
    }
    if install_root().join("installing").exists() {
        return Ok(Some("signal-cli is already installing"));
    }
    std::process::Command::new("/bin/sh")
        .args(["-c", SIGNAL_INSTALL_SCRIPT])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("couldn't start the install: {e}"))?;
    Ok(Some("Installing Java and signal-cli in the background (about 250 MB, a few minutes)"))
}

const PROBE_ID: &str = "consilium-probe";

/// Asks the local signal-cli daemon for its version over JSON-RPC. A reply proves the daemon is
/// running and reachable; anything else, or a non-loopback endpoint, reports "down".
pub fn probe_signal(cfg: &BridgeConfig, timeout: std::time::Duration) -> ConnectorStatus {
    let status = |state, detail: String| ConnectorStatus { id: "signal", name: "Signal", state, detail };
    if !cfg.enabled {
        return status("off", "Signal isn't set up. Click to set it up.".into());
    }
    if let Err(e) = cfg.validate_endpoint() {
        return status("down", e);
    }
    match ask_version(cfg.endpoint.trim(), timeout) {
        Ok(v) => status("ok", format!("Connected to signal-cli {v} on this computer")),
        Err(e) => status("down", format!("signal-cli isn't answering: {e}")),
    }
}

fn ask_version(endpoint: &str, timeout: std::time::Duration) -> Result<String, String> {
    #[cfg(unix)]
    if let Some(path) = endpoint.strip_prefix("unix:").or(endpoint.starts_with('/').then_some(endpoint)) {
        let s = std::os::unix::net::UnixStream::connect(path).map_err(|e| e.to_string())?;
        s.set_read_timeout(Some(timeout)).map_err(|e| e.to_string())?;
        s.set_write_timeout(Some(timeout)).map_err(|e| e.to_string())?;
        return exchange(s);
    }
    let raw = endpoint
        .strip_prefix("tcp://")
        .or_else(|| endpoint.strip_prefix("http://"))
        .unwrap_or(endpoint);
    use std::net::ToSocketAddrs;
    let addrs: Vec<SocketAddr> = raw.to_socket_addrs().map_err(|e| e.to_string())?.collect();
    // validate_loopback accepted the name; re-check what it resolved to before connecting.
    let addr = addrs
        .into_iter()
        .find(|a| a.ip().is_loopback())
        .ok_or("endpoint doesn't resolve to this computer")?;
    let s = std::net::TcpStream::connect_timeout(&addr, timeout).map_err(|e| e.to_string())?;
    s.set_read_timeout(Some(timeout)).map_err(|e| e.to_string())?;
    s.set_write_timeout(Some(timeout)).map_err(|e| e.to_string())?;
    exchange(s)
}

/// Sends one version request and reads lines until its answer, skipping any messages the
/// daemon pushes in between. Bounded so a chatty or broken daemon can't hold the check.
fn exchange<S: std::io::Read + std::io::Write>(mut s: S) -> Result<String, String> {
    use std::io::BufRead;
    let req = format!("{{\"jsonrpc\":\"2.0\",\"method\":\"version\",\"id\":\"{PROBE_ID}\"}}\n");
    s.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
    s.flush().map_err(|e| e.to_string())?;
    let mut reader = std::io::BufReader::new(std::io::Read::take(s, 256 * 1024));
    let mut line = String::new();
    for _ in 0..50 {
        line.clear();
        if reader.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
            return Err("connection closed".into());
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
        if v.get("id").and_then(|i| i.as_str()) != Some(PROBE_ID) {
            continue;
        }
        if let Some(r) = v.get("result") {
            let ver = r.get("version").and_then(|x| x.as_str()).unwrap_or("");
            let ver: String = ver.chars().filter(|c| c.is_ascii_alphanumeric() || ".-+".contains(*c)).take(32).collect();
            return Ok(if ver.is_empty() { "(version unknown)".into() } else { ver });
        }
        return Err("it returned an error".into());
    }
    Err("no answer to the version request".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled_at(endpoint: String) -> BridgeConfig {
        BridgeConfig { enabled: true, endpoint, ..BridgeConfig::default() }
    }

    #[test]
    fn probe_reports_off_when_not_set_up() {
        let st = probe_signal(&BridgeConfig::default(), std::time::Duration::from_millis(200));
        assert_eq!(st.state, "off");
    }

    #[test]
    fn probe_refuses_a_remote_endpoint_without_connecting() {
        let st = probe_signal(&enabled_at("203.0.113.9:7583".into()), std::time::Duration::from_millis(200));
        assert_eq!(st.state, "down");
    }

    #[test]
    fn probe_is_ok_when_the_daemon_answers_after_pushed_messages() {
        use std::io::{BufRead, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let t = std::thread::spawn(move || {
            let (mut c, _) = l.accept().unwrap();
            let mut line = String::new();
            std::io::BufReader::new(c.try_clone().unwrap()).read_line(&mut line).unwrap();
            assert!(line.contains("\"version\""));
            c.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"receive\",\"params\":{}}\n").unwrap();
            c.write_all(b"{\"jsonrpc\":\"2.0\",\"result\":{\"version\":\"0.13.9\"},\"id\":\"consilium-probe\"}\n").unwrap();
        });
        let st = probe_signal(&enabled_at(addr.to_string()), std::time::Duration::from_secs(2));
        t.join().unwrap();
        assert_eq!(st.state, "ok", "{}", st.detail);
        assert!(st.detail.contains("0.13.9"));
    }

    #[test]
    fn probe_is_down_when_nothing_listens_or_the_daemon_hangs_up() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let t = std::thread::spawn(move || drop(l.accept().unwrap()));
        let st = probe_signal(&enabled_at(addr.to_string()), std::time::Duration::from_secs(2));
        t.join().unwrap();
        assert_eq!(st.state, "down");
        let st = probe_signal(&enabled_at(addr.to_string()), std::time::Duration::from_millis(300));
        assert_eq!(st.state, "down");
    }

    #[test]
    fn loopback_validator_accepts_valid_endpoints() {
        assert!(validate_loopback("127.0.0.1:7583").is_ok());
        assert!(validate_loopback("localhost:7583").is_ok());
        assert!(validate_loopback("[::1]:7583").is_ok());
        assert!(validate_loopback("127.0.0.1").is_ok());
        assert!(validate_loopback("unix:/tmp/signal.sock").is_ok());
        assert!(validate_loopback("/var/run/signal.sock").is_ok());
    }

    #[test]
    fn loopback_validator_rejects_remote_hosts() {
        assert!(validate_loopback("192.168.1.50:7583").is_err());
        assert!(validate_loopback("api.signal.org:443").is_err());
        assert!(validate_loopback("evil.com:7583").is_err());
        assert!(validate_loopback("8.8.8.8:53").is_err());
        assert!(validate_loopback("").is_err());
    }

    #[test]
    fn allowlist_matching_is_case_insensitive() {
        let cfg = BridgeConfig {
            allowed_acis: vec!["a1b2c3d4-e5f6-7890-abcd-ef1234567890".into()],
            ..Default::default()
        };
        assert!(cfg.is_sender_allowed("A1B2C3D4-E5F6-7890-ABCD-EF1234567890"));
        assert!(cfg.is_sender_allowed("  a1b2c3d4-e5f6-7890-abcd-ef1234567890  "));
        assert!(!cfg.is_sender_allowed("00000000-0000-0000-0000-000000000000"));
        assert!(!cfg.is_sender_allowed(""));
    }

    #[test]
    fn deduplicator_catches_replays() {
        let mut dedup = MessageDeduplicator::new(50);
        let sender = "00da9915-cb9d-43e5-b1a4-2e656a7ffec8";
        assert!(!dedup.is_duplicate(sender, 1, 1000));
        assert!(dedup.is_duplicate(sender, 1, 1000)); // second time duplicate
        assert!(!dedup.is_duplicate(sender, 1, 1001)); // new timestamp ok
        assert!(!dedup.is_duplicate(sender, 2, 1000)); // new device ok
    }

    #[test]
    fn translates_short_approvals_into_decisions() {
        let pending = vec!["Should we enable the live voice visualizer?".into()];

        // "Approve" -> Approved: Should we enable the live voice visualizer?
        let res = resolve_incoming_message("Approve", &pending, true);
        assert_eq!(
            res,
            ResolvedSignalAction::Decision {
                approved: true,
                request: "Should we enable the live voice visualizer?".into(),
                formatted_message: "Approved: Should we enable the live voice visualizer?".into(),
            }
        );

        // "Deny" -> Denied: Should we enable the live voice visualizer?
        let res = resolve_incoming_message("deny", &pending, true);
        assert_eq!(
            res,
            ResolvedSignalAction::Decision {
                approved: false,
                request: "Should we enable the live voice visualizer?".into(),
                formatted_message: "Denied: Should we enable the live voice visualizer?".into(),
            }
        );
    }

    #[test]
    fn short_decisions_require_exactly_one_pending_request() {
        let several = vec!["First request?".into(), "Second request?".into()];
        assert_eq!(
            resolve_incoming_message("Approve", &several, true),
            ResolvedSignalAction::Prompt { text: "Approve".into() }
        );
        assert_eq!(
            resolve_incoming_message("Deny", &[], true),
            ResolvedSignalAction::Prompt { text: "Deny".into() }
        );
    }

    #[test]
    fn explicit_decisions_must_quote_a_request_that_is_still_pending() {
        let pending = vec!["Current request?".into()];
        assert_eq!(
            resolve_incoming_message("Approved: Old request?", &pending, true),
            ResolvedSignalAction::Prompt { text: "Approved: Old request?".into() }
        );
        assert_eq!(
            resolve_incoming_message("Denied: Current request?", &pending, true),
            ResolvedSignalAction::Decision {
                approved: false,
                request: "Current request?".into(),
                formatted_message: "Denied: Current request?".into(),
            }
        );
    }

    #[test]
    fn normal_text_remains_prompt() {
        let pending = vec!["Some pending request?".into()];
        let res = resolve_incoming_message("Hey council, run tests again", &pending, true);
        assert_eq!(
            res,
            ResolvedSignalAction::Prompt {
                text: "Hey council, run tests again".into(),
            }
        );
    }

    #[test]
    fn outbound_notifications_are_secret_scrubbed() {
        let fake_key = format!("{}{}", "sk-ant-", "1234567890abcdef1234567890");
        let raw = format!("Here is the key: {fake_key}\nREQUEST: Proceed?");
        let formatted = format_outbound_notification("Claude", &raw);
        assert!(!formatted.contains("sk-ant-"));
        assert!(formatted.contains("[REDACTED Anthropic key]"));
        assert!(formatted.contains("⚠️ [Consilium REQUEST from Claude]"));
    }

    #[test]
    fn launcher_recognizes_allowed_targets() {
        assert!(is_launch_target_allowed("terminal"));
        assert!(is_launch_target_allowed("vscode"));
        assert!(is_launch_target_allowed("code"));
        assert!(is_launch_target_allowed("dashboard"));
        assert!(!is_launch_target_allowed("rm -rf /"));
        assert!(!is_launch_target_allowed("sh"));
    }

    #[test]
    fn resolve_incoming_message_parses_launch_action() {
        let pending = vec![];
        let res = resolve_incoming_message("action:launch:terminal", &pending, true);
        assert_eq!(res, ResolvedSignalAction::Launch { target: "terminal".into() });

        let res2 = resolve_incoming_message("launch vscode", &pending, true);
        assert_eq!(res2, ResolvedSignalAction::Launch { target: "vscode".into() });

        let res3 = resolve_incoming_message("launch evil_script", &pending, true);
        assert_eq!(res3, ResolvedSignalAction::Prompt { text: "launch evil_script".into() });
    }
}

#[cfg(test)]
mod setup_tests {
    use super::*;

    fn change(account: Option<&str>, enabled: bool) -> SetupChange {
        SetupChange {
            enabled,
            account: account.map(String::from),
            endpoint: String::new(),
            allowed_acis: vec![],
            auto_forward_decisions: true,
        }
    }

    #[test]
    fn saves_valid_settings_and_masks_the_number() {
        let next = apply_change(&BridgeConfig::default(), change(Some("+1 (555) 123-4821"), true)).unwrap();
        assert_eq!(next.account, "+15551234821");
        assert_eq!(next.endpoint, DEFAULT_ENDPOINT);
        let v = view(&next, Path::new("/x/messaging.json"));
        assert_eq!(v.account_hint, "•••• 4821");
        assert!(!format!("{v:?}").contains("5551234821"));
        // None keeps the saved number.
        assert_eq!(apply_change(&next, change(None, true)).unwrap().account, "+15551234821");
    }

    #[test]
    fn rejects_bad_input() {
        let d = BridgeConfig::default();
        assert!(apply_change(&d, change(Some("5551234"), false)).is_err());
        assert!(apply_change(&d, change(Some("+1555; rm -rf ~"), false)).is_err());
        assert!(apply_change(&d, change(None, true)).is_err(), "can't turn on without a number");
        let mut c = change(Some("+15551234821"), true);
        c.endpoint = "203.0.113.5:7583".into();
        assert!(apply_change(&d, c).is_err(), "remote endpoints are refused");
        let mut c = change(Some("+15551234821"), true);
        c.allowed_acis = vec!["not-a-uuid".into()];
        assert!(apply_change(&d, c).is_err());
    }

    #[test]
    fn start_command_uses_only_validated_values() {
        let mut cfg = BridgeConfig::default();
        assert!(setup_command("start", &cfg).is_err() || find_tool("signal-cli").is_none());
        cfg.account = "+15551234821".into();
        if let Ok((_, cmd)) = setup_command("start", &cfg) {
            assert!(cmd.ends_with("-a '+15551234821' daemon --tcp 127.0.0.1:7583"));
        }
        assert!(setup_command("rm", &cfg).is_err());
    }

    #[test]
    fn background_install_is_pinned_and_checksummed() {
        let s = SIGNAL_INSTALL_SCRIPT;
        assert!(s.starts_with("#!/bin/sh") && s.contains("set -eu"));
        assert!(s.contains("shasum -a 256") && s.contains("didn't match its expected checksum"));
        assert!(!s.contains("sudo") && !s.contains("| sh") && !s.contains("| bash"));
        assert!(s.contains("SIGNAL_SHA=") && s.matches("JRE_SHA=").count() == 4 && s.contains("sha256sum"));
    }

}
