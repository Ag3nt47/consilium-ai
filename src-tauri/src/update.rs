//! Update Now: installs a build staged in the app's data folder and relaunches into it.
//!
//! `scripts/stage-update.mjs` builds the app and stages it in `<data>/update` with one rename, so
//! a half-copied build is never offered:
//! - macOS: `update/Consilium.app`, whose Info.plist holds the version.
//! - Windows and Linux: `update/next/` holding the new program (`consilium.exe`, or `consilium`
//!   plus `Consilium.AppImage`, `Consilium.deb` or `Consilium.rpm` on Linux) and a `VERSION` file.
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
        None => {
            for pkg in ["Consilium.deb", "Consilium.rpm"] {
                if dir.join(pkg).is_file() {
                    // The package manager replaces the system-wide install; no write check here.
                    return Ok((dir.join(pkg), exe.to_path_buf()));
                }
            }
            (dir.join("consilium"), exe.to_path_buf())
        }
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

/// The Linux helper for a .deb or .rpm install: after the app exits, the system's package
/// manager installs the staged package through pkexec, which shows the system's own password
/// prompt. If it is cancelled or fails, the old install is untouched and is reopened.
pub fn linux_package_script(pid: u32, staged: &Path, target: &Path, leftover: &Path) -> String {
    let (s, t, l) = (quote(staged), quote(target), quote(leftover));
    let install = if staged.extension().and_then(|e| e.to_str()) == Some("rpm") {
        format!("if command -v dnf >/dev/null; then pkexec dnf install -y {s}; elif command -v zypper >/dev/null; then pkexec zypper --non-interactive install --allow-unsigned-rpm {s}; else pkexec rpm -U {s}; fi")
    } else {
        format!("pkexec apt-get install -y {s} || pkexec dpkg -i {s}")
    };
    format!(
        "for i in $(seq 1 60); do kill -0 {pid} 2>/dev/null || break; sleep 0.5; done\n\
         kill -9 {pid} 2>/dev/null; sleep 0.3\n\
         {install}\n\
         rm -rf {l}\n\
         nohup {t} >/dev/null 2>&1 &\n"
    )
}

/// How this Linux copy was installed: "deb" or "rpm" when the running program belongs to a
/// system package, otherwise None (an AppImage or a copied program file).
pub fn linux_package_kind() -> Option<&'static str> {
    if !cfg!(target_os = "linux") || std::env::var_os("APPIMAGE").is_some() {
        return None;
    }
    let exe = std::env::current_exe().ok()?;
    if !exe.starts_with("/usr") {
        return None;
    }
    let owns = |tool: &str, arg: &str| {
        std::process::Command::new(tool)
            .arg(arg)
            .arg(&exe)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    };
    if owns("dpkg", "-S") {
        Some("deb")
    } else if owns("rpm", "-qf") {
        Some("rpm")
    } else {
        None
    }
}

