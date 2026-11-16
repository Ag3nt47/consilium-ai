//! Update Now: installs a build staged in the app's data folder and relaunches into it.
//!
//! `scripts/stage-update.mjs` builds the app and stages it in `<data>/update` with one rename, so
//! a half-copied build is never offered:
//! - macOS: `update/Consilium.app`, whose Info.plist holds the version.
//! - Windows and Linux: `update/next/` holding the new program (`consilium.exe`, or `consilium`
//!   plus `Consilium.AppImage` on Linux) and a `VERSION` file.
//!
//! The window polls `staged`, and Update Now hands the swap to a small detached helper (`sh` on
//! macOS and Linux, PowerShell on Windows): it waits for this process to exit, renames the staged
//! program over the installed one and opens it. If the swap fails the old program is put back and
//! opened. Turns cut short resume on the next launch through the resume journal, as after any quit.

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

/// Where Windows and Linux builds are staged.
pub fn staged_dir(data: &Path) -> PathBuf {
    data.join("update").join("next")
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

/// The version of the staged build for this system.
fn staged_version(data: &Path) -> Option<String> {
    if cfg!(target_os = "macos") {
        bundle_version(&staged_app(data))
    } else {
        let v = std::fs::read_to_string(staged_dir(data).join("VERSION")).ok()?;
        Some(v.trim().to_string()).filter(|v| !v.is_empty())
    }
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
    let version = staged_version(data)?;
    let current = env!("CARGO_PKG_VERSION").to_string();
    newer(&version, &current).then_some(Staged { version, current })
}

/// The installed `.app` this process runs from (…/Consilium.app/Contents/MacOS/consilium).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn running_bundle() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let app = exe.ancestors().nth(3).ok_or("can't find the running app")?;
    if app.extension().and_then(|e| e.to_str()) != Some("app") {
        return Err("Consilium isn't running from an installed .app, so it can't replace itself".into());
    }
    Ok(app.to_path_buf())
}

/// What a Windows or Linux install replaces, and with which staged file. A Linux AppImage is
/// replaced by the staged AppImage (the running program lives inside its read-only mount);
/// otherwise the running program file is replaced by the staged program.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn swap_paths(data: &Path, exe: &Path, appimage: Option<&Path>) -> Result<(PathBuf, PathBuf), String> {
    let dir = staged_dir(data);
    let (staged, target) = match appimage {
        Some(a) => (dir.join("Consilium.AppImage"), a.to_path_buf()),
        None if cfg!(windows) => {
            if dir.join("Consilium-Windows-Setup.exe").is_file() {
                (dir.join("Consilium-Windows-Setup.exe"), exe.to_path_buf())
            } else {
                (dir.join("consilium.exe"), exe.to_path_buf())
            }
        }
        None => (dir.join("consilium"), exe.to_path_buf()),
    };
    if !staged.is_file() {
        return Err(format!("the staged build has no {}", staged.file_name().unwrap_or_default().to_string_lossy()));
    }
    let folder = target.parent().ok_or("can't find the folder Consilium is installed in")?;
    let probe = folder.join(".consilium-update-check");
    std::fs::write(&probe, b"").map_err(|_| {
        format!(
            "Consilium is installed in {}, which this account can't change (a system-wide package?). Install the new build with the system's package manager instead",
            folder.display()
        )
    })?;
    let _ = std::fs::remove_file(&probe);
    Ok((staged, target))
}

fn quote(p: &Path) -> String {
    format!("'{}'", p.display().to_string().replace('\'', "'\\''"))
}

/// PowerShell single-quoted string: a quote is written twice.
fn ps_quote(p: &Path) -> String {
    format!("'{}'", p.display().to_string().replace('\'', "''"))
}

/// The macOS helper: wait for `pid` to exit (30 s, then force it), swap, relaunch. On a failed
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

/// The Linux helper: as on macOS, for a single program file, then starts it detached.
pub fn linux_helper_script(pid: u32, staged: &Path, target: &Path, leftover: &Path) -> String {
    let (s, t, l) = (quote(staged), quote(target), quote(leftover));
    let old = quote(&PathBuf::from(format!("{}.old", target.display())));
    format!(
        "for i in $(seq 1 60); do kill -0 {pid} 2>/dev/null || break; sleep 0.5; done\n\
         kill -9 {pid} 2>/dev/null; sleep 0.3\n\
         rm -f {old}\n\
         chmod 755 {s}\n\
         if mv {t} {old} && mv {s} {t}; then rm -f {old}; else [ -e {t} ] || mv {old} {t}; fi\n\
         rm -rf {l}\n\
         nohup {t} >/dev/null 2>&1 &\n"
    )
}

