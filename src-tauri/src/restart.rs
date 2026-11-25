use std::path::Path;

fn lock_file(path: &Path) -> Result<std::fs::File, String> {
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|e| format!("couldn't open Consilium's restart lock: {e}"))?;
    lock.try_lock().map_err(|error| match error {
        std::fs::TryLockError::WouldBlock => {
            "Consilium is already preparing an update or restart".into()
        }
        std::fs::TryLockError::Error(error) => {
            format!("couldn't lock Consilium's restart state: {error}")
        }
    })?;
    Ok(lock)
}

/// The detached helper holds its own lock after the original process's exit lock is released.
#[cfg(windows)]
pub(crate) fn windows_handoff_script(data: &Path) -> String {
    let path = crate::update::ps_quote(&data.join("watchdog").join("detached-exit.lock"));
    format!(
        "$ErrorActionPreference = 'Stop'\n\
        $handoff = [IO.File]::Open({path}, 'OpenOrCreate', 'ReadWrite', 'ReadWrite')\n\
        $handoff.Lock(0, 1)\n"
    )
}

/// An installer and a relaunch must never both be armed for the same app exit.
pub(crate) struct ExitClaim {
    _lock: std::fs::File,
}

impl ExitClaim {
    pub(crate) fn take(data: &Path) -> Result<Self, String> {
        let dir = data.join("watchdog");
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("couldn't prepare Consilium's restart lock: {e}"))?;
        let lock = lock_file(&dir.join("exit.lock"))?;
        #[cfg(windows)]
        drop(lock_file(&dir.join("detached-exit.lock"))?);
        Ok(Self { _lock: lock })
    }

    pub(crate) fn commit(self) {
        std::mem::forget(self);
    }
}

pub(crate) fn request(app: &tauri::AppHandle, data: &Path, stalled: bool) -> Result<(), String> {
    let claim = ExitClaim::take(data)?;
    crate::watchdog::can_relaunch(data).map_err(str::to_owned)?;
    #[cfg(windows)]
    {
        let pid = std::process::id();
        let target = std::env::current_exe().map_err(|e| format!("can't locate Consilium: {e}"))?;
        windows::arm(data, &target, pid, stalled, false)?;
    }
    #[cfg(not(windows))]
    let _ = stalled;
    claim.commit();
    let app = app.clone();
    std::thread::spawn(move || {
        // Give the manual command time to return its acknowledgement.
        std::thread::sleep(std::time::Duration::from_millis(300));
        #[cfg(windows)]
        app.exit(0);
        #[cfg(not(windows))]
        app.restart();
    });
    Ok(())
}

#[cfg(windows)]
pub(crate) mod windows {
    use crate::update::{ps_quote, start_windows_helper};
    use std::io;
    use std::path::Path;
    use std::time::{Duration, Instant};
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    pub(crate) fn app_pid(target: &Path, data: &Path) -> Result<Option<u32>, String> {
        use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
        let target = std::fs::canonicalize(target)
            .map_err(|e| format!("can't locate the installed app: {e}"))?;
        let data = std::fs::canonicalize(data)
            .map_err(|e| format!("can't locate Consilium's profile: {e}"))?;
        let mut system = System::new();
        let kind = ProcessRefreshKind::nothing()
            .with_exe(UpdateKind::Always)
            .with_cmd(UpdateKind::Always);
        system.refresh_processes_specifics(ProcessesToUpdate::All, true, kind);
        let mut candidates = Vec::new();
        for process in system.processes().values() {
            let matches = process.exe().is_some_and(|exe| {
                let matches_name = exe
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.eq_ignore_ascii_case("consilium.exe"));
                matches_name && std::fs::canonicalize(exe).ok().as_ref() == Some(&target)
            });
            if !matches {
                continue;
            }
            let Some((_, args)) = process.cmd().split_first() else {
                return Err("couldn't verify how the running Consilium process was started".into());
            };
            let args: Vec<_> = args
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            if !crate::cli::wants_cli(&args) {
                candidates.push(process.pid());
            }
        }
        if candidates.is_empty() {
            return Ok(None);
        }

