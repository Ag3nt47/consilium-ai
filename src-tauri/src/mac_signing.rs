//! The order of `security`/`codesign` steps that signs a staged macOS update with this Mac's
//! stable "Consilium Local Signing" identity, kept free of process spawning so the exact command
//! sequence can be tested on every platform.
//!
//! codesign only finds an identity in a keychain on the user's search list, even with
//! `--keychain`. The signing keychain is joined to that list only for the signing and taken off
//! again afterwards.
//!
//! Concurrency: one process-wide lock serialises join → sign → restore inside Consilium, so two
//! stagings in this app never interleave. The keychain search list itself is per user and macOS
//! offers no lock for it, so another process (`scripts/stage-update.mjs`, Keychain Access, another
//! tool) can still change it between our read and our write. To keep that window harmless the
//! restore never writes back the old snapshot: it re-reads the current list and removes only the
//! keychain this run added, so a change another process made meanwhile is kept.

use std::ffi::OsString;
use std::path::Path;
use std::sync::Mutex;

pub const IDENTITY: &str = "Consilium Local Signing";

/// The commands signing needs. `run` returns whether the command exited successfully; output is
/// never captured, so no argument (such as the keychain password) can reach a log through it.
pub trait Runner {
    fn run(&mut self, program: &str, args: &[OsString]) -> bool;
    /// The user's keychain search list in order, or `None` when it can't be read.
    fn search_list(&mut self) -> Option<Vec<String>>;
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// Signed with the stable identity, rather than ad-hoc.
    pub stable: bool,
    /// Signed at all.
    pub signed: bool,
    /// Why the stable identity wasn't used, if it wasn't and a key exists.
    pub stable_failure: Option<&'static str>,
    /// The search list couldn't be put back; the signing keychain may still be on it.
    pub restore_failure: Option<&'static str>,
}

static SIGNING: Mutex<()> = Mutex::new(());

