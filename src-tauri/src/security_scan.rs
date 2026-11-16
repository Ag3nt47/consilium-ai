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
const RETRY_DELAY_MS: u64 = 15 * 60 * 1_000;
/// GitHub release v3.0.2 standalone Python asset, reviewed and pinned on 2026-11-16.
const ZERO_BREACH_SHA256: &str = "0b6df5f02c6b13256b735bfb55c1ef0a31ff0c762550910be9f66b4f5090dafb";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScanSettings {
    /// Enabled by default, but inert until the separately approved scanner is installed.
    pub enabled: bool,
    pub interval_hours: u32,
}

impl Default for ScanSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_hours: DEFAULT_INTERVAL_HOURS,
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
        let pressure = crate::pressure::sample().percent;
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
                    let summary = match code {
                        Some(0) => "No known indicators or evidence gaps were reported by this scan.",
                        Some(2) => "Zero Breach reported one or more actionable matches. Review the private report.",
                        Some(3) => "No actionable match was reported, but some evidence could not be checked.",
                        Some(c) => return self.finish(Some(c), format!("Zero Breach ended with exit code {c}."), None),
                        None => "Zero Breach was interrupted before it returned a result.",
                    };
                    set_private_file(&json);
                    set_private_file(&text);
                    self.finish(code, summary.into(), Some(text));
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
            let pressure = crate::pressure::sample().percent;
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

fn set_private_file(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
}

#[cfg(test)]
mod tests {
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
}
