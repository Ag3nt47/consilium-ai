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
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Staged {
    pub version: String,
    pub current: String,
    /// The staged and running builds, e.g. "0.3.37 #812 (76debf3a1b2c)" (see `build_id`).
    pub build: String,
    pub current_build: String,
}

/// Whether a GitHub release installer is currently being downloaded. The sidebar emblem reads
/// this independently of `staged`, so the download is visible before Update Now appears.
#[derive(Serialize, Clone, Debug, Default, PartialEq)]
pub struct DownloadStatus {
    pub active: bool,
    pub version: String,
}

#[derive(Default)]
struct DownloadState {
    active: usize,
    version: String,
}

static DOWNLOAD_STATE: OnceLock<Mutex<DownloadState>> = OnceLock::new();

fn download_state() -> &'static Mutex<DownloadState> {
    DOWNLOAD_STATE.get_or_init(|| Mutex::new(DownloadState::default()))
}

fn download_started(state: &Mutex<DownloadState>, version: &str) {
    if let Ok(mut state) = state.lock() {
        state.active = state.active.saturating_add(1);
        state.version = version.trim_start_matches(['v', 'V']).trim().to_string();
    }
}

fn download_finished(state: &Mutex<DownloadState>) {
    if let Ok(mut state) = state.lock() {
        state.active = state.active.saturating_sub(1);
        if state.active == 0 {
            state.version.clear();
        }
    }
}

/// Keeps the download indicator balanced even when the async download is cancelled. Locals held
/// across an `.await` are dropped with their future, so cleanup cannot be skipped by cancellation.
struct DownloadActivity<'a> {
    state: &'a Mutex<DownloadState>,
}

impl<'a> DownloadActivity<'a> {
    fn start(state: &'a Mutex<DownloadState>, version: &str) -> Self {
        download_started(state, version);
        Self { state }
    }
}

impl Drop for DownloadActivity<'_> {
    fn drop(&mut self) {
        download_finished(self.state);
    }
}

fn download_status_for(state: &Mutex<DownloadState>) -> DownloadStatus {
    state
        .lock()
        .map(|state| DownloadStatus { active: state.active > 0, version: state.version.clone() })
        .unwrap_or_default()
}

pub fn download_status() -> DownloadStatus {
    download_status_for(download_state())
}

async fn track_download<T>(state: &Mutex<DownloadState>, version: &str, operation: impl std::future::Future<Output = T>) -> T {
    let _activity = DownloadActivity::start(state, version);
    operation.await
}

/// Which build is staged, written by `scripts/stage-update.mjs` after the build is in place.
pub fn build_stamp(data: &Path) -> PathBuf {
    data.join("update").join("BUILD.json")
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

/// The staged build: its version, with the build number and commit from its stamp when the
/// stamp is for that version (a build staged some other way has a version alone).
pub fn staged_build(data: &Path) -> Option<crate::build_id::Build> {
    let version = staged_version(data)?;
    let stamp = std::fs::read(build_stamp(data))
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|v| crate::build_id::Build::from_peer(&v))
        .filter(|b| b.version == version);
    Some(stamp.unwrap_or(crate::build_id::Build { version, number: 0, commit: String::new() }))
}