fn os(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

fn set_list(list: &[String]) -> Vec<OsString> {
    let mut args = os(&["list-keychains", "-d", "user", "-s"]);
    args.extend(list.iter().map(OsString::from));
    args
}

fn same(a: &str, b: &Path) -> bool {
    Path::new(a) == b
}

/// Signs `app`, preferring the stable identity in `keychain` when `password` unlocks it.
pub fn sign(runner: &mut dyn Runner, keychain: &Path, password: Option<&str>, app: &Path) -> Report {
    let _guard = SIGNING.lock().unwrap_or_else(|p| p.into_inner());
    let mut report = Report::default();

    let unlocked = match password {
        Some(pass) => {
            let mut args = os(&["unlock-keychain", "-p", pass.trim()]);
            args.push(keychain.into());
            let ok = runner.run("/usr/bin/security", &args);
            if !ok {
                report.stable_failure = Some("the signing keychain didn't unlock");
            }
            ok
        }
        None => false,
    };

    // Join the search list only when the keychain isn't already on it, and remember that we did.
    let mut added = false;
    let joined = unlocked
        && match runner.search_list() {
            None => {
                report.stable_failure = Some("the keychain search list couldn't be read");
                false
            }
            Some(list) if list.iter().any(|k| same(k, keychain)) => true,
            Some(mut list) => {
                list.push(keychain.to_string_lossy().into_owned());
                added = runner.run("/usr/bin/security", &set_list(&list));
                if !added {
                    report.stable_failure = Some("the signing keychain couldn't join the search list");
                }
                added
            }
        };

    if joined {
        let mut args = os(&["--force", "--deep", "--sign", IDENTITY, "--keychain"]);
        args.push(keychain.into());
        args.push(app.into());
        report.stable = runner.run("/usr/bin/codesign", &args);
        if !report.stable {
            report.stable_failure = Some("codesign couldn't use the stable identity");
        }
    }
    report.signed = report.stable || {
        let mut args = os(&["--force", "--deep", "--sign", "-"]);
        args.push(app.into());
        runner.run("/usr/bin/codesign", &args)
    };

    if added {
        // Take off only what this run added, from the list as it is now.
        report.restore_failure = match runner.search_list() {
            None => Some("the keychain search list couldn't be read to remove the signing keychain"),
            Some(now) => {
                let kept: Vec<String> = now.into_iter().filter(|k| !same(k, keychain)).collect();
                (!runner.run("/usr/bin/security", &set_list(&kept)))
                    .then_some("the signing keychain couldn't be removed from the search list")
            }
        };
    }
    if unlocked {
        let mut args = os(&["lock-keychain"]);
        args.push(keychain.into());
        let _ = runner.run("/usr/bin/security", &args);
    }
    report
}

/// Parses `security list-keychains` output: one quoted path per line.
pub fn parse_list(text: &str) -> Vec<String> {
    text.lines().map(|l| l.trim().trim_matches('"').to_string()).filter(|l| !l.is_empty()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    const KC: &str = "/data/signing/consilium-signing.keychain-db";
    const LOGIN: &str = "/kc/Keychains/login.keychain-db";

    /// Scripted results, recording every command in order with the password masked.
    #[derive(Default)]
    struct Script {
        log: Vec<String>,
        results: VecDeque<bool>,
        lists: VecDeque<Option<Vec<String>>>,
    }

    impl Runner for Script {
        fn run(&mut self, program: &str, args: &[OsString]) -> bool {
            let args: Vec<String> = args.iter().map(|a| a.to_string_lossy().into_owned()).collect();
            let shown: Vec<&str> = args
                .iter()
                .enumerate()
                .map(|(i, a)| if i > 0 && args[i - 1] == "-p" { "<pass>" } else { a.as_str() })
                .collect();
            let name = program.rsplit('/').next().unwrap_or(program);
            self.log.push(format!("{name} {}", shown.join(" ")));
            self.results.pop_front().unwrap_or(true)
        }
        fn search_list(&mut self) -> Option<Vec<String>> {
            self.log.push("read-list".into());
            self.lists.pop_front().unwrap_or(None)
        }
    }

    fn list(items: &[&str]) -> Option<Vec<String>> {
        Some(items.iter().map(|s| s.to_string()).collect())
    }

    fn run(script: &mut Script, password: Option<&str>) -> Report {
        sign(script, Path::new(KC), password, Path::new("/stage/Consilium.app"))
    }

    #[test]
    fn joins_signs_and_removes_only_its_own_keychain() {
        let mut s = Script { lists: [list(&[LOGIN]), list(&[LOGIN, KC])].into(), ..Default::default() };
        let r = run(&mut s, Some("secret\n"));
        assert_eq!(r, Report { stable: true, signed: true, ..Default::default() });
        assert_eq!(
            s.log,
            vec![
                format!("security unlock-keychain -p <pass> {KC}"),
                "read-list".into(),
                format!("security list-keychains -d user -s {LOGIN} {KC}"),
                format!("codesign --force --deep --sign {IDENTITY} --keychain {KC} /stage/Consilium.app"),
                "read-list".into(),
                format!("security list-keychains -d user -s {LOGIN}"),
                format!("security lock-keychain {KC}"),
            ]
        );
    }

    #[test]
    fn a_keychain_already_on_the_list_is_left_there_and_no_snapshot_is_restored() {
        let mut s = Script { lists: [list(&[LOGIN, KC])].into(), ..Default::default() };
        let r = run(&mut s, Some("secret"));
        assert!(r.stable && r.restore_failure.is_none());
        assert!(!s.log.iter().any(|l| l.contains("list-keychains")), "{:?}", s.log);
        assert_eq!(s.log.iter().filter(|l| *l == "read-list").count(), 1);
    }

    #[test]
    fn a_change_made_by_another_process_meanwhile_is_kept() {
        let other = "/kc/Keychains/other.keychain-db";
        let mut s = Script { lists: [list(&[LOGIN]), list(&[LOGIN, other, KC])].into(), ..Default::default() };
        run(&mut s, Some("secret"));
        assert!(s.log.contains(&format!("security list-keychains -d user -s {LOGIN} {other}")), "{:?}", s.log);
    }

    #[test]
    fn a_failed_join_signs_ad_hoc_and_restores_nothing() {
        // unlock ok, join fails, ad-hoc ok, lock ok
        let mut s = Script { lists: [list(&[LOGIN])].into(), results: [true, false, true, true].into(), ..Default::default() };
        let r = run(&mut s, Some("secret"));
        assert!(!r.stable && r.signed);
        assert_eq!(r.stable_failure, Some("the signing keychain couldn't join the search list"));
        assert_eq!(r.restore_failure, None);
        assert_eq!(s.log.iter().filter(|l| l.contains("list-keychains")).count(), 1);
        assert!(s.log.contains(&"codesign --force --deep --sign - /stage/Consilium.app".to_string()));
        assert_eq!(s.log.last().unwrap(), &format!("security lock-keychain {KC}"));
    }

    #[test]
    fn a_failed_stable_sign_falls_back_to_ad_hoc_and_still_restores() {
        // unlock, join, stable codesign fails, ad-hoc, restore, lock
        let mut s = Script {
            lists: [list(&[LOGIN]), list(&[LOGIN, KC])].into(),
            results: [true, true, false, true, true, true].into(),
            ..Default::default()
        };
        let r = run(&mut s, Some("secret"));
        assert!(!r.stable && r.signed && r.restore_failure.is_none());
        assert_eq!(r.stable_failure, Some("codesign couldn't use the stable identity"));
        assert!(s.log.contains(&format!("security list-keychains -d user -s {LOGIN}")));
    }

    #[test]
    fn a_failed_restore_is_reported() {
        // unlock, join, sign, restore fails, lock
        let mut s = Script {
            lists: [list(&[LOGIN]), list(&[LOGIN, KC])].into(),
            results: [true, true, true, false, true].into(),
            ..Default::default()
        };
        let r = run(&mut s, Some("secret"));
        assert!(r.stable);
        assert_eq!(r.restore_failure, Some("the signing keychain couldn't be removed from the search list"));
        assert_eq!(s.log.last().unwrap(), &format!("security lock-keychain {KC}"));
    }

    #[test]
    fn an_unreadable_list_after_signing_is_reported() {
        let mut s = Script { lists: [list(&[LOGIN]), None].into(), ..Default::default() };
        let r = run(&mut s, Some("secret"));
        assert!(r.stable);
        assert!(r.restore_failure.unwrap().contains("couldn't be read"));
    }

    #[test]
    fn a_failed_unlock_or_no_key_signs_ad_hoc_without_touching_the_list() {
        let mut s = Script { results: [false, true].into(), ..Default::default() };
        let r = run(&mut s, Some("wrong"));
        assert_eq!(r.stable_failure, Some("the signing keychain didn't unlock"));
        assert_eq!(s.log, vec![format!("security unlock-keychain -p <pass> {KC}"), "codesign --force --deep --sign - /stage/Consilium.app".into()]);

        let mut s = Script::default();
        let r = run(&mut s, None);
        assert!(r.signed && !r.stable && r.stable_failure.is_none());
        assert_eq!(s.log, vec!["codesign --force --deep --sign - /stage/Consilium.app".to_string()]);
    }

    #[test]
    fn parses_quoted_paths_in_order() {
        let out = "    \"/kc/Keychains/login.keychain-db\"\n    \"/kc/Application Support/x/signing/consilium-signing.keychain-db\"\n\n";
        assert_eq!(
            parse_list(out),
            vec!["/kc/Keychains/login.keychain-db".to_string(), "/kc/Application Support/x/signing/consilium-signing.keychain-db".to_string()]
        );
    }
}