        let kind = ProcessRefreshKind::nothing().with_environ(UpdateKind::Always);
        system.refresh_processes_specifics(ProcessesToUpdate::Some(&candidates), true, kind);
        let mut matches = Vec::new();
        for pid in candidates {
            let Some(process) = system.process(pid) else {
                continue;
            };
            if process.environ().is_empty() {
                return Err("couldn't verify the running app's profile".into());
            }
            let profile = process
                .environ()
                .iter()
                .filter_map(|value| value.to_str())
                .find_map(|value| {
                    let (key, value) = value.split_once('=')?;
                    (key.eq_ignore_ascii_case("CONSILIUM_DATA") && !value.is_empty())
                        .then_some(value)
                });
            let profile = match profile {
                Some(path) if Path::new(path).is_absolute() => std::fs::canonicalize(path).ok(),
                Some(_) => return Err("couldn't verify a relative running app profile".into()),
                None => dirs::data_dir()
                    .and_then(|path| std::fs::canonicalize(path.join(crate::IDENTIFIER)).ok()),
            };
            if profile.as_ref() == Some(&data) {
                matches.push(pid.as_u32());
            }
        }
        match matches.as_slice() {
            [] => Ok(None),
            [pid] => Ok(Some(*pid)),
            _ => Err("more than one Consilium window is running for this profile".into()),
        }
    }

    pub(crate) fn remote(data: &Path, target: &Path) -> Result<(), String> {
        let _claim = super::ExitClaim::take(data)?;
        crate::watchdog::can_relaunch(data).map_err(str::to_owned)?;
        match app_pid(target, data)? {
            Some(pid) => arm(data, target, pid, true, true)?,
            None => {
                let folder = target
                    .parent()
                    .ok_or("can't locate the installed app's folder")?;
                std::process::Command::new(target)
                    .current_dir(folder)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .map_err(|e| format!("couldn't open Consilium: {e}"))?;
            }
        }
        Ok(())
    }

    fn started_ticks(pid: u32) -> Result<u64, String> {
        // Read the creation time Windows associates with this PID. The script checks it again
        // while holding its own process handle, before promising to stop or replace the process.
        let mut creation = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let mut exit = creation;
        let mut kernel = creation;
        let mut user = creation;
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if process.is_null() {
            return Err(format!(
                "can't open the Consilium process: {}",
                io::Error::last_os_error()
            ));
        }
        let result =
            unsafe { GetProcessTimes(process, &mut creation, &mut exit, &mut kernel, &mut user) };
        let error = (result == 0).then(io::Error::last_os_error);
        unsafe { CloseHandle(process) };
        if let Some(error) = error {
            return Err(format!("can't identify the Consilium process: {error}"));
        }
        let filetime =
            (u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime);
        Ok(filetime + 504_911_232_000_000_000) // Windows FILETIME epoch as .NET DateTime ticks.
    }

    pub(crate) fn arm(
        data: &Path,
        target: &Path,
        pid: u32,
        stalled: bool,
        close_window: bool,
    ) -> Result<(), String> {
        let data = std::path::absolute(data)
            .map_err(|e| format!("can't locate Consilium's profile: {e}"))?;
        let dir = data.join("watchdog");
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("couldn't prepare the restart helper: {e}"))?;

        let attempt = uuid::Uuid::new_v4();
        let helper = dir.join(format!("{attempt}.ps1"));
        let launcher = dir.join(format!("{attempt}-launch.ps1"));
        let ready = dir.join(format!("{attempt}.ready"));
        let arm = dir.join(format!("{attempt}.arm"));
        let log = dir.join("relaunch.log");
        let lock = data.join("update").join("installing.lock");
        let quit = data.join("deliberate_quit.flag");
        let profile = if crate::data_override().is_some() {
            data.as_path()
        } else {
            Path::new("")
        };
        let old_started = started_ticks(pid)?;
        let ps_bool = |value| if value { "$true" } else { "$false" };
        let script = format!(
            "\u{feff}{}$oldPid = {pid}\n\
            $oldStartedTicks = [int64]{old_started}\n\
            $target = {}\n\
            $profile = {}\n\
            $readyPath = {}\n\
            $armPath = {}\n\
            $installLockPath = {}\n\
            $quitFlagPath = {}\n\
            $logPath = {}\n\
            $forceStalled = {}\n\
            $closeWindow = {}\n{}",
            super::windows_handoff_script(&data),
            ps_quote(target),
            ps_quote(profile),
            ps_quote(&ready),
            ps_quote(&arm),
            ps_quote(&lock),
            ps_quote(&quit),
            ps_quote(&log),
            ps_bool(stalled),
            ps_bool(close_window),
            include_str!("watchdog_relaunch.ps1")
        );
        std::fs::write(&helper, script)
            .map_err(|e| format!("couldn't write the restart helper: {e}"))?;
        let launched = start_windows_helper(&helper, &launcher);
        let _ = std::fs::remove_file(&launcher);
        let helper_pid = launched?;
        let deadline = Instant::now() + Duration::from_secs(8);
        while Instant::now() < deadline {
            if std::fs::read_to_string(&ready)
                .ok()
                .and_then(|value| value.trim().parse::<u32>().ok())
                == Some(helper_pid)
            {
                // A late helper never restarts the app without this attempt-specific arm file.
                return std::fs::write(&arm, b"armed\n")
                    .map_err(|e| format!("couldn't arm the restart helper: {e}"));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Err(format!(
            "the restart helper didn't confirm readiness; see {}",
            log.display()
        ))
    }
}