/// The Windows helper. A running .exe can be renamed but not overwritten, so the same
/// rename-aside swap works once the old process has exited. A setup installer is run silently
/// and the app is reopened from where the installer put it (its uninstall entry's DisplayIcon),
/// which can differ from the copy that was running. Every step goes to `log`, so a failed
/// update can be explained afterwards.
pub fn windows_helper_script(pid: u32, staged: &Path, target: &Path, leftover: &Path, log: &Path) -> String {
    let (s, t, l, g) = (ps_quote(staged), ps_quote(target), ps_quote(leftover), ps_quote(log));
    let old = ps_quote(&PathBuf::from(format!("{}.old", target.display())));
    format!(
        "$ErrorActionPreference = 'SilentlyContinue'\n\
         function Log($m) {{ Add-Content -LiteralPath {g} -Value ((Get-Date -Format o) + ' ' + $m) }}\n\
         Set-Content -LiteralPath {g} -Value ((Get-Date -Format o) + ' update helper started for pid {pid}')\n\
         for ($i = 0; $i -lt 60; $i++) {{ if (-not (Get-Process -Id {pid})) {{ break }}; Start-Sleep -Milliseconds 500 }}\n\
         if (Get-Process -Id {pid}) {{ Log 'app still running after 30 s; stopping it'; Stop-Process -Id {pid} -Force }}\n\
         Start-Sleep -Milliseconds 500\n\
         $run = {t}\n\
         if ({s} -like '*-Setup.exe' -or {s} -like '*-setup.exe') {{\n\
             $p = Start-Process -FilePath {s} -ArgumentList '/S' -Wait -PassThru\n\
             Log ('installer exit code ' + $p.ExitCode)\n\
             $keys = 'HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\*', 'HKLM:\\Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\*'\n\
             $icon = Get-ItemProperty -Path $keys | Where-Object {{ $_.DisplayName -eq 'Consilium' -and $_.DisplayIcon }} | Select-Object -First 1 -ExpandProperty DisplayIcon\n\
             if ($icon) {{ $icon = $icon.Trim([char]34).Split(',')[0] }}\n\
             if ($icon -and (Test-Path -LiteralPath $icon)) {{ $run = $icon }}\n\
             if ($p.ExitCode -eq 0) {{ Remove-Item -LiteralPath {l} -Recurse -Force }}\n\
         }} else {{\n\
             Remove-Item -LiteralPath {old} -Force\n\
             try {{ Move-Item -LiteralPath {t} -Destination {old} -ErrorAction Stop; Move-Item -LiteralPath {s} -Destination {t} -ErrorAction Stop; Remove-Item -LiteralPath {old} -Force; Log 'swapped the program file' }}\n\
             catch {{ Log ('swap failed: ' + $_); if (-not (Test-Path -LiteralPath {t})) {{ Move-Item -LiteralPath {old} -Destination {t} }} }}\n\
             Remove-Item -LiteralPath {l} -Recurse -Force\n\
         }}\n\
         Log ('reopening ' + $run)\n\
         try {{ Start-Process -FilePath $run -ErrorAction Stop }} catch {{ Log ('reopen failed: ' + $_) }}\n"
    )
}

/// Where the Windows helper writes what it did.
pub fn install_log(data: &Path) -> PathBuf {
    data.join("update").join("install.log")
}

/// Downloads and stages a release binary into `<data>/update/next`.
/// Release downloads are only taken from this project's own GitHub releases.
const RELEASE_PREFIX: &str = "https://github.com/Ag3nt47/consilium-ai/releases/download/";

/// Checks a release download before anything is fetched: it must be one of this project's
/// GitHub release files, the version must be a plain version number, and GitHub's published
/// SHA-256 (`sha256:<64 hex>`) must be supplied so the file can be checked after download.
fn check_release_download(version: &str, url: &str, digest: &str) -> Result<String, String> {
    let path = url.strip_prefix(RELEASE_PREFIX).ok_or("update downloads must come from Consilium's own GitHub releases")?;
    if path.contains("..") || path.contains('?') || path.contains('#') || path.split('/').count() != 2 {
        return Err("the update download address is not a release file".into());
    }
    let v = version.trim_start_matches(['v', 'V']);
    if v.is_empty() || !v.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return Err("the update version is not a version number".into());
    }
    let hex = digest.strip_prefix("sha256:").unwrap_or("").to_ascii_lowercase();
    if hex.len() != 64 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("GitHub gave no checksum for this update, so it was not downloaded".into());
    }
    Ok(hex)
}