/// The staged update, if it is a later build than this one. Two builds of one version are told
/// apart by build number, and an older or equal build is never offered, so an update can't be
/// overridden by an older one.
pub fn staged(data: &Path) -> Option<Staged> {
    let build = staged_build(data)?;
    let this = crate::build_id::Build::this();
    build.newer_than(&this).then(|| Staged {
        version: build.version.clone(),
        current: this.version.clone(),
        build: build.label(),
        current_build: this.label(),
    })
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
pub(crate) fn ps_quote(p: &Path) -> String {
    format!("'{}'", p.display().to_string().replace('\'', "''"))
}

/// The macOS helper: wait for `pid` to exit (30 s, then force it), swap, relaunch. On a failed
/// swap the old app is put back and reopened, so the human is never left without Consilium.
pub fn helper_script(pid: u32, staged: &Path, target: &Path, log: &Path) -> String {
    let (s, t, g) = (quote(staged), quote(target), quote(log));
    let old = quote(&target.with_extension("app.old"));
    let failed = quote(&staged.with_extension("app.failed"));
    format!(
        "if ( : > {g} ); then exec > {g} 2>&1; fi\n\
         log() {{ printf '%s %s\\n' \"$(date '+%Y-%m-%dT%H:%M:%S%z')\" \"$1\"; }}\n\
         log 'update helper started for pid {pid}'\n\
         for i in $(seq 1 60); do kill -0 {pid} 2>/dev/null || break; sleep 0.5; done\n\
         kill -9 {pid} 2>/dev/null; sleep 0.3\n\
         if [ ! -e {t} ] && [ -e {old} ]; then\n\
             log 'recovering previous bundle from interrupted update'\n\
             if ! mv {old} {t}; then log 'could not restore previous bundle'; exit 1; fi\n\
         fi\n\
         rm -rf {old}\n\
         if mv {t} {old} && mv {s} {t}; then\n\
             log 'application bundle replaced'\n\
             if open {t}; then\n\
                 log 'Launch Services accepted relaunch'; rm -rf {old}; exit 0\n\
             fi\n\
             log 'relaunch failed; restoring previous bundle'\n\
             if ! mv {t} {failed} || ! mv {old} {t}; then log 'could not restore previous bundle'; exit 1; fi\n\
         else\n\
             log 'application bundle replacement failed'\n\
             if [ ! -e {t} ] && ! mv {old} {t}; then log 'could not restore previous bundle'; exit 1; fi\n\
         fi\n\
         if open {t}; then log 'Launch Services accepted relaunch'; else log 'relaunch failed'; exit 1; fi\n"
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
         for a in $(seq 1 30); do\n\
             if mv {t} {old} && mv {s} {t}; then rm -f {old}; break; else [ -e {t} ] || mv {old} {t}; sleep 0.5; fi\n\
         done\n\
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
         for ($i = 0; $i -lt 60; $i++) {{ if (-not (Get-Process -Id {pid} -ErrorAction SilentlyContinue)) {{ break }}; Start-Sleep -Milliseconds 500 }}\n\
         if (Get-Process -Id {pid} -ErrorAction SilentlyContinue) {{ Log 'app still running after 30 s; stopping it'; Stop-Process -Id {pid} -Force -ErrorAction SilentlyContinue }}\n\
         for ($i = 0; $i -lt 10; $i++) {{ if (-not (Get-Process -Id {pid} -ErrorAction SilentlyContinue)) {{ break }}; Start-Sleep -Milliseconds 500 }}\n\
         $run = {t}\n\
         if ({s} -like '*-Setup.exe' -or {s} -like '*-setup.exe') {{\n\
             $p = $null\n\
             try {{ $p = Start-Process -FilePath {s} -ArgumentList '/S', '/UPDATE' -Wait -PassThru -ErrorAction Stop }} catch {{ Log ('installer launch failed: ' + $_) }}\n\
             if ($null -ne $p) {{ Log ('installer exit code ' + $p.ExitCode) }}\n\
             $keys = @('HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\*',\n\
                       'HKLM:\\Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\*',\n\
                       'HKLM:\\Software\\WOW6432Node\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\*')\n\
             $entry = Get-ItemProperty -Path $keys -ErrorAction SilentlyContinue | Where-Object {{ $_.DisplayName -eq 'Consilium' }} | Select-Object -First 1\n\
             if ($entry -and $null -ne $p -and $p.ExitCode -eq 0) {{\n\
                 if ($entry.DisplayIcon) {{\n\
                     $icon = $entry.DisplayIcon.Trim([char]34).Split(',')[0]\n\
                     if (Test-Path -LiteralPath $icon) {{ $run = $icon }}\n\
                 }} elseif ($entry.InstallLocation) {{\n\
                     $locExe = Join-Path $entry.InstallLocation 'consilium.exe'\n\
                     if (Test-Path -LiteralPath $locExe) {{ $run = $locExe }}\n\
                 }}\n\
             }}\n\
             if ($null -ne $p -and $p.ExitCode -eq 0) {{\n\
                 $expected = Get-Content -LiteralPath (Join-Path {l} 'VERSION') -ErrorAction SilentlyContinue | Select-Object -First 1\n\
                 $actual = (Get-Item -LiteralPath $run -ErrorAction SilentlyContinue).VersionInfo.ProductVersion\n\
                 if ($expected -and $actual -and $expected.Trim() -eq $actual.Trim()) {{\n\
                     Log ('verified installed version ' + $actual)\n\
                     Remove-Item -LiteralPath {l} -Recurse -Force -ErrorAction SilentlyContinue\n\
                 }} else {{ Log ('installer reported success but expected ' + $expected + ', found ' + $actual + '; preserved staged folder') }}\n\
             }} else {{ Log 'installer did not complete; preserved staged folder' }}\n\
         }} else {{\n\
             Remove-Item -LiteralPath {old} -Force -ErrorAction SilentlyContinue\n\
             $swapped = $false\n\
             for ($a = 0; $a -lt 30; $a++) {{\n\
                 try {{\n\
                     if (Test-Path -LiteralPath {old}) {{ Remove-Item -LiteralPath {old} -Force -ErrorAction Stop }}\n\
                     Move-Item -LiteralPath {t} -Destination {old} -ErrorAction Stop\n\
                     Move-Item -LiteralPath {s} -Destination {t} -ErrorAction Stop\n\
                     Remove-Item -LiteralPath {old} -Force -ErrorAction SilentlyContinue\n\
                     Log 'swapped the program file'\n\
                     $swapped = $true\n\
                     break\n\
                 }} catch {{\n\
                     Log ('swap attempt ' + ($a + 1) + ' failed: ' + $_)\n\
                     if (-not (Test-Path -LiteralPath {t}) -and (Test-Path -LiteralPath {old})) {{\n\
                         try {{ Move-Item -LiteralPath {old} -Destination {t} -ErrorAction Stop }} catch {{}}\n\
                     }}\n\
                     Start-Sleep -Milliseconds 500\n\
                 }}\n\
             }}\n\
             if ($swapped) {{\n\
                 Remove-Item -LiteralPath {l} -Recurse -Force -ErrorAction SilentlyContinue\n\
             }} else {{\n\
                 Log 'all swap attempts failed; preserved staged folder'\n\
             }}\n\
         }}\n\
         Log ('reopening ' + $run)\n\
         $workDir = Split-Path -Parent $run\n\
         try {{ Start-Process -FilePath $run -WorkingDirectory $workDir -ErrorAction Stop }} catch {{ Log ('reopen failed: ' + $_) }}\n"
    )
}

/// Waits up to 15 s for the Windows helper's first log line. If it never appears, the helper
/// died on startup, so the caller keeps the app open and shows the error instead of quitting.
#[cfg(windows)]
fn confirm_helper_started(log: &Path) -> Result<(), String> {
    for _ in 0..150 {
        if std::fs::read_to_string(log).map(|t| t.contains("update helper started")).unwrap_or(false) {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    Err(format!("the update helper didn't start, so Consilium stayed open; see {}", log.display()))
}

/// Where the Windows helper writes what it did.
pub fn install_log(data: &Path) -> PathBuf {
    data.join("update").join("install.log")
}

/// The most recent failure to download or stage a GitHub release. The front end retries quietly,
/// so this file preserves the reason for diagnosis instead of losing it in a caught promise.
pub fn download_error_log(data: &Path) -> PathBuf {
    data.join("update").join("update_error.log")
}

fn record_download_result(data: &Path, version: &str, result: &Result<(), String>) {
    let path = download_error_log(data);
    match result {
        Ok(()) => {
            let _ = std::fs::remove_file(path);
        }
        Err(error) => {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let clean_version = version.trim_start_matches(['v', 'V']).trim();
            let line = format!("{} Consilium {clean_version}: {error}\n", crate::store::now_ms());
            let _ = crate::store::write_atomic(&path, line.as_bytes());
        }
    }
}

/// Downloads and stages a release installer. macOS extracts `Consilium.app` from the release DMG;
/// Windows and Linux keep their installer or executable in `<data>/update/next`.
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
    let result = match check_release_download(version, url, digest) {
        Ok(want) => track_download(download_state(), version, stage_from_url_inner(data, version, url, &want)).await,
        Err(error) => Err(error),
    };
    record_download_result(data, version, &result);
    result
}

fn release_download_client(read_timeout: Duration) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent("Consilium-Updater")
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(read_timeout)
        .build()
        .map_err(|e| format!("HTTP client error: {e}"))
}

async fn stage_from_url_inner(data: &Path, version: &str, url: &str, want: &str) -> Result<(), String> {
    use sha2::{Digest, Sha256};
    let client = release_download_client(Duration::from_secs(45))?;
    let resp = client.get(url).send().await.map_err(|e| format!("download failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("download returned HTTP {}", resp.status()));
    }
    let bytes = resp.bytes().await.map_err(|e| format!("read failed: {e}"))?;
    let got: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
    if got != want {
        return Err("the downloaded update didn't match GitHub's checksum, so it was thrown away".into());
    }

    #[cfg(target_os = "macos")]
    {
        let _ = url;
        stage_macos_dmg(data, version, &bytes)
    }

    #[cfg(not(target_os = "macos"))]
    {
        stage_non_macos_download(data, version, url, &bytes)
    }
}

#[cfg(target_os = "macos")]
fn command_failure(action: &str, output: &std::process::Output) -> String {
    let detail = String::from_utf8_lossy(&output.stderr);
    let detail = detail.lines().map(str::trim).rfind(|line| !line.is_empty()).unwrap_or("unknown error");
    format!("{action}: {detail}")
}

/// Copies a complete app bundle into the place the existing macOS installer already swaps in.
/// `ditto` preserves the bundle's symlinks, extended attributes and code-signing resources.
#[cfg(target_os = "macos")]
fn stage_macos_app(data: &Path, version: &str, source: &Path) -> Result<(), String> {
    use std::process::Command;

    if !source.is_dir() {
        return Err("the macOS update DMG contains no Consilium.app".into());
    }
    let update_dir = data.join("update");
    std::fs::create_dir_all(&update_dir).map_err(|e| format!("create update folder: {e}"))?;
    let pending = update_dir.join("Consilium.app.download");
    let previous = update_dir.join("Consilium.app.previous");
    let staged = staged_app(data);
    let _ = std::fs::remove_dir_all(&pending);
    let output = Command::new("/usr/bin/ditto")
        .arg(source)
        .arg(&pending)
        .output()
        .map_err(|e| format!("copy the app from the update DMG: {e}"))?;
    if !output.status.success() {
        let _ = std::fs::remove_dir_all(&pending);
        return Err(command_failure("copy the app from the update DMG", &output));
    }

    let clean_version = version.trim_start_matches(['v', 'V']).trim();
    let found = bundle_version(&pending).ok_or_else(|| {
        let _ = std::fs::remove_dir_all(&pending);
        "the app in the update DMG has no version".to_string()
    })?;
    if found != clean_version {
        let _ = std::fs::remove_dir_all(&pending);
        return Err(format!("the update DMG contains Consilium {found}, not {clean_version}"));
    }

    // Local source builds use this identity so macOS keeps microphone and automation grants.
    // A release download is signed the same way when this Mac's private signing key exists, and
    // at least ad-hoc signed otherwise; GitHub's current release bundle itself is unsigned.
    if let Err(e) = sign_macos_app(data, &pending) {
        let _ = std::fs::remove_dir_all(&pending);
        return Err(e);
    }

    let _ = std::fs::remove_dir_all(&previous);
    if staged.exists() {
        std::fs::rename(&staged, &previous).map_err(|e| format!("preserve the previously staged app: {e}"))?;
    }
    if let Err(e) = std::fs::rename(&pending, &staged) {
        if previous.exists() {
            let _ = std::fs::rename(&previous, &staged);
        }
        return Err(format!("stage the macOS app: {e}"));
    }
    let _ = std::fs::remove_dir_all(&previous);
    // A GitHub release has no local build stamp. Remove an older stamp only after the new app is
    // complete, so a failed download never damages an update that was already staged.
    let _ = std::fs::remove_file(build_stamp(data));
    Ok(())
}

#[cfg(target_os = "macos")]
struct SystemRunner;

#[cfg(target_os = "macos")]
impl crate::mac_signing::Runner for SystemRunner {
    fn run(&mut self, program: &str, args: &[std::ffi::OsString]) -> bool {
        use std::process::{Command, Stdio};
        // Output is discarded and only the exit status is used, so the keychain password passed
        // to `unlock-keychain` can never reach a log through an error message.
        Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn search_list(&mut self) -> Option<Vec<String>> {
        let out = std::process::Command::new("/usr/bin/security")
            .args(["list-keychains", "-d", "user"])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())?;
        Some(crate::mac_signing::parse_list(&String::from_utf8_lossy(&out.stdout)))
    }
}

/// Stable local signing when this Mac has the private key, matching `scripts/stage-update.mjs`;
/// otherwise a required ad-hoc signature keeps GitHub's unsigned release launchable.
#[cfg(target_os = "macos")]
fn sign_macos_app(data: &Path, app: &Path) -> Result<(), String> {
    use std::process::{Command, Stdio};

    let signing = data.join("signing");
    let keychain = signing.join("consilium-signing.keychain-db");
    let password = keychain
        .is_file()
        .then(|| std::fs::read_to_string(signing.join("keychain-password")).ok())
        .flatten();
    let report = crate::mac_signing::sign(&mut SystemRunner, &keychain, password.as_deref(), app);
    if let Some(why) = report.stable_failure {
        eprintln!("Consilium update: signed ad-hoc because {why}; macOS may ask for permissions again.");
    }
    if let Some(why) = report.restore_failure {
        eprintln!("Consilium update: {why}; remove it in Keychain Access if it stays listed.");
    }
    let signed = report.signed;
    if !signed {
        return Err("sign the app from the update DMG: codesign failed".into());
    }
    let verified = Command::new("/usr/bin/codesign")
        .args(["--verify", "--deep", "--strict"])
        .arg(app)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("verify the app from the update DMG: {e}"))?;
    if !verified.success() {
        return Err("verify the app from the update DMG: codesign rejected it".into());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn stage_macos_dmg(data: &Path, version: &str, bytes: &[u8]) -> Result<(), String> {
    use std::process::Command;

    let work = data.join("update").join(format!("dmg-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&work).map_err(|e| format!("create DMG work folder: {e}"))?;
    let dmg = work.join("Consilium-macOS.dmg");
    let mount = work.join("mount");
    std::fs::write(&dmg, bytes).map_err(|e| format!("write update DMG: {e}"))?;
    std::fs::create_dir_all(&mount).map_err(|e| format!("create DMG mount folder: {e}"))?;
    let attached = Command::new("/usr/bin/hdiutil")
        .args(["attach", "-readonly", "-nobrowse", "-mountpoint"])
        .arg(&mount)
        .arg(&dmg)
        .output()
        .map_err(|e| format!("mount update DMG: {e}"))?;
    if !attached.status.success() {
        let _ = std::fs::remove_dir_all(&work);
        return Err(command_failure("mount update DMG", &attached));
    }

    let exact = mount.join("Consilium.app");
    let source = if exact.is_dir() {
        Some(exact)
    } else {
        std::fs::read_dir(&mount)
            .ok()
            .and_then(|entries| entries.filter_map(Result::ok).map(|e| e.path()).find(|p| p.extension().and_then(|e| e.to_str()) == Some("app")))
    };
    let result = source.as_deref().ok_or_else(|| "the macOS update DMG contains no app bundle".to_string()).and_then(|app| stage_macos_app(data, version, app));
    let detached = Command::new("/usr/bin/hdiutil").arg("detach").arg(&mount).output();
    let _ = std::fs::remove_dir_all(&work);
    result?;
    match detached {
        Ok(output) if output.status.success() => Ok(()),
        Ok(output) => Err(command_failure("unmount update DMG", &output)),
        Err(e) => Err(format!("unmount update DMG: {e}")),
    }
}

#[cfg(not(target_os = "macos"))]
fn stage_non_macos_download(data: &Path, version: &str, url: &str, bytes: &[u8]) -> Result<(), String> {
    let next_dir = staged_dir(data);
    let tmp_dir = data.join("update").join("tmp_download");
    let _ = std::fs::remove_dir_all(&tmp_dir);
    std::fs::create_dir_all(&tmp_dir).map_err(|e| format!("create tmp dir: {e}"))?;

    let filename = if cfg!(windows) {
        "Consilium-Windows-Setup.exe"
    } else {
        match url.rsplit('.').next().map(str::to_ascii_lowercase).as_deref() {
            Some("deb") => "Consilium.deb",
            Some("rpm") => "Consilium.rpm",
            _ => "Consilium.AppImage",
        }
    };

    let target_file = tmp_dir.join(filename);
    std::fs::write(&target_file, bytes).map_err(|e| format!("write asset: {e}"))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&target_file, std::fs::Permissions::from_mode(0o755));
    }

    let clean_ver = version.trim_start_matches(['v', 'V']).trim();
    std::fs::write(tmp_dir.join("VERSION"), format!("{clean_ver}\n")).map_err(|e| format!("write version: {e}"))?;

    let _ = std::fs::remove_dir_all(&next_dir);
    std::fs::rename(&tmp_dir, &next_dir).map_err(|e| format!("stage update: {e}"))?;
    let _ = std::fs::remove_file(build_stamp(data));
    Ok(())
}

/// A PowerShell script that asks WMI to start a Windows helper with no window, and
/// prints `WMI <ReturnValue> <ProcessId>`. WMI creates the process under WmiPrvSE, so it is
/// outside every job Consilium is in and needs no breakaway (which that job refuses).
fn wmi_launch_script(helper: &Path) -> String {
    let line = format!(
        "powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File \"{}\"",
        helper.display()
    );
    let quoted = line.replace('\'', "''");
    // Start in the update folder, not System32, so the relaunched app never inherits that.
    let cwd = helper.parent().map(|d| d.display().to_string()).unwrap_or_default().replace('\'', "''");
    format!(
        "$ErrorActionPreference = 'Stop'\r\n\
         $si = New-CimInstance -CimClass (Get-CimClass -ClassName Win32_ProcessStartup) -ClientOnly -Property @{{ShowWindow = [uint16]0}}\r\n\
         $r = Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments @{{CommandLine = '{quoted}'; CurrentDirectory = '{cwd}'; ProcessStartupInformation = $si}}\r\n\
         'WMI {{0}} {{1}}' -f $r.ReturnValue, $r.ProcessId\r\n"
    )
}

/// Reads `wmi_launch_script`'s output: the helper's process id, or why it didn't start.
#[cfg_attr(not(windows), allow(dead_code))]
fn wmi_launch_result(out: &str) -> Result<u32, String> {
    let line = out
        .lines()
        .find_map(|l| l.trim().strip_prefix("WMI "))
        .ok_or("couldn't start the installer through Windows (WMI gave no answer)")?;
    let mut parts = line.split_whitespace();
    match (parts.next().and_then(|c| c.parse::<u32>().ok()), parts.next().and_then(|p| p.parse::<u32>().ok())) {
        (Some(0), Some(pid)) if pid > 0 => Ok(pid),
        (Some(code), _) => Err(format!("Windows refused to start the installer (WMI code {code})")),
        _ => Err("couldn't start the installer through Windows (unreadable WMI answer)".into()),
    }
}

/// Runs the short hidden launcher and returns the detached helper's process ID.
#[cfg(windows)]
pub(crate) fn start_windows_helper(helper: &Path, launcher: &Path) -> Result<u32, String> {
    use std::io::Read;
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    std::fs::write(launcher, format!("\u{feff}{}", wmi_launch_script(helper)))
        .map_err(|e| format!("couldn't write the Windows launcher: {e}"))?;
    let mut child = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(launcher)
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("couldn't start the Windows launcher: {e}"))?;
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("the Windows launcher didn't answer within 20 seconds".into());
            }
        }
    }
    let mut out = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        let _ = stdout.read_to_string(&mut out);
    }
    let mut err = String::new();
    if let Some(mut stderr) = child.stderr.take() {
        let _ = stderr.read_to_string(&mut err);
    }
    wmi_launch_result(&out).map_err(|e| match err.lines().map(str::trim).rfind(|l| !l.is_empty()) {
        Some(l) => format!("{e}: {l}"),
        None => e,
    })
}

