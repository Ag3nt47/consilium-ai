//! Optional, low-priority host scanning with Zero Breach Toolkit.
//!
//! The scanner is deliberately external: Consilium never silently downloads security software.
//! Once the person installs the pinned, reviewed scanner into the app data folder, this adapter
//! runs its read-only quick scan only after the machine has stayed idle for five minutes. A new AI
//! turn or heavy host pressure stops the scan and leaves it due for the next idle window.

use crate::engine::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const DEFAULT_INTERVAL_HOURS: u32 = 24 * 7;
const MIN_INTERVAL_HOURS: u32 = 24;
const MAX_INTERVAL_HOURS: u32 = 24 * 30;
const IDLE_TICKS_REQUIRED: u8 = 5;
const START_PRESSURE_LIMIT: u8 = 35;
const STOP_PRESSURE_LIMIT: u8 = 75;
/// Memory alone only blocks a scan near exhaustion: macOS and Windows keep most RAM in use as
/// cache, so an ordinary 8 GB Mac sits at 60-70% and a 35% combined gate would never open.
const MEMORY_LIMIT: u8 = 90;
const RETRY_DELAY_MS: u64 = 15 * 60 * 1_000;
/// GitHub release v3.0.2 standalone Python asset, reviewed and pinned on 2026-11-16.
const ZERO_BREACH_SHA256: &str = "0b6df5f02c6b13256b735bfb55c1ef0a31ff0c762550910be9f66b4f5090dafb";

fn default_auto_remove() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScanSettings {
    /// Enabled by default, but inert until the separately approved scanner is installed.
    pub enabled: bool,
    pub interval_hours: u32,
    #[serde(default = "default_auto_remove")]
    pub auto_remove_malicious_extensions: bool,
}

impl Default for ScanSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_hours: DEFAULT_INTERVAL_HOURS,
            auto_remove_malicious_extensions: true,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ScanRecord {
    last_started_ms: u64,
    last_finished_ms: u64,
    last_exit_code: Option<i32>,
    last_summary: String,
    last_report: Option<String>,
}

#[derive(Debug, Default)]
struct Runtime {
    running: bool,
    idle_ticks: u8,
    retry_after_ms: u64,
    paused_reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScanStatus {
    pub installed: bool,
    pub verified: bool,
    pub tool_path: String,
    pub enabled: bool,
    pub interval_hours: u32,
    pub auto_remove_malicious_extensions: bool,
    pub running: bool,
    pub last_started_ms: u64,
    pub last_finished_ms: u64,
    pub last_exit_code: Option<i32>,
    pub last_summary: String,
    pub last_report: Option<String>,
    pub paused_reason: String,
}

pub struct Scanner {
    root: PathBuf,
    settings_path: PathBuf,
    record_path: PathBuf,
    settings: Mutex<ScanSettings>,
    record: Mutex<ScanRecord>,
    runtime: Mutex<Runtime>,
}

impl Scanner {
    pub fn open(data: &Path) -> Arc<Self> {
        let root = data.join("security");
        let settings_path = root.join("scanner.json");
        let record_path = root.join("last-scan.json");
        let settings = read_json(&settings_path).unwrap_or_default();
        let record = read_json(&record_path).unwrap_or_default();
        Arc::new(Self {
            root,
            settings_path,
            record_path,
            settings: Mutex::new(settings),
            record: Mutex::new(record),
            runtime: Mutex::new(Runtime::default()),
        })
    }

    /// Expected path for the pinned, cross-platform standalone scanner. Keeping it inside the app
    /// data folder prevents PATH substitution and gives every operating system one layout.
    fn tool_path(&self) -> PathBuf {
        self.root.join("zero-breach/zero-breach-toolkit.py")
    }

    pub fn status(&self) -> ScanStatus {
        let tool = self.tool_path();
        let settings = self
            .settings
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let record = self
            .record
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let runtime = self.runtime.lock().unwrap_or_else(|p| p.into_inner());
        let installed = tool.is_file();
        let verified = installed && verify_tool(&tool).is_ok();
        ScanStatus {
            installed,
            verified,
            tool_path: tool.display().to_string(),
            enabled: settings.enabled,
            interval_hours: settings.interval_hours,
            auto_remove_malicious_extensions: settings.auto_remove_malicious_extensions,
            running: runtime.running,
            last_started_ms: record.last_started_ms,
            last_finished_ms: record.last_finished_ms,
            last_exit_code: record.last_exit_code,
            last_summary: record.last_summary,
            last_report: record.last_report,
            paused_reason: runtime.paused_reason.clone(),
        }
    }

