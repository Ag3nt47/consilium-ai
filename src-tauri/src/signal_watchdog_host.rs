//! Windows starts the Signal recovery listener outside Consilium's GUI process and job.

use crate::update::{ps_quote, start_windows_helper};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn fingerprint(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn copy_worker(dir: &Path) -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("couldn't find Consilium: {e}"))?;
    let expected =
        fingerprint(&exe).map_err(|e| format!("couldn't identify this Consilium build: {e}"))?;
    let worker = dir.join(format!("worker-{expected}.exe"));
    if fingerprint(&worker).is_ok_and(|digest| digest == expected) {
        return Ok(worker);
    }
    let staged = dir.join(format!("worker-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        fs::copy(exe, &staged).map_err(|e| format!("couldn't copy the Signal watchdog: {e}"))?;
        if fingerprint(&staged)
            .map_err(|e| format!("couldn't verify the Signal watchdog copy: {e}"))?
            != expected
        {
            return Err("the Signal watchdog copy didn't match Consilium".into());
        }
        fs::rename(&staged, &worker)
            .map_err(|e| format!("couldn't save the Signal watchdog copy: {e}"))
    })();
    if result.is_err() {
        let _ = fs::remove_file(staged);
    }
    result.map(|()| worker)
}

const LAUNCH_WORKER: &str = r#"
$ErrorActionPreference = 'Stop'
function Write-Log([string]$message) {
    [IO.File]::AppendAllText($logPath, "$( [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() ) $message" + [Environment]::NewLine)
}
try {
    $wait = [Diagnostics.Stopwatch]::StartNew()
    while (-not (Test-Path -LiteralPath $armPath)) {
        if ($wait.Elapsed.TotalSeconds -ge 30) { return }
        Start-Sleep -Milliseconds 100
    }
    $env:CONSILIUM_DATA = $dataOverride
    $targetArg = '"' + $target + '"'
    $process = Start-Process -FilePath $worker -ArgumentList @('--signal-watchdog-worker', $targetArg) `
        -WorkingDirectory ([IO.Path]::GetDirectoryName($target)) -WindowStyle Hidden `
        -RedirectStandardError $errorLog -PassThru
    Write-Log "started Signal watchdog process $($process.Id)"
} catch {
    Write-Log "couldn't launch Signal watchdog (error $($_.Exception.HResult))"
    exit 1
} finally {
    Remove-Item -LiteralPath $armPath, $PSCommandPath -ErrorAction SilentlyContinue
}
"#;

/// Only one launcher per profile can prepare the copy and wait for the worker to acquire its lock.
pub(crate) fn start(data: &Path, target: &Path) -> Result<(), String> {
    let data =
        std::path::absolute(data).map_err(|e| format!("couldn't find Consilium's profile: {e}"))?;
    if !crate::signal_watchdog::configured(&data) {
        return Ok(());
    }
    let target =
        std::path::absolute(target).map_err(|e| format!("couldn't find Consilium: {e}"))?;
    if !target.is_file() {
        return Err("couldn't find Consilium's installed program".into());
    }
    let dir = data.join("watchdog");
    fs::create_dir_all(&dir)
        .map_err(|e| format!("couldn't prepare the Signal watchdog folder: {e}"))?;
    let launch_lock = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join("signal-launch.lock"))
        .map_err(|e| format!("couldn't open the Signal watchdog launch lock: {e}"))?;
    match launch_lock.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Ok(()),
        Err(TryLockError::Error(e)) => {
            return Err(format!("couldn't lock the Signal watchdog launcher: {e}"));
        }
    }
    if crate::signal_watchdog::worker_running(&data)? {
        return Ok(());
    }

    let worker = copy_worker(&dir)?;
    let id = uuid::Uuid::new_v4();
    let helper = dir.join(format!("signal-{id}.ps1"));
    let launcher = dir.join(format!("signal-{id}-launch.ps1"));
    let arm = dir.join(format!("signal-{id}.arm"));
    let data_override = if crate::data_override().is_some() {
        data.as_path()
    } else {
        Path::new("")
    };
    let script = format!(
        "\u{feff}$worker = {}\n\
         $target = {}\n\
         $dataOverride = {}\n\
         $armPath = {}\n\
         $logPath = {}\n\
         $errorLog = {}\n{}",
        ps_quote(&worker),
        ps_quote(&target),
        ps_quote(data_override),
        ps_quote(&arm),
        ps_quote(&dir.join("signal-launch.log")),
        ps_quote(&dir.join("signal-worker.stderr.log")),
        LAUNCH_WORKER,
    );
    fs::write(&helper, script)
        .map_err(|e| format!("couldn't write the Signal watchdog launcher: {e}"))?;
    let launched = start_windows_helper(&helper, &launcher);
    let _ = fs::remove_file(launcher);
    if let Err(error) = launched {
        let _ = fs::remove_file(helper);
        return Err(error);
    }
    fs::write(&arm, b"armed\n")
        .map_err(|e| format!("couldn't arm the Signal watchdog launcher: {e}"))?;
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        if crate::signal_watchdog::worker_running(&data)? {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = fs::remove_file(arm);
    Err("the Signal watchdog didn't acquire its profile lock; see the watchdog logs".into())
}