/// The Windows helper. A running .exe can be renamed but not overwritten, so the same
/// rename-aside swap works once the old process has exited.
pub fn windows_helper_script(pid: u32, staged: &Path, target: &Path, leftover: &Path) -> String {
    let (s, t, l) = (ps_quote(staged), ps_quote(target), ps_quote(leftover));
    let old = ps_quote(&PathBuf::from(format!("{}.old", target.display())));
    format!(
        "$ErrorActionPreference = 'SilentlyContinue'\n\
         for ($i = 0; $i -lt 60; $i++) {{ if (-not (Get-Process -Id {pid})) {{ break }}; Start-Sleep -Milliseconds 500 }}\n\
         Stop-Process -Id {pid} -Force; Start-Sleep -Milliseconds 300\n\
         if ({s} -like '*-Setup.exe' -or {s} -like '*-setup.exe') {{\n\
             Start-Process -FilePath {s} -ArgumentList '/S' -Wait\n\
             Remove-Item -LiteralPath {l} -Recurse -Force\n\
         }} else {{\n\
             Remove-Item -LiteralPath {old} -Force\n\
             try {{ Move-Item -LiteralPath {t} -Destination {old} -ErrorAction Stop; Move-Item -LiteralPath {s} -Destination {t} -ErrorAction Stop; Remove-Item -LiteralPath {old} -Force }}\n\
             catch {{ if (-not (Test-Path -LiteralPath {t})) {{ Move-Item -LiteralPath {old} -Destination {t} }} }}\n\
             Remove-Item -LiteralPath {l} -Recurse -Force\n\
         }}\n\
         Start-Process -FilePath {t}\n"
    )
}

/// Downloads and stages a release binary into `<data>/update/next`.
pub async fn stage_from_url(data: &Path, version: &str, url: &str) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .user_agent("Consilium-Updater")
        .build()
        .map_err(|e| format!("HTTP client error: {e}"))?;
    let resp = client.get(url).send().await.map_err(|e| format!("download failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("download returned HTTP {}", resp.status()));
    }
    let bytes = resp.bytes().await.map_err(|e| format!("read failed: {e}"))?;
    let next_dir = staged_dir(data);
    let tmp_dir = data.join("update").join("tmp_download");
    let _ = std::fs::remove_dir_all(&tmp_dir);
    std::fs::create_dir_all(&tmp_dir).map_err(|e| format!("create tmp dir: {e}"))?;

    let filename = if cfg!(windows) {
        "Consilium-Windows-Setup.exe"
    } else if cfg!(target_os = "linux") {
        "Consilium.AppImage"
    } else {
        "update.bin"
    };

    let target_file = tmp_dir.join(filename);
    std::fs::write(&target_file, &bytes).map_err(|e| format!("write asset: {e}"))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&target_file, std::fs::Permissions::from_mode(0o755));
    }

    let clean_ver = version.trim_start_matches(['v', 'V']).trim();
    std::fs::write(tmp_dir.join("VERSION"), format!("{clean_ver}\n")).map_err(|e| format!("write version: {e}"))?;

    let _ = std::fs::remove_dir_all(&next_dir);
    std::fs::rename(&tmp_dir, &next_dir).map_err(|e| format!("stage update: {e}"))?;
    Ok(())
}