    pub fn save_settings(&self, mut next: ScanSettings) -> Result<ScanStatus, String> {
        next.interval_hours = next
            .interval_hours
            .clamp(MIN_INTERVAL_HOURS, MAX_INTERVAL_HOURS);
        write_private_json(&self.settings_path, &next)?;
        *self.settings.lock().unwrap_or_else(|p| p.into_inner()) = next;
        Ok(self.status())
    }

    /// Schedules one immediate run, still respecting the idle/resource gate.
    pub fn request_run(&self) -> Result<ScanStatus, String> {
        let tool = self.tool_path();
        if !tool.is_file() {
            return Err(format!("Zero Breach isn't installed at {}", tool.display()));
        }
        verify_tool(&tool)?;
        let mut record = self.record.lock().unwrap_or_else(|p| p.into_inner());
        record.last_finished_ms = 0;
        record.last_summary = "Waiting for five idle minutes before the requested scan.".into();
        write_private_json(&self.record_path, &*record)?;
        let mut runtime = self.runtime.lock().unwrap_or_else(|p| p.into_inner());
        runtime.idle_ticks = 0;
        runtime.retry_after_ms = 0;
        runtime.paused_reason = "Waiting for the computer to be idle.".into();
        drop(runtime);
        drop(record);
        Ok(self.status())
    }

