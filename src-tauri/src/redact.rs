//! Masks secrets before they reach the shared transcript or the council memory.
//!
//! Everything written to a conversation is sent to every AI in the council, and so to every
//! provider behind them. An AI that prints a `.env` file, or a person who pastes a token, would
//! otherwise share that secret with all of them and keep it in the conversation log for good.
//! The patterns match the ones `scripts/check-secrets.mjs` blocks from commits.

use regex::{Captures, Regex};
use std::sync::OnceLock;

struct Rule {
    label: &'static str,
    re: Regex,
}

fn rules() -> &'static [Rule] {
    static RULES: OnceLock<Vec<Rule>> = OnceLock::new();
    RULES.get_or_init(|| {
    let r = |label, pat: &str| Rule { label, re: Regex::new(pat).expect("valid secret pattern") };
    vec![
        r("private key", r"-----BEGIN (?:[A-Z]+ )*PRIVATE KEY-----[\s\S]*?(?:-----END (?:[A-Z]+ )*PRIVATE KEY-----|\z)"),
        r("Anthropic key", r"\bsk-ant-[A-Za-z0-9_-]{20,}"),
        r("OpenAI key", r"\bsk-(?:proj-|svcacct-|admin-)?[A-Za-z0-9_-]{20,}"),
        r("GitHub token", r"\b(?:gh[pousr]_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{40,})"),
        r("AWS key", r"\b(?:AKIA|ASIA)[A-Z0-9]{16}\b"),
        r("Google key", r"\bAIza[0-9A-Za-z_-]{35}"),
        r("Slack token", r"\bxox[abprs]-[A-Za-z0-9-]{10,}"),
        r("Stripe key", r"\b(?:sk|rk)_live_[A-Za-z0-9]{20,}"),
        r("JWT", r"\beyJ[A-Za-z0-9_-]{10,}\.eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}"),
        r("bearer token", r"\b(?i:bearer)\s+[A-Za-z0-9._~+/-]{30,}=*"),
    ]
    })
}

/// `NAME=value` or `name: "value"` where the name says it holds a secret. The name is kept so the
/// AIs can still see which setting it is; only the value is masked.
fn assignment() -> &'static Regex {
    static ASSIGNMENT: OnceLock<Regex> = OnceLock::new();
    ASSIGNMENT.get_or_init(|| { Regex::new(
        r#"(?i)\b([A-Z0-9_.-]*(?:password|passwd|secret|api[_-]?key|access[_-]?token|auth[_-]?token|private[_-]?key|client[_-]?secret)[A-Z0-9_.-]*["']?\s*[:=]\s*)(["']?)([^\s"'`,;]{8,})"#,
    )
    .expect("valid assignment pattern") })
}

fn mask(label: &str) -> String {
    format!("[REDACTED {label}]")
}

/// Values that are clearly not real secrets: references, placeholders and already-masked text.
fn is_placeholder(v: &str) -> bool {
    let l = v.to_ascii_lowercase();
    v.starts_with('$')
        || v.starts_with('<')
        || v.starts_with('{')
        || v.starts_with('[')
        || v.starts_with('*')
        || l.starts_with("process.env")
        || l.starts_with("env::")
        || l.starts_with("os.environ")
        || l.contains("example")
        || l.contains("placeholder")
        || l.contains("your")
        || l.contains("xxxx")
        || l.contains("redacted")
        || l.contains("changeme")
}

/// Returns `text` with secrets masked as `[REDACTED <kind>]`.
pub fn scrub(text: &str) -> String {
    let mut out = text.to_string();
    for rule in rules() {
        if rule.re.is_match(&out) {
            out = rule
                .re
                .replace_all(&out, mask(rule.label).as_str())
                .into_owned();
        }
    }
    let assignment = assignment();
    if assignment.is_match(&out) {
        out = assignment
            .replace_all(&out, |c: &Captures| {
                let value = &c[3];
                if is_placeholder(value) {
                    c[0].to_string()
                } else {
                    format!("{}{}{}", &c[1], &c[2], mask("value"))
                }
            })
            .into_owned();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::scrub;

    // Fake secrets are assembled at run time so the commit scanner doesn't flag this file.
    fn fake(prefix: &str, body: &str, n: usize) -> String {
        format!("{prefix}{}", body.repeat(n))
    }

    #[test]
    fn masks_provider_keys_and_tokens() {
        let cases = [
            (fake("sk-ant-", "aB3d", 10), "Anthropic key"),
            (fake("sk-proj-", "Zx9_", 10), "OpenAI key"),
            (fake("ghp_", "A1b2", 10), "GitHub token"),
            (fake("AKIA", "ABCD", 4), "AWS key"),
            (fake("AIza", "Sy0-a", 7), "Google key"),
            (fake("xoxb-", "123-ab", 3), "Slack token"),
            (
                format!(
                    "{}.{}.{}",
                    fake("eyJ", "hbGciOi", 2),
                    fake("eyJ", "zdWIiOi", 2),
                    "s1gnatureXYZabc"
                ),
                "JWT",
            ),
        ];
        for (secret, label) in cases {
            let out = scrub(&format!("token is {secret} ok"));
            assert_eq!(
                out,
                format!("token is [REDACTED {label}] ok"),
                "for {label}"
            );
        }
    }

    #[test]
    fn masks_a_whole_private_key_block() {
        let begin = format!("-----BEGIN {}-----", "OPENSSH PRIVATE KEY");
        let end = format!("-----END {}-----", "OPENSSH PRIVATE KEY");
        let text = format!("here:\n{begin}\nb3BlbnNzaC1rZXktdjEAAAA\nQQQQ\n{end}\ndone");
        assert_eq!(scrub(&text), "here:\n[REDACTED private key]\ndone");
    }

    #[test]
    fn masks_secret_assignments_but_keeps_the_name() {
        let text = format!(
            "DATABASE_PASSWORD={}\nexport STRIPE_SECRET=\"{}\"",
            "hunter2hunter2", "q8w7e6r5t4y3"
        );
        assert_eq!(
            scrub(&text),
            "DATABASE_PASSWORD=[REDACTED value]\nexport STRIPE_SECRET=\"[REDACTED value]\""
        );
        let json = format!("{{\"api_key\": \"{}\"}}", "0123456789abcdef");
        assert_eq!(scrub(&json), "{\"api_key\": \"[REDACTED value]\"}");
    }

    #[test]
    fn leaves_placeholders_and_ordinary_text_alone() {
        for text in [
            "API_KEY=$OPENAI_API_KEY",
            "password: <your-password>",
            "SECRET=${SECRET}",
            "api_key = process.env.KEY_NAME",
            "Run `git log` and commit 54cb587 to fix the password check.",
            "uuid 00da9915-cb9d-43e5-b1a4-2e656a7ffec8 and sk-short",
            "Use Bearer tokens for auth.",
        ] {
            assert_eq!(scrub(text), text);
        }
    }

    #[test]
    fn scrubbing_twice_changes_nothing_more() {
        let once = scrub(&format!(
            "KEY {} and TOKEN={}",
            fake("ghp_", "A1b2", 10),
            "abcdefgh12345"
        ));
        assert_eq!(scrub(&once), once);
    }
}
