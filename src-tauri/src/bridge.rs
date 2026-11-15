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

    // Direct explicit answers: "Approved: <text>" or "Denied: <text>"
    if let Some(rest) = trimmed.strip_prefix("Approved:") {
        let req = rest.trim();
        return ResolvedSignalAction::Decision {
            approved: true,
            request: req.to_string(),
            formatted_message: format!("Approved: {req}"),
        };
    }
    if let Some(rest) = trimmed.strip_prefix("Denied:") {
        let req = rest.trim();
        return ResolvedSignalAction::Decision {
            approved: false,
            request: req.to_string(),
            formatted_message: format!("Denied: {req}"),
        };
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
        if let Some(latest) = pending_requests.last() {
            let approved = is_approve;
            let formatted_message = if approved {
                format!("Approved: {latest}")
            } else {
                format!("Denied: {latest}")
            };
            return ResolvedSignalAction::Decision {
                approved,
                request: latest.clone(),
                formatted_message,
            };
        }
    }

    ResolvedSignalAction::Prompt {
        text: trimmed.to_string(),
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