    pub async fn watch(self: Arc<Self>, engine: Arc<Engine>) {
        // Do not contend with application startup, resumed turns, or update checks.
        tokio::time::sleep(Duration::from_secs(60)).await;
        loop {
            self.tick(engine.clone());
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    }

    fn tick(self: &Arc<Self>, engine: Arc<Engine>) {
        let settings = self
            .settings
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let tool = self.tool_path();
        let now = crate::store::now_ms();
        let mut runtime = self.runtime.lock().unwrap_or_else(|p| p.into_inner());
        if runtime.running {
            return;
        }
        if !settings.enabled {
            runtime.idle_ticks = 0;
            runtime.paused_reason = "Background scans are off.".into();
            return;
        }
        if !tool.is_file() {
            runtime.idle_ticks = 0;
            runtime.paused_reason = "Waiting for the approved Zero Breach install.".into();
            return;
        }
        if let Err(error) = verify_tool(&tool) {
            runtime.idle_ticks = 0;
            runtime.paused_reason = error;
            return;
        }
        let record = self
            .record
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let interval_ms = u64::from(settings.interval_hours) * 60 * 60 * 1_000;
        if record.last_finished_ms > 0 && now.saturating_sub(record.last_finished_ms) < interval_ms
        {
            runtime.idle_ticks = 0;
            runtime.paused_reason = "Next scheduled scan is not due yet.".into();
            return;
        }
        if now < runtime.retry_after_ms {
            runtime.paused_reason = "A paused scan will retry after the computer settles.".into();
            return;
        }
        if !engine.running_ids().is_empty() {
            runtime.idle_ticks = 0;
            runtime.paused_reason = "Paused while an AI is working.".into();
            return;
        }
        let pressure = scan_load(&crate::pressure::sample());
        if pressure > START_PRESSURE_LIMIT {
            runtime.idle_ticks = 0;
            runtime.paused_reason = format!("Waiting for lower system load ({pressure}%).");
            return;
        }
        runtime.idle_ticks = runtime.idle_ticks.saturating_add(1);
        if runtime.idle_ticks < IDLE_TICKS_REQUIRED {
            runtime.paused_reason = format!(
                "Idle check {}/{}; the scan starts only after five quiet minutes.",
                runtime.idle_ticks, IDLE_TICKS_REQUIRED
            );
            return;
        }
        runtime.running = true;
        runtime.idle_ticks = 0;
        runtime.paused_reason.clear();
        drop(runtime);

        let scanner = self.clone();
        tauri::async_runtime::spawn(async move {
            let scanner_for_run = scanner.clone();
            let result =
                tokio::task::spawn_blocking(move || scanner_for_run.run(tool, engine)).await;
            if let Err(e) = result {
                scanner.finish(None, format!("Scanner task failed: {e}"), None);
            }
        });
    }

    fn run(&self, tool: PathBuf, engine: Arc<Engine>) {
        if let Err(error) = verify_tool(&tool) {
            self.finish(None, error, None);
            return;
        }
        let started = crate::store::now_ms();
        {
            let mut record = self.record.lock().unwrap_or_else(|p| p.into_inner());
            record.last_started_ms = started;
            record.last_summary = "Quick read-only scan is running.".into();
            record.last_exit_code = None;
            let _ = write_private_json(&self.record_path, &*record);
        }
        let reports = self.root.join("reports");
        if let Err(e) = private_dir(&reports) {
            self.finish(None, e, None);
            return;
        }
        let json = reports.join(format!("zero-breach-{started}.json"));
        let text = reports.join(format!("zero-breach-{started}.txt"));
        let log_path = reports.join(format!("zero-breach-{started}.log"));
        let log = match File::create(&log_path) {
            Ok(file) => file,
            Err(e) => {
                self.finish(
                    None,
                    format!("Couldn't create the private scan log: {e}"),
                    None,
                );
                return;
            }
        };
        set_private_file(&log_path);
        let mut command = command_for(&tool);
        let stdout = match log.try_clone() {
            Ok(file) => file,
            Err(e) => {
                self.finish(
                    None,
                    format!("Couldn't prepare the private scan log: {e}"),
                    None,
                );
                return;
            }
        };
        command
            .args(["--quick", "--no-menu", "--json"])
            .arg(&json)
            .args(["--report"])
            .arg(&text)
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(log));
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(e) => {
                self.finish(None, format!("Couldn't start Zero Breach: {e}"), None);
                return;
            }
        };
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    let code = status.code();
                    let mut summary = match code {
                        Some(0) => "No known indicators or evidence gaps were reported by this scan.".to_string(),
                        Some(2) => "Zero Breach reported one or more actionable matches. Review the private report.".to_string(),
                        Some(3) => "No actionable match was reported, but some evidence could not be checked.".to_string(),
                        Some(c) => return self.finish(Some(c), format!("Zero Breach ended with exit code {c}."), None),
                        None => "Zero Breach was interrupted before it returned a result.".to_string(),
                    };
                    set_private_file(&json);
                    set_private_file(&text);
                    let auto_remove = self
                        .settings
                        .lock()
                        .map(|s| s.auto_remove_malicious_extensions)
                        .unwrap_or(true);
                    if auto_remove && json.is_file() {
                        if let Ok(outcome) = auto_remove_malicious_extensions(&json) {
                            if !outcome.removed_ids.is_empty() {
                                summary = format!(
                                    "{} Automatically removed {} malicious browser extension(s): {}. Open chrome://extensions and confirm it's gone; reset your search engine if it was changed.",
                                    summary,
                                    outcome.removed_ids.len(),
                                    outcome.removed_ids.join(", ")
                                );
                            }
                            if !outcome.skipped_browsers.is_empty() {
                                summary = format!(
                                    "{} Removal of malicious extension(s) in {} deferred until the browser is closed.",
                                    summary,
                                    outcome.skipped_browsers.join(", ")
                                );
                            }
                        }
                    }
                    self.finish(code, summary, Some(text));
                    return;
                }
                Ok(None) => {}
                Err(e) => {
                    let _ = child.kill();
                    self.finish(None, format!("Couldn't monitor Zero Breach: {e}"), None);
                    return;
                }
            }
            let busy = !engine.running_ids().is_empty();
            let pressure = scan_load(&crate::pressure::sample());
            if busy || pressure >= STOP_PRESSURE_LIMIT {
                let _ = child.kill();
                let _ = child.wait();
                let why = if busy {
                    "Paused because AI work started; it will retry in the next idle window."
                        .to_string()
                } else {
                    format!(
                        "Paused because system load reached {pressure}%; it will retry when idle."
                    )
                };
                let mut runtime = self.runtime.lock().unwrap_or_else(|p| p.into_inner());
                runtime.running = false;
                runtime.retry_after_ms = crate::store::now_ms().saturating_add(RETRY_DELAY_MS);
                runtime.paused_reason = why.clone();
                drop(runtime);
                let mut record = self.record.lock().unwrap_or_else(|p| p.into_inner());
                record.last_summary = why;
                record.last_exit_code = None;
                let _ = write_private_json(&self.record_path, &*record);
                return;
            }
            std::thread::sleep(Duration::from_secs(5));
        }
    }

    fn finish(&self, code: Option<i32>, summary: String, report: Option<PathBuf>) {
        let mut record = self.record.lock().unwrap_or_else(|p| p.into_inner());
        record.last_finished_ms = crate::store::now_ms();
        record.last_exit_code = code;
        record.last_summary = summary;
        record.last_report = report.map(|p| p.display().to_string());
        let _ = write_private_json(&self.record_path, &*record);
        drop(record);
        let mut runtime = self.runtime.lock().unwrap_or_else(|p| p.into_inner());
        runtime.running = false;
        runtime.paused_reason.clear();
    }
}

