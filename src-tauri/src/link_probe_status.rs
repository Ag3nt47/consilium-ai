//! Diagnostic text only; transport selection and authentication behavior stay in Link.

use regex::Regex;
use std::sync::OnceLock;

const MAX_DETAIL_CHARS: usize = 180;

fn native_detail(error: &str) -> String {
    static ENDPOINTS: OnceLock<Regex> = OnceLock::new();
    let endpoints = ENDPOINTS.get_or_init(|| Regex::new(
        r"(?i)\b[a-z][a-z0-9+.-]*://\S+|\b(?:[0-9a-f]{2}[:-]){5}[0-9a-f]{2}\b|\b[0-9a-f]{12}\b|\b(?:[0-9]{1,3}\.){3}[0-9]{1,3}(?::[0-9]{1,5})?\b|\[[0-9a-f:%]+\](?::[0-9]{1,5})?"
    ).expect("valid diagnostic endpoint pattern"));
    let scrubbed = crate::redact::scrub(error);
    let without_endpoints = endpoints.replace_all(&scrubbed, "[endpoint]");
    without_endpoints.split_whitespace().collect::<Vec<_>>().join(" ")
        .chars().filter(|c| !c.is_control()).take(MAX_DETAIL_CHARS).collect()
}

pub(super) fn connection_failure(native: bool, error: Option<&str>) -> String {
    match (native, error) {
        (false, Some(_)) => "Refused or unreachable".into(),
        (false, None) => "Timed out".into(),
        (true, None) => "Native Bluetooth connection timed out".into(),
        (true, Some(error)) => {
            let detail = native_detail(error);
            if detail.is_empty() { "Native Bluetooth connection failed".into() }
            else { format!("Native Bluetooth connection failed: {detail}") }
        }
    }
}

pub(super) fn authentication_failure(native: bool, transport: &str, connected_ms: u64, timed_out: bool) -> String {
    if !native {
        return format!("{transport} connected in {connected_ms} ms · peer authentication unconfirmed");
    }
    let reason = if timed_out { "timed out" } else { "failed" };
    // Peer-supplied authentication frames are not echoed into diagnostic text.
    format!("RFCOMM connected in {connected_ms} ms · PeerLink authentication {reason}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_driver_and_feature_failures_survive_while_tcp_stays_generic() {
        let driver = "Windows Bluetooth connection failed (10022).";
        assert!(connection_failure(true, Some(driver)).contains(driver));
        assert!(connection_failure(true, Some("Enable native Bluetooth in Consilium on both computers first.")).contains("Enable native Bluetooth"));
        assert_eq!(connection_failure(false, Some(driver)), "Refused or unreachable");
        assert_eq!(connection_failure(false, None), "Timed out");
        assert_eq!(connection_failure(true, None), "Native Bluetooth connection timed out");
    }

    #[test]
    fn reached_native_transport_is_distinct_from_peer_authentication() {
        assert_eq!(authentication_failure(true, "RFCOMM", 23, false), "RFCOMM connected in 23 ms · PeerLink authentication failed");
        assert_eq!(authentication_failure(true, "RFCOMM", 23, true), "RFCOMM connected in 23 ms · PeerLink authentication timed out");
        for timed_out in [false, true] {
            assert_eq!(authentication_failure(false, "TCP", 23, timed_out), "TCP connected in 23 ms · peer authentication unconfirmed");
        }
    }

    #[test]
    fn native_detail_is_bounded_and_hides_credentials_and_endpoints() {
        let secret = format!("{}{}", "sk-proj-", "Ab12".repeat(10));
        let error = format!("driver 10022\n{secret} rfcomm://001122AABBCC 00:11:22:AA:BB:CC 192.0.2.4:8470 https://example.invalid/private [fe80::1]:8470");
        let status = connection_failure(true, Some(&error));
        assert!(status.contains("10022"));
        assert!(status.contains("[REDACTED"));
        for private in [secret.as_str(), "001122AABBCC", "00:11:22:AA:BB:CC", "192.0.2.4", "example.invalid", "fe80::1"] {
            assert!(!status.contains(private));
        }
        assert!(!status.chars().any(char::is_control));
        assert!(connection_failure(true, Some(&"é".repeat(1000))).chars().count() <= MAX_DETAIL_CHARS + 40);
    }
}