pub async fn stage_from_url(data: &Path, version: &str, url: &str, digest: &str) -> Result<(), String> {
    use sha2::{Digest, Sha256};
    let want = check_release_download(version, url, digest)?;
    let client = reqwest::Client::builder()
        .user_agent("Consilium-Updater")
        .build()
        .map_err(|e| format!("HTTP client error: {e}"))?;
    let resp = client.get(url).send().await.map_err(|e| format!("download failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("download returned HTTP {}", resp.status()));
    }
    let bytes = resp.bytes().await.map_err(|e| format!("read failed: {e}"))?;
    let got: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
    if got != want {
        return Err("the downloaded update didn't match GitHub's checksum, so it was thrown away".into());
    }
    let next_dir = staged_dir(data);
    let tmp_dir = data.join("update").join("tmp_download");
    let _ = std::fs::remove_dir_all(&tmp_dir);
    std::fs::create_dir_all(&tmp_dir).map_err(|e| format!("create tmp dir: {e}"))?;

    let filename = if cfg!(windows) {
        "Consilium-Windows-Setup.exe"
    } else if cfg!(target_os = "linux") {
        match url.rsplit('.').next().map(str::to_ascii_lowercase).as_deref() {
            Some("deb") => "Consilium.deb",
            Some("rpm") => "Consilium.rpm",
            _ => "Consilium.AppImage",
        }
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
                .arg(windows_helper_script(pid, &staged, &target, &staged_dir(data), &install_log(data)));
        } else {
            cmd = Command::new("/bin/sh");
            let pkg = matches!(staged.extension().and_then(|e| e.to_str()), Some("deb" | "rpm"));
            let script = if pkg {
                linux_package_script(pid, &staged, &target, &staged_dir(data))
            } else {
                linux_helper_script(pid, &staged, &target, &staged_dir(data))
            };
            cmd.arg("-c").arg(script);
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
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        // If Consilium was started inside a job that ends its children when it exits (some
        // launchers and terminals do this), the helper must leave that job or it dies with the
        // app and nothing reopens. A job that forbids leaving refuses the flag, so retry without.
        cmd.creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS | CREATE_BREAKAWAY_FROM_JOB);
        if let Ok(child) = cmd.spawn() {
            drop(child);
            return Ok(s);
        }
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
    fn linux_package_helper_uses_the_package_manager() {
        let deb = linux_package_script(7, Path::new("/d/next/Consilium.deb"), Path::new("/usr/bin/consilium"), Path::new("/d/next"));
        assert!(deb.contains("pkexec apt-get install -y '/d/next/Consilium.deb'"));
        assert!(deb.contains("nohup '/usr/bin/consilium'"));
        let rpm = linux_package_script(7, Path::new("/d/next/Consilium.rpm"), Path::new("/usr/bin/consilium"), Path::new("/d/next"));
        assert!(rpm.contains("pkexec dnf install -y '/d/next/Consilium.rpm'"));
        assert!(!rpm.contains("apt-get"));
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
            Path::new(r"C:\Users\O'Neil\AppData\Roaming\c\update\install.log"),
        );
        assert!(s.contains("Get-Process -Id 9"));
        assert!(s.contains(r"Add-Content -LiteralPath 'C:\Users\O''Neil\AppData\Roaming\c\update\install.log'"));
        assert!(s.contains(r"Move-Item -LiteralPath 'C:\Users\O''Neil\AppData\Local\Consilium\Consilium.exe' -Destination 'C:\Users\O''Neil\AppData\Local\Consilium\Consilium.exe.old'"));
        assert!(s.contains("if (-not (Test-Path -LiteralPath"));
        assert!(s.contains(r"$run = 'C:\Users\O''Neil\AppData\Local\Consilium\Consilium.exe'"));
        assert!(s.contains("Start-Process -FilePath $run"));

        let setup = windows_helper_script(
            9,
            Path::new(r"C:\Users\O'Neil\AppData\Roaming\c\update\next\Consilium-Windows-Setup.exe"),
            Path::new(r"C:\Users\O'Neil\AppData\Local\Consilium\Consilium.exe"),
            Path::new(r"C:\Users\O'Neil\AppData\Roaming\c\update\next"),
            Path::new(r"C:\Users\O'Neil\AppData\Roaming\c\update\install.log"),
        );
        assert!(setup.contains("Start-Process -FilePath 'C:\\Users\\O''Neil\\AppData\\Roaming\\c\\update\\next\\Consilium-Windows-Setup.exe' -ArgumentList '/S' -Wait -PassThru"));
        assert!(setup.contains("DisplayIcon"));
        assert!(setup.contains("if ($p.ExitCode -eq 0)"));
        assert!(setup.trim_end().ends_with("catch { Log ('reopen failed: ' + $_) }"));
    }

    #[test]
    fn release_downloads_are_checked() {
        let url = "https://github.com/Ag3nt47/consilium-ai/releases/download/v0.3.28/Consilium-Windows-Setup.exe";
        let d = format!("sha256:{}", "a".repeat(64));
        assert_eq!(check_release_download("v0.3.28", url, &d).unwrap(), "a".repeat(64));
        assert!(check_release_download("v0.3.28", url, "").is_err());
        assert!(check_release_download("v0.3.28", "https://evil.example/x.exe", &d).is_err());
        assert!(check_release_download("v0.3.28", "https://github.com/Ag3nt47/consilium-ai/releases/download/v1/../../x/y.exe", &d).is_err());
        assert!(check_release_download("0.3.28; rm", url, &d).is_err());
    }
}