/// Starts the detached helper. The caller then exits the app.
pub fn start_install(data: &Path) -> Result<Staged, String> {
    use std::process::{Command, Stdio};
    let s = staged(data).ok_or("no newer build is staged")?;
    let pid = std::process::id();
    let mut cmd;
    if cfg!(target_os = "macos") {
        let target = running_bundle()?;
        cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg(helper_script(pid, &staged_app(data), &target));
    } else {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let appimage = std::env::var_os("APPIMAGE").filter(|_| cfg!(target_os = "linux")).map(PathBuf::from);
        let (staged, target) = swap_paths(data, &exe, appimage.as_deref())?;
        if cfg!(windows) {
            cmd = Command::new("powershell.exe");
            cmd.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-WindowStyle", "Hidden", "-Command"])
                .arg(windows_helper_script(pid, &staged, &target, &staged_dir(data)));
        } else {
            cmd = Command::new("/bin/sh");
            cmd.arg("-c").arg(linux_helper_script(pid, &staged, &target, &staged_dir(data)));
        }
    }
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0); // outlives this app's process group
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        cmd.creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS);
    }
    cmd.spawn().map_err(|e| format!("couldn't start the installer: {e}"))?;
    Ok(s)
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
    #[cfg(target_os = "macos")] // Windows and Linux read update/next/VERSION, tested below
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

    #[test]
    fn reads_a_windows_or_linux_staged_version_file() {
        let dir = std::env::temp_dir().join(format!("consilium-update-next-{}", std::process::id()));
        std::fs::create_dir_all(staged_dir(&dir)).unwrap();
        std::fs::write(staged_dir(&dir).join("VERSION"), "999.1.0\n").unwrap();
        if !cfg!(target_os = "macos") {
            assert_eq!(staged(&dir).map(|s| s.version), Some("999.1.0".into()));
        }
        // A program file is required before anything is swapped.
        assert!(swap_paths(&dir, &dir.join("consilium"), None).is_err());
        let name = if cfg!(windows) { "consilium.exe" } else { "consilium" };
        std::fs::write(staged_dir(&dir).join(name), b"new").unwrap();
        let exe = dir.join("installed").join(name);
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        let (s, t) = swap_paths(&dir, &exe, None).unwrap();
        assert_eq!((s, t), (staged_dir(&dir).join(name), exe));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn linux_helper_swaps_and_restores_on_failure() {
        let s = linux_helper_script(7, Path::new("/d/next/consilium"), Path::new("/home/user/bin/consilium"), Path::new("/d/next"));
        assert!(s.contains("kill -0 7"));
        assert!(s.contains("mv '/home/user/bin/consilium' '/home/user/bin/consilium.old' && mv '/d/next/consilium' '/home/user/bin/consilium'"));
        assert!(s.contains("[ -e '/home/user/bin/consilium' ] || mv '/home/user/bin/consilium.old' '/home/user/bin/consilium'"));
        assert!(s.trim_end().ends_with("nohup '/home/user/bin/consilium' >/dev/null 2>&1 &"));
    }

    #[test]
    fn windows_helper_swaps_restores_and_quotes() {
        let s = windows_helper_script(
            9,
            Path::new(r"C:\Users\O'Neil\AppData\Roaming\c\update\next\consilium.exe"),
            Path::new(r"C:\Users\O'Neil\AppData\Local\Consilium\Consilium.exe"),
            Path::new(r"C:\Users\O'Neil\AppData\Roaming\c\update\next"),
        );
        assert!(s.contains("Get-Process -Id 9"));
        assert!(s.contains(r"Move-Item -LiteralPath 'C:\Users\O''Neil\AppData\Local\Consilium\Consilium.exe' -Destination 'C:\Users\O''Neil\AppData\Local\Consilium\Consilium.exe.old'"));
        assert!(s.contains("catch { if (-not (Test-Path"));
        assert!(s.trim_end().ends_with(r"Start-Process -FilePath 'C:\Users\O''Neil\AppData\Local\Consilium\Consilium.exe'"));

        let setup = windows_helper_script(
            9,
            Path::new(r"C:\Users\O'Neil\AppData\Roaming\c\update\next\Consilium-Windows-Setup.exe"),
            Path::new(r"C:\Users\O'Neil\AppData\Local\Consilium\Consilium.exe"),
            Path::new(r"C:\Users\O'Neil\AppData\Roaming\c\update\next"),
        );
        assert!(setup.contains("Start-Process -FilePath 'C:\\Users\\O''Neil\\AppData\\Roaming\\c\\update\\next\\Consilium-Windows-Setup.exe' -ArgumentList '/S' -Wait"));
        assert!(setup.trim_end().ends_with(r"Start-Process -FilePath 'C:\Users\O''Neil\AppData\Local\Consilium\Consilium.exe'"));
    }
}
