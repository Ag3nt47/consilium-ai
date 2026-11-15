//! Update Now: installs a build staged in the app's data folder and relaunches into it.
//!
//! `scripts/stage-update.sh` builds the app and moves it to `<data>/update/Consilium.app` in one
//! rename, so a half-copied app is never offered. The window polls `staged`, and Update Now hands
//! the swap to a small detached shell helper: it waits for this process to exit, renames the staged
//! app over the installed one (same volume, so it is instant) and opens it. Turns cut short resume
//! on the next launch through the resume journal, as after any quit.

use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Staged {
    pub version: String,
    pub current: String,
}

pub fn staged_app(data: &Path) -> PathBuf {
    data.join("update").join("Consilium.app")
}

/// The version string in a macOS app bundle's Info.plist.
fn bundle_version(app: &Path) -> Option<String> {
    let plist = std::fs::read_to_string(app.join("Contents").join("Info.plist")).ok()?;
    let at = plist.find("<key>CFBundleShortVersionString</key>")?;
    let rest = &plist[at..];
    let start = rest.find("<string>")? + "<string>".len();
    let end = rest[start..].find("</string>")? + start;
    Some(rest[start..end].trim().to_string())
}

fn parse(v: &str) -> Vec<u64> {
    v.split(['.', '-', '+']).map_while(|p| p.parse().ok()).collect()
}

/// Whether `staged` is a newer version than `current`.
pub fn newer(staged: &str, current: &str) -> bool {
    parse(staged) > parse(current)
}

/// The staged update, if there is one newer than this build.
pub fn staged(data: &Path) -> Option<Staged> {
    let version = bundle_version(&staged_app(data))?;
    let current = env!("CARGO_PKG_VERSION").to_string();
    newer(&version, &current).then_some(Staged { version, current })
}

/// The installed `.app` this process runs from (…/Consilium.app/Contents/MacOS/consilium).
fn running_bundle() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let app = exe.ancestors().nth(3).ok_or("can't find the running app")?;
    if app.extension().and_then(|e| e.to_str()) != Some("app") {
        return Err("Consilium isn't running from an installed .app, so it can't replace itself".into());
    }
    Ok(app.to_path_buf())
}

fn quote(p: &Path) -> String {
    format!("'{}'", p.display().to_string().replace('\'', "'\\''"))
}

/// The helper script: wait for `pid` to exit (30 s, then force it), swap, relaunch. On a failed
/// swap the old app is put back and reopened, so the human is never left without Consilium.
pub fn helper_script(pid: u32, staged: &Path, target: &Path) -> String {
    let (s, t) = (quote(staged), quote(target));
    let old = quote(&target.with_extension("app.old"));
    format!(
        "for i in $(seq 1 60); do kill -0 {pid} 2>/dev/null || break; sleep 0.5; done\n\
         kill -9 {pid} 2>/dev/null; sleep 0.3\n\
         rm -rf {old}\n\
         if mv {t} {old} && mv {s} {t}; then rm -rf {old}; else [ -e {t} ] || mv {old} {t}; fi\n\
         open {t}\n"
    )
}

/// Starts the detached helper. The caller then exits the app.
#[cfg(target_os = "macos")]
pub fn start_install(data: &Path) -> Result<Staged, String> {
    let s = staged(data).ok_or("no newer build is staged")?;
    let target = running_bundle()?;
    let script = helper_script(std::process::id(), &staged_app(data), &target);
    std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("couldn't start the installer: {e}"))?;
    Ok(s)
}

#[cfg(not(target_os = "macos"))]
pub fn start_install(_data: &Path) -> Result<Staged, String> {
    Err("Update Now installs on macOS only for now; on this system, install the new build by hand".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically() {
        assert!(newer("0.3.10", "0.3.9"));
        assert!(newer("0.4.0", "0.3.1"));
        assert!(!newer("0.3.1", "0.3.1"));
        assert!(!newer("0.3.0", "0.3.1"));
    }

    #[test]
    fn reads_the_staged_version_only_when_newer() {
        let dir = std::env::temp_dir().join(format!("consilium-update-{}", std::process::id()));
        let contents = staged_app(&dir).join("Contents");
        std::fs::create_dir_all(&contents).unwrap();
        let plist = |v: &str| {
            format!("<dict>\n<key>CFBundleName</key>\n<string>Consilium</string>\n<key>CFBundleShortVersionString</key>\n<string>{v}</string>\n</dict>")
        };
        std::fs::write(contents.join("Info.plist"), plist("999.0.0")).unwrap();
        assert_eq!(staged(&dir).map(|s| s.version), Some("999.0.0".into()));
        std::fs::write(contents.join("Info.plist"), plist("0.0.1")).unwrap();
        assert_eq!(staged(&dir), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn helper_waits_swaps_and_restores_on_failure() {
        let s = helper_script(42, Path::new("/d/update/Consilium.app"), Path::new("/Applications/Consilium.app"));
        assert!(s.starts_with("for i in"));
        assert!(s.contains("kill -0 42"));
        assert!(s.contains("mv '/Applications/Consilium.app' '/Applications/Consilium.app.old' && mv '/d/update/Consilium.app' '/Applications/Consilium.app'"));
        assert!(s.contains("mv '/Applications/Consilium.app.old' '/Applications/Consilium.app'"));
        assert!(s.trim_end().ends_with("open '/Applications/Consilium.app'"));
    }
}