/// Load figure the scan gates use: processor use, unless memory is close to running out.
fn scan_load(p: &crate::pressure::SystemPressure) -> u8 {
    if p.memory_percent >= MEMORY_LIMIT {
        p.memory_percent
    } else {
        p.cpu_percent
    }
}

fn command_for(tool: &Path) -> Command {
    if tool
        .extension()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.eq_ignore_ascii_case("py"))
    {
        let python = if cfg!(windows) {
            find_program("python.exe").or_else(|| find_program("python3.exe"))
        } else {
            find_program("python3").or_else(|| Some(PathBuf::from("/usr/bin/python3")))
        }
        .unwrap_or_else(|| PathBuf::from("python3"));
        let mut command = Command::new(python);
        command.args(["-X", "utf8"]).arg(tool);
        command
    } else {
        Command::new(tool)
    }
}

fn find_program(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|p| p.join(name))
            .find(|p| p.is_file())
    })
}

fn verify_tool(path: &Path) -> Result<(), String> {
    let bytes =
        fs::read(path).map_err(|e| format!("Couldn't read the Zero Breach scanner: {e}"))?;
    let digest = Sha256::digest(bytes);
    let actual: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    if actual == ZERO_BREACH_SHA256 {
        Ok(())
    } else {
        Err(
            "The Zero Breach scanner doesn't match the reviewed v3.0.2 SHA-256; it will not run."
                .into(),
        )
    }
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

fn write_private_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        private_dir(parent)?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(
        &tmp,
        serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    set_private_file(&tmp);
    crate::store::replace_file(&tmp, path).map_err(|e| e.to_string())?;
    set_private_file(path);
    Ok(())
}

fn private_dir(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn set_private_file(_path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(_path, fs::Permissions::from_mode(0o600));
    }
}

/// Extracts all valid Chromium extension IDs (32 chars in a..=p) from a Zero Breach report.
pub fn parse_malicious_extension_ids(report_json: &str) -> Vec<String> {
    let mut ids = std::collections::BTreeSet::new();
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(report_json) {
        if let Some(doors) = value.get("doors").and_then(|d| d.as_array()) {
            for door in doors {
                if door.get("door").and_then(|d| d.as_str()) == Some("browser:malicious-extension") {
                    if let Some(observed) = door.get("observed").and_then(|o| o.as_str()) {
                        for segment in observed.split('[') {
                            if let Some(candidate) = segment.split(']').next() {
                                if candidate.len() == 32 && candidate.chars().all(|c| ('a'..='p').contains(&c)) {
                                    ids.insert(candidate.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    ids.into_iter().collect()
}

pub fn chromium_user_data_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            let app_sup = PathBuf::from(home).join("Library/Application Support");
            roots.push(app_sup.join("Google/Chrome"));
            roots.push(app_sup.join("Google/Chrome Canary"));
            roots.push(app_sup.join("Chromium"));
            roots.push(app_sup.join("Microsoft Edge"));
            roots.push(app_sup.join("Microsoft Edge Canary"));
            roots.push(app_sup.join("Microsoft Edge Dev"));
            roots.push(app_sup.join("BraveSoftware/Brave-Browser"));
            roots.push(app_sup.join("Vivaldi"));
            roots.push(app_sup.join("Arc/User Data"));
        }
    }
    #[cfg(windows)]
    {
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            let local_dir = PathBuf::from(local);
            roots.push(local_dir.join("Google\\Chrome\\User Data"));
            roots.push(local_dir.join("Google\\Chrome SxS\\User Data"));
            roots.push(local_dir.join("Microsoft\\Edge\\User Data"));
            roots.push(local_dir.join("Microsoft\\Edge SxS\\User Data"));
            roots.push(local_dir.join("Microsoft\\Edge Dev\\User Data"));
            roots.push(local_dir.join("BraveSoftware\\Brave-Browser\\User Data"));
            roots.push(local_dir.join("Chromium\\User Data"));
            roots.push(local_dir.join("Vivaldi\\User Data"));
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(home) = std::env::var_os("HOME") {
            let cfg = PathBuf::from(home).join(".config");
            roots.push(cfg.join("google-chrome"));
            roots.push(cfg.join("google-chrome-beta"));
            roots.push(cfg.join("google-chrome-unstable"));
            roots.push(cfg.join("chromium"));
            roots.push(cfg.join("microsoft-edge"));
            roots.push(cfg.join("microsoft-edge-dev"));
            roots.push(cfg.join("BraveSoftware/Brave-Browser"));
            roots.push(cfg.join("vivaldi"));
        }
    }
    roots.retain(|r| r.is_dir());
    roots
}

pub fn remove_browser_extension_in_roots(roots: &[PathBuf], extension_id: &str) -> Vec<PathBuf> {
    if extension_id.len() != 32 || !extension_id.chars().all(|c| ('a'..='p').contains(&c)) {
        return Vec::new();
    }
    let mut removed = Vec::new();
    let subdirs = [
        "Extensions",
        "Sync Extension Settings",
        "Local Extension Settings",
        "Extension Rules",
        "Extension State",
    ];

    for root in roots {
        let entries = match fs::read_dir(root) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let profile_path = entry.path();
            if !profile_path.is_dir() {
                continue;
            }
            for sub in &subdirs {
                let target = profile_path.join(sub).join(extension_id);
                if target.is_dir() && fs::remove_dir_all(&target).is_ok() {
                    removed.push(target);
                }
            }
        }
    }
    removed
}

pub fn browser_for_root(root: &Path) -> &'static str {
    let s = root.to_string_lossy().to_ascii_lowercase();
    if s.contains("edge") {
        "Microsoft Edge"
    } else if s.contains("brave") {
        "Brave"
    } else if s.contains("vivaldi") {
        "Vivaldi"
    } else if s.contains("arc") {
        "Arc"
    } else if s.contains("chromium") {
        "Chromium"
    } else {
        "Google Chrome"
    }
}

pub fn browser_process_names(browser: &str) -> &'static [&'static str] {
    match browser {
        "Microsoft Edge" => &[
            "msedge",
            "msedge.exe",
            "microsoft edge",
            "microsoft-edge",
            "microsoft edge dev",
            "microsoft edge canary",
        ],
        "Brave" => &["brave", "brave.exe", "brave browser", "brave-browser"],
        "Vivaldi" => &["vivaldi", "vivaldi.exe"],
        "Arc" => &["arc", "arc.exe"],
        "Chromium" => &["chromium", "chromium.exe", "chromium-browser"],
        _ => &[
            "chrome",
            "chrome.exe",
            "google chrome",
            "google-chrome",
            "google-chrome-beta",
            "google-chrome-unstable",
            "google chrome canary",
        ],
    }
}

pub fn is_browser_running(browser: &str, active_processes: &[String]) -> bool {
    let targets = browser_process_names(browser);
    active_processes.iter().any(|proc_name| {
        let p_lower = proc_name.to_ascii_lowercase();
        let trimmed = p_lower.strip_suffix(".exe").unwrap_or(&p_lower);
        targets.iter().any(|&target| {
            let t_trimmed = target.strip_suffix(".exe").unwrap_or(target);
            trimmed == t_trimmed || p_lower.starts_with(target)
        })
    })
}

pub fn running_processes() -> Vec<String> {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing(),
    );
    sys.processes()
        .values()
        .map(|p| p.name().to_string_lossy().into_owned())
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RemovalOutcome {
    pub removed_ids: Vec<String>,
    pub skipped_browsers: Vec<String>,
}

pub fn remove_browser_extensions_safe(
    roots: &[PathBuf],
    extension_ids: &[String],
    active_processes: &[String],
) -> RemovalOutcome {
    let mut outcome = RemovalOutcome::default();
    for root in roots {
        let browser = browser_for_root(root);
        if is_browser_running(browser, active_processes) {
            if !outcome.skipped_browsers.iter().any(|b| b == browser) {
                outcome.skipped_browsers.push(browser.to_string());
            }
            continue;
        }
        for id in extension_ids {
            let paths = remove_browser_extension_in_roots(std::slice::from_ref(root), id);
            if !paths.is_empty() && !outcome.removed_ids.iter().any(|r| r == id) {
                outcome.removed_ids.push(id.clone());
            }
        }
    }
    outcome
}

pub fn auto_remove_malicious_extensions_with_processes(
    report_path: &Path,
    active_processes: &[String],
) -> Result<RemovalOutcome, String> {
    let content = fs::read_to_string(report_path).map_err(|e| e.to_string())?;
    let ids = parse_malicious_extension_ids(&content);
    if ids.is_empty() {
        return Ok(RemovalOutcome::default());
    }
    let roots = chromium_user_data_roots();
    Ok(remove_browser_extensions_safe(&roots, &ids, active_processes))
}

pub fn auto_remove_malicious_extensions(report_path: &Path) -> Result<RemovalOutcome, String> {
    let procs = running_processes();
    auto_remove_malicious_extensions_with_processes(report_path, &procs)
}

#[cfg(test)]
mod tests {
    #[test]
    fn ordinary_memory_use_does_not_block_an_idle_scan() {
        let mut p = crate::pressure::sample();
        p.cpu_percent = 10;
        p.memory_percent = 65;
        assert_eq!(scan_load(&p), 10);
        p.memory_percent = 95;
        assert_eq!(scan_load(&p), 95);
    }

    use super::*;

    #[test]
    fn settings_are_private_and_interval_is_bounded() {
        let root =
            std::env::temp_dir().join(format!("consilium-security-{}", uuid::Uuid::new_v4()));
        let scanner = Scanner::open(&root);
        scanner
            .save_settings(ScanSettings {
                enabled: false,
                interval_hours: 1,
                auto_remove_malicious_extensions: true,
            })
            .unwrap();
        let status = scanner.status();
        assert!(!status.enabled);
        assert_eq!(status.interval_hours, MIN_INTERVAL_HOURS);
        let again = Scanner::open(&root);
        assert_eq!(again.status().interval_hours, MIN_INTERVAL_HOURS);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(root.join("security/scanner.json"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn missing_tool_is_reported_without_running_anything() {
        let root =
            std::env::temp_dir().join(format!("consilium-security-{}", uuid::Uuid::new_v4()));
        let scanner = Scanner::open(&root);
        assert!(!scanner.status().installed);
        assert!(scanner
            .request_run()
            .unwrap_err()
            .contains("isn't installed"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn an_unreviewed_scanner_is_refused() {
        let root =
            std::env::temp_dir().join(format!("consilium-security-{}", uuid::Uuid::new_v4()));
        let scanner = Scanner::open(&root);
        let tool = scanner.tool_path();
        private_dir(tool.parent().unwrap()).unwrap();
        fs::write(&tool, b"print('not the reviewed release')").unwrap();
        let status = scanner.status();
        assert!(status.installed);
        assert!(!status.verified);
        assert!(scanner.request_run().unwrap_err().contains("doesn't match"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn parses_malicious_extension_ids_from_report_doors() {
        let json = r#"{
            "doors": [
                {
                    "door": "browser:malicious-extension",
                    "observed": "Protecto Whois Search™ [dkcjihabohaldgjkdmenepolojcjdaah] in chrome Default - malicious"
                },
                {
                    "door": "browser:malicious-extension",
                    "observed": "Verify Site Safety for Chrome [dkcjihabohaldgjkdmenepolojcjdaah] in edge Default - malicious"
                },
                {
                    "door": "browser:ad-blocker",
                    "observed": "uBlock Origin"
                }
            ]
        }"#;
        let ids = parse_malicious_extension_ids(json);
        assert_eq!(ids, vec!["dkcjihabohaldgjkdmenepolojcjdaah".to_string()]);
    }

    #[test]
    fn removes_matching_extension_directories_and_preserves_others() {
        let root =
            std::env::temp_dir().join(format!("consilium-ext-test-{}", uuid::Uuid::new_v4()));
        let profile = root.join("Default");
        let bad_id = "dkcjihabohaldgjkdmenepolojcjdaah";
        let good_id = "cjpalhdlnbpafiamejdnhcphjbkeiagm";

        let bad_ext = profile.join("Extensions").join(bad_id).join("1.0.0");
        let bad_sync = profile.join("Sync Extension Settings").join(bad_id);
        let good_ext = profile.join("Extensions").join(good_id).join("1.0.0");

        fs::create_dir_all(&bad_ext).unwrap();
        fs::create_dir_all(&bad_sync).unwrap();
        fs::create_dir_all(&good_ext).unwrap();
        fs::write(bad_ext.join("manifest.json"), b"{}").unwrap();
        fs::write(good_ext.join("manifest.json"), b"{}").unwrap();

        let removed = remove_browser_extension_in_roots(std::slice::from_ref(&root), bad_id);
        assert_eq!(removed.len(), 2);
        assert!(!profile.join("Extensions").join(bad_id).exists());
        assert!(!profile.join("Sync Extension Settings").join(bad_id).exists());
        assert!(profile.join("Extensions").join(good_id).exists());

        let invalid = remove_browser_extension_in_roots(std::slice::from_ref(&root), "../../etc/passwd");
        assert!(invalid.is_empty());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn settings_round_trip_with_auto_remove_toggle() {
        let root =
            std::env::temp_dir().join(format!("consilium-sec-test-{}", uuid::Uuid::new_v4()));
        let scanner = Scanner::open(&root);
        let status = scanner.status();
        assert!(status.auto_remove_malicious_extensions);

        scanner
            .save_settings(ScanSettings {
                enabled: true,
                interval_hours: 48,
                auto_remove_malicious_extensions: false,
            })
            .unwrap();

        let updated = scanner.status();
        assert!(!updated.auto_remove_malicious_extensions);
        assert_eq!(updated.interval_hours, 48);

        let reloaded = Scanner::open(&root);
        assert!(!reloaded.status().auto_remove_malicious_extensions);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn skips_extension_cleanup_when_browser_is_running() {
        let root =
            std::env::temp_dir().join(format!("consilium-running-browser-test-{}", uuid::Uuid::new_v4()));
        let profile = root.join("Default");
        let bad_id = "dkcjihabohaldgjkdmenepolojcjdaah";

        let bad_ext = profile.join("Extensions").join(bad_id).join("1.0.0");
        fs::create_dir_all(&bad_ext).unwrap();
        fs::write(bad_ext.join("manifest.json"), b"{}").unwrap();

        // When browser process (e.g. "Google Chrome") is active, nothing is deleted.
        let active = vec!["Google Chrome".to_string()];
        let outcome = remove_browser_extensions_safe(
            std::slice::from_ref(&root),
            &[bad_id.to_string()],
            &active,
        );
        assert!(outcome.removed_ids.is_empty());
        assert_eq!(outcome.skipped_browsers, vec!["Google Chrome".to_string()]);
        assert!(bad_ext.exists(), "Extension files must not be deleted while browser is running");

        // When browser is closed, cleanup proceeds.
        let closed: Vec<String> = Vec::new();
        let outcome_closed = remove_browser_extensions_safe(
            std::slice::from_ref(&root),
            &[bad_id.to_string()],
            &closed,
        );
        assert_eq!(outcome_closed.removed_ids, vec![bad_id.to_string()]);
        assert!(outcome_closed.skipped_browsers.is_empty());
        assert!(!bad_ext.exists(), "Extension files should be removed once browser is closed");

        let _ = fs::remove_dir_all(root);
    }
}
