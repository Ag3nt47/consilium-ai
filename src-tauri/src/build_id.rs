//! Which build of Consilium this is, and how two builds compare.
//!
//! A version number alone can't tell builds apart: two builds of 0.3.37 made from different
//! commits look the same. So each build also carries a build number (the commit count of its
//! history, stamped by `build.rs`, which only grows along a branch) and its commit. Linked
//! computers share this in every heartbeat (see `link`), an update is only ever installed over an
//! older build (see `update::staged`), and a computer behind the others prepares the newest one's
//! exact commit (see `follow`).

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct Build {
    pub version: String,
    /// Commits in the build's history; 0 when unknown (an older build, or no git at build time).
    #[serde(default)]
    pub number: u64,
    /// Short commit id, with "+dirty" if it had uncommitted changes; empty when unknown.
    #[serde(default)]
    pub commit: String,
}

fn parse(v: &str) -> Vec<u64> {
    v.split(['.', '-', '+']).map_while(|p| p.parse().ok()).collect()
}

impl Build {
    /// This running build.
    pub fn this() -> Build {
        Build {
            version: env!("CARGO_PKG_VERSION").to_string(),
            number: env!("CONSILIUM_BUILD_NUMBER").parse().unwrap_or(0),
            commit: env!("CONSILIUM_COMMIT").to_string(),
        }
    }

    /// Whether this is a later build than `other`: a higher version, or the same version with a
    /// higher build number. Equal or unknown is never newer, so nothing is replaced by its equal.
    pub fn newer_than(&self, other: &Build) -> bool {
        let (a, b) = (parse(&self.version), parse(&other.version));
        if a != b {
            return a > b;
        }
        self.number > other.number
    }

    /// Whether another computer can rebuild exactly this: a known, committed commit.
    pub fn reproducible(&self) -> bool {
        self.number > 0
            && !self.commit.is_empty()
            && !self.commit.ends_with("+dirty")
            && self.commit.chars().all(|c| c.is_ascii_hexdigit())
    }

    /// e.g. "0.3.37 #812 (76debf3a1b2c)", or just the version when the rest is unknown.
    pub fn label(&self) -> String {
        match (self.number, self.commit.as_str()) {
            (0, _) => self.version.clone(),
            (n, "") => format!("{} #{n}", self.version),
            (n, c) => format!("{} #{n} ({c})", self.version),
        }
    }

    /// A build as reported by another computer, kept only if it looks like one: short, clean
    /// fields, so a bad heartbeat can't put anything odd on screen or into a git command.
    pub fn from_peer(v: &serde_json::Value) -> Option<Build> {
        let b: Build = serde_json::from_value(v.clone()).ok()?;
        let version_ok = !b.version.is_empty()
            && b.version.len() <= 32
            && b.version.chars().all(|c| c.is_ascii_alphanumeric() || ".-+".contains(c));
        let commit_ok = b.commit.len() <= 48 && b.commit.chars().all(|c| c.is_ascii_hexdigit() || c == '+' || c.is_ascii_lowercase());
        (version_ok && commit_ok).then_some(b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(version: &str, number: u64, commit: &str) -> Build {
        Build { version: version.into(), number, commit: commit.into() }
    }

    #[test]
    fn two_builds_of_one_version_are_told_apart_and_never_go_backwards() {
        assert!(b("0.3.37", 812, "a").newer_than(&b("0.3.37", 790, "b")));
        assert!(!b("0.3.37", 790, "b").newer_than(&b("0.3.37", 812, "a")), "an older build never replaces a newer one");
        assert!(!b("0.3.37", 812, "a").newer_than(&b("0.3.37", 812, "a")), "nor its equal");
        assert!(b("0.3.38", 0, "").newer_than(&b("0.3.37", 900, "c")), "a higher version wins");
        assert!(!b("0.3.36", 999, "d").newer_than(&b("0.3.37", 1, "e")));
        assert!(b("0.3.37", 812, "a").newer_than(&b("0.3.37", 0, "")), "a numbered build beats an unnumbered one");
    }

    #[test]
    fn only_committed_known_builds_can_be_followed() {
        assert!(b("0.3.37", 812, "76debf3a1b2c").reproducible());
        assert!(!b("0.3.37", 812, "76debf3a1b2c+dirty").reproducible(), "uncommitted changes");
        assert!(!b("0.3.37", 0, "76debf3").reproducible());
        assert!(!b("0.3.37", 812, "").reproducible());
        assert!(!b("0.3.37", 812, "main; rm -rf /").reproducible());
    }

    #[test]
    fn a_peer_report_is_checked_before_use() {
        let ok = serde_json::json!({"version": "0.3.37", "number": 812, "commit": "76debf3a1b2c"});
        assert_eq!(Build::from_peer(&ok).unwrap().label(), "0.3.37 #812 (76debf3a1b2c)");
        assert!(Build::from_peer(&serde_json::json!({"version": "0.3.37\u{7}"})).is_none());
        assert!(Build::from_peer(&serde_json::json!({"version": "1", "commit": "x y"})).is_none());
        assert_eq!(Build::from_peer(&serde_json::json!({"version": "0.3.37"})).unwrap().label(), "0.3.37", "older computers send less");
        assert!(Build::this().version == env!("CARGO_PKG_VERSION"));
    }
}