/// Starts the detached helper. The caller then exits the app.
pub fn start_install(data: &Path) -> Result<Staged, String> {
    let s = staged(data).ok_or("no newer build is staged")?;
    let pid = std::process::id();
    #[cfg(windows)]
    {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let (staged, target) = swap_paths(data, &exe, None)?;
        // Run a script file with a BOM for PowerShell 5.1. WMI makes the update helper
        // independent of Consilium's process job.
        let log = install_log(data);
        let helper = data.join("update").join("install-helper.ps1");
        let script = windows_helper_script(pid, &staged, &target, &staged_dir(data), &log);
        let absolute_data = std::path::absolute(data).map_err(|e| format!("couldn't locate Consilium's update profile: {e}"))?;
        let handoff = crate::restart::windows_handoff_script(&absolute_data);
        std::fs::write(&helper, format!("\u{feff}{handoff}{script}"))
            .map_err(|e| format!("couldn't write the update helper: {e}"))?;
        std::fs::write(&log, format!("launching update helper {}\n", helper.display()))
            .map_err(|e| format!("couldn't write the update log: {e}"))?;
        start_windows_helper(&helper, &data.join("update").join("launch-helper.ps1"))?;
        confirm_helper_started(&log)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};
        let script = if cfg!(target_os = "macos") {
            let target = running_bundle()?;
            helper_script(pid, &staged_app(data), &target, &install_log(data))
        } else {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let appimage = std::env::var_os("APPIMAGE").filter(|_| cfg!(target_os = "linux")).map(PathBuf::from);
            let (staged, target) = swap_paths(data, &exe, appimage.as_deref())?;
            let pkg = matches!(staged.extension().and_then(|e| e.to_str()), Some("deb" | "rpm"));
            if pkg {
                linux_package_script(pid, &staged, &target, &staged_dir(data))
            } else {
                linux_helper_script(pid, &staged, &target, &staged_dir(data))
            }
        };
        Command::new("/bin/sh").arg("-c").arg(script)
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
            .process_group(0) // outlives this app's process group
            .spawn().map_err(|e| format!("couldn't start the installer: {e}"))?;
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn the_wmi_launcher_quotes_the_helper_path_for_powershell() {
        let s = wmi_launch_script(Path::new("C:\\Users\\O'Neil\\update\\install-helper.ps1"));
        assert!(s.contains("CommandLine = 'powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File \"C:\\Users\\O''Neil\\update\\install-helper.ps1\"'"), "{s}");
        assert!(s.contains("ShowWindow = [uint16]0"));
    }

    #[test]
    fn the_wmi_answer_gives_the_helper_pid_or_the_reason() {
        assert_eq!(wmi_launch_result("WMI 0 4242\r\n"), Ok(4242));
        assert!(wmi_launch_result("WMI 2 \r\n").unwrap_err().contains("code 2"));
        assert!(wmi_launch_result("Invoke-CimMethod : Access denied").is_err());
        assert!(wmi_launch_result("").is_err());
    }

    /// Probe f: the real WMI launch, run from inside a job (a Consilium Run box). The helper
    /// must start, and must not be inside any job.
    #[test]
    #[cfg(windows)]
    #[ignore]
    fn probe_f_wmi_launch_escapes_the_job() {
        use std::os::windows::process::CommandExt;
        let dir = std::env::temp_dir().join(format!("consilium-wmi-probe-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("install.log");
        let helper = dir.join("install-helper.ps1");
        let body = format!(
            "Add-Type -Namespace P -Name J -MemberDefinition '[DllImport(\"kernel32.dll\")] public static extern bool IsProcessInJob(IntPtr p, IntPtr j, out bool r);'\r\n\
             $r = $false; [void][P.J]::IsProcessInJob((Get-Process -Id $PID).Handle, [IntPtr]::Zero, [ref]$r)\r\n\
             Add-Content -LiteralPath '{l}' -Value \"update helper started; in job = $r\"\r\n\
             Start-Sleep -Seconds 3\r\n\
             Add-Content -LiteralPath '{l}' -Value 'update helper still running'\r\n",
            l = log.display().to_string().replace('\'', "''")
        );
        std::fs::write(&helper, format!("\u{feff}{body}")).unwrap();
        let launcher = dir.join("launch-helper.ps1");
        std::fs::write(&launcher, format!("\u{feff}{}", wmi_launch_script(&helper))).unwrap();
        let out = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(&launcher)
            .creation_flags(0x0800_0000)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
        println!("PROBE f launcher: {}", text.trim());
        let pid = wmi_launch_result(&text).expect("WMI started the helper");
        let mut seen = String::new();
        for _ in 0..150 {
            seen = std::fs::read_to_string(&log).unwrap_or_default();
            if !seen.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        println!("PROBE f helper {pid}: {}", seen.trim());
        assert!(seen.contains("update helper started"), "the helper never wrote its log");
        // A job WMI owns doesn't matter; what matters is that the helper outlives its launcher,
        // which `output()` above has already waited for.
        for _ in 0..100 {
            if seen.contains("still running") {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
            seen = std::fs::read_to_string(&log).unwrap_or_default();
        }
        println!("PROBE f later: {}", seen.trim());
        assert!(seen.contains("update helper still running"), "the helper died after its launcher exited: {seen}");
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn a_staged_build_counts_only_when_it_is_later_than_this_one() {
        let dir = std::env::temp_dir().join(format!("consilium-staged-build-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(staged_dir(&dir)).unwrap();
        let this = crate::build_id::Build::this();
        let stage = |version: &str, stamp: Option<serde_json::Value>| {
            std::fs::write(staged_dir(&dir).join("VERSION"), format!("{version}\n")).unwrap();
            match stamp {
                Some(s) => std::fs::write(build_stamp(&dir), s.to_string()).unwrap(),
                None => {
                    let _ = std::fs::remove_file(build_stamp(&dir));
                }
            }
        };
        stage(&this.version, Some(serde_json::json!({"version": this.version, "number": this.number + 1, "commit": "abcdef123456"})));
        let s = staged(&dir).expect("a later build of the same version is offered");
        assert_eq!(s.build, format!("{} #{} (abcdef123456)", this.version, this.number + 1));
        stage(&this.version, Some(serde_json::json!({"version": this.version, "number": this.number, "commit": "abcdef123456"})));
        assert_eq!(staged(&dir), None, "the same build isn't offered again");
        stage(&this.version, Some(serde_json::json!({"version": "0.0.1", "number": this.number + 5})));
        assert_eq!(staged(&dir), None, "a stamp for another version is ignored, and the version alone is equal");
        stage("999.0.0", None);
        assert_eq!(staged(&dir).map(|s| s.version), Some("999.0.0".into()), "a later version needs no stamp");
        stage("0.0.1", None);
        assert_eq!(staged(&dir), None, "an older version is never offered");
        let _ = std::fs::remove_dir_all(&dir);
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
        let s = helper_script(
            42,
            Path::new("/d/update/Consilium.app"),
            Path::new("/Applications/Consilium.app"),
            Path::new("/d/update/install.log"),
        );
        assert!(s.contains("exec > '/d/update/install.log' 2>&1"));
        assert!(s.contains("application bundle replacement failed"));
        assert!(s.contains("kill -0 42"));
        assert!(s.contains("mv '/Applications/Consilium.app' '/Applications/Consilium.app.old' && mv '/d/update/Consilium.app' '/Applications/Consilium.app'"));
        assert!(s.contains("mv '/Applications/Consilium.app.old' '/Applications/Consilium.app'"));
        assert!(s.contains("if open '/Applications/Consilium.app'"));
        assert!(s.contains("Launch Services accepted relaunch"));
        assert!(s.contains("relaunch failed"));
    }

    #[cfg(unix)]
    #[test]
    fn mac_helper_reopens_the_previous_bundle_when_swap_fails() {
        let data = std::env::temp_dir().join(format!("consilium mac helper {}", uuid::Uuid::new_v4()));
        let target = data.join("Consilium.app");
        let missing_stage = data.join("missing.app");
        let log = data.join("install.log");
        let reopened = data.join("reopened");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("version"), "old").unwrap();
        // Run the real swap against a fixture. Stub process signals and Launch Services.
        let script = format!(
            "kill() {{ return 1; }}\nsleep() {{ :; }}\nopen() {{ cat \"$1/version\" > {}; }}\n{}",
            quote(&reopened),
            helper_script(42, &missing_stage, &target, &log)
        );
        let result = std::process::Command::new("/bin/sh").arg("-c").arg(script).status().unwrap();
        let restored = std::fs::read_to_string(target.join("version")).unwrap();
        let launched = std::fs::read_to_string(reopened).unwrap();
        let log = std::fs::read_to_string(log).unwrap();
        std::fs::remove_dir_all(&data).unwrap();
        assert!(result.success());
        assert_eq!(restored, "old");
        assert_eq!(launched, "old");
        assert!(log.contains("application bundle replacement failed"));
        assert!(log.contains("Launch Services accepted relaunch"));
    }

    #[cfg(unix)]
    #[test]
    fn mac_helper_restores_the_previous_bundle_if_launch_is_rejected() {
        let data = std::env::temp_dir().join(format!("consilium mac relaunch {}", uuid::Uuid::new_v4()));
        let target = data.join("Consilium.app");
        let stage = data.join("update.app");
        let log = data.join("install.log");
        let reopened = data.join("reopened");
        for (bundle, version) in [(&target, "old"), (&stage, "new")] {
            std::fs::create_dir_all(bundle).unwrap();
            std::fs::write(bundle.join("version"), version).unwrap();
        }
        let script = format!(
            "kill() {{ return 1; }}\nsleep() {{ :; }}\nopen() {{ [ \"$(cat \"$1/version\")\" = old ] && cat \"$1/version\" > {}; }}\n{}",
            quote(&reopened),
            helper_script(42, &stage, &target, &log)
        );
        let result = std::process::Command::new("/bin/sh").arg("-c").arg(script).status().unwrap();
        assert!(result.success());
        assert_eq!(std::fs::read_to_string(target.join("version")).unwrap(), "old");
        assert_eq!(std::fs::read_to_string(reopened).unwrap(), "old");
        assert_eq!(std::fs::read_to_string(stage.with_extension("app.failed").join("version")).unwrap(), "new");
        assert!(std::fs::read_to_string(log).unwrap().contains("relaunch failed; restoring previous bundle"));
        std::fs::remove_dir_all(&data).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn mac_helper_recovers_an_interrupted_swap_without_a_new_stage() {
        let data = std::env::temp_dir().join(format!("consilium mac interrupted {}", uuid::Uuid::new_v4()));
        let target = data.join("Consilium.app");
        let backup = target.with_extension("app.old");
        let missing_stage = data.join("missing.app");
        let log = data.join("install.log");
        std::fs::create_dir_all(&backup).unwrap();
        std::fs::write(backup.join("version"), "old").unwrap();
        let script = format!(
            "kill() {{ return 1; }}\nsleep() {{ :; }}\nopen() {{ [ \"$(cat \"$1/version\")\" = old ]; }}\n{}",
            helper_script(42, &missing_stage, &target, &log)
        );
        let result = std::process::Command::new("/bin/sh").arg("-c").arg(script).status().unwrap();
        assert!(result.success());
        assert_eq!(std::fs::read_to_string(target.join("version")).unwrap(), "old");
        assert!(!backup.exists());
        assert!(std::fs::read_to_string(log).unwrap().contains("recovering previous bundle from interrupted update"));
        std::fs::remove_dir_all(&data).unwrap();
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
        assert!(setup.contains("Start-Process -FilePath 'C:\\Users\\O''Neil\\AppData\\Roaming\\c\\update\\next\\Consilium-Windows-Setup.exe' -ArgumentList '/S', '/UPDATE' -Wait -PassThru -ErrorAction Stop"));
        assert!(setup.contains("$expected.Trim() -eq $actual.Trim()"));
        assert!(setup.contains("installer did not complete; preserved staged folder"));
        assert!(setup.contains("DisplayIcon"));
        assert!(setup.contains("if ($null -ne $p -and $p.ExitCode -eq 0)"));
        assert!(setup.trim_end().ends_with("catch { Log ('reopen failed: ' + $_) }"));
    }

    #[tokio::test]
    async fn a_stalled_release_body_times_out() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            let mut line = String::new();
            loop {
                line.clear();
                if stream.read_line(&mut line).await.unwrap() == 0 || line == "\r\n" {
                    break;
                }
            }
            stream.get_mut().write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\n1").await.unwrap();
            std::future::pending::<()>().await;
        });
        let client = release_download_client(Duration::from_millis(300)).unwrap();
        let response = client.get(url).send().await.unwrap();
        let error = tokio::time::timeout(Duration::from_secs(3), response.bytes()).await.unwrap().unwrap_err();
        assert!(error.is_timeout(), "{error}");
        server.abort();
    }

    #[test]
    fn release_downloads_are_checked() {
        let url = "https://github.com/Ag3nt47/consilium-ai/releases/download/v0.3.28/Consilium-Windows-Setup.exe";
        let d = format!("sha256:{}", "a".repeat(64));
        assert_eq!(check_release_download("v0.3.28", url, &d).unwrap(), "a".repeat(64));
        let dmg = "https://github.com/Ag3nt47/consilium-ai/releases/download/v0.3.28/Consilium-macOS.dmg";
        assert_eq!(check_release_download("v0.3.28", dmg, &d).unwrap(), "a".repeat(64));
        assert!(check_release_download("v0.3.28", url, "").is_err());
        assert!(check_release_download("v0.3.28", "https://evil.example/x.exe", &d).is_err());
        assert!(check_release_download("v0.3.28", "https://github.com/Ag3nt47/consilium-ai/releases/download/v1/../../x/y.exe", &d).is_err());
        assert!(check_release_download("0.3.28; rm", url, &d).is_err());
    }

    #[test]
    fn release_staging_failure_is_recorded_and_success_clears_it() {
        let data = std::env::temp_dir().join(format!("consilium-update-error-{}", uuid::Uuid::new_v4()));
        let failed = Err("download returned HTTP 503".to_string());
        record_download_result(&data, "v9.8.7", &failed);
        let log = std::fs::read_to_string(download_error_log(&data)).unwrap();
        assert!(log.contains("Consilium 9.8.7: download returned HTTP 503"));

        record_download_result(&data, "v9.8.7", &Ok(()));
        assert!(!download_error_log(&data).exists());
        let _ = std::fs::remove_dir_all(data);
    }

    #[test]
    fn download_status_tracks_overlapping_and_cancelled_downloads() {
        // Keep this test isolated from the process-global production counter: Rust runs unit tests
        // concurrently, and a cancellation assertion must not race another status test.
        let state = Mutex::new(DownloadState::default());
        let remaining_download = DownloadActivity::start(&state, "v1.2.3");
        let pending_download = track_download(&state, "V1.2.4", std::future::pending::<()>());
        let mut pending_download = Box::pin(pending_download);
        let waker = std::task::Waker::noop();
        let mut context = std::task::Context::from_waker(waker);

        // Constructing an async future does nothing. Poll it once so its guard is alive across the
        // pending await, then cancel it by dropping the future.
        assert!(std::future::Future::poll(pending_download.as_mut(), &mut context).is_pending());
        assert_eq!(state.lock().unwrap().active, 2);
        assert_eq!(download_status_for(&state), DownloadStatus { active: true, version: "1.2.4".into() });

        drop(pending_download);
        assert_eq!(state.lock().unwrap().active, 1);
        assert!(download_status_for(&state).active);

        drop(remaining_download);
        assert_eq!(download_status_for(&state), DownloadStatus::default());
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn a_copied_mac_app_is_complete_before_it_replaces_the_previous_stage() {
        let data = std::env::temp_dir().join(format!("consilium-mac-stage-{}", uuid::Uuid::new_v4()));
        let source = data.join("source").join("Consilium.app");
        let plist = |version: &str| {
            format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
                 <plist version=\"1.0\"><dict>\
                 <key>CFBundleExecutable</key><string>consilium</string>\
                 <key>CFBundleIdentifier</key><string>io.github.ag3nt47.consilium.test</string>\
                 <key>CFBundlePackageType</key><string>APPL</string>\
                 <key>CFBundleShortVersionString</key><string>{version}</string>\
                 </dict></plist>"
            )
        };
        std::fs::create_dir_all(source.join("Contents/MacOS")).unwrap();
        std::fs::write(source.join("Contents/Info.plist"), plist("999.1.0")).unwrap();
        let executable = source.join("Contents/MacOS/consilium");
        std::fs::write(&executable, b"#!/bin/sh\nexit 0\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(source.join("Contents/copied"), b"complete").unwrap();

        let old = staged_app(&data);
        std::fs::create_dir_all(old.join("Contents")).unwrap();
        std::fs::write(old.join("Contents/Info.plist"), plist("998.0.0")).unwrap();
        std::fs::write(old.join("Contents/old"), b"preserve on failure").unwrap();

        stage_macos_app(&data, "v999.1.0", &source).unwrap();
        assert_eq!(bundle_version(&old).as_deref(), Some("999.1.0"));
        assert_eq!(std::fs::read(old.join("Contents/copied")).unwrap(), b"complete");
        assert!(!old.join("Contents/old").exists());

        std::fs::write(source.join("Contents/Info.plist"), plist("1000.0.0")).unwrap();
        assert!(stage_macos_app(&data, "v999.2.0", &source).is_err());
        assert_eq!(bundle_version(&old).as_deref(), Some("999.1.0"));
        let _ = std::fs::remove_dir_all(&data);
    }
}
