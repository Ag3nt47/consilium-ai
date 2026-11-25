use std::process::{Command, Stdio};
use std::time::Duration;

pub(crate) const WORKER_ARG: &str = "--watchdog-suspend-worker";
pub(crate) const SCHEDULED: &str = "Consilium sleep scheduled";

#[cfg(target_os = "macos")]
const SUSPEND_PROGRAM: &str = "/usr/bin/pmset";
#[cfg(target_os = "linux")]
const SUSPEND_PROGRAM: &str = "/usr/bin/systemctl";

/// The caller can acknowledge and close its SSH channel before the machine suspends.
pub(crate) fn schedule() -> Result<(), String> {
    #[cfg(windows)]
    windows::prepare()?;
    #[cfg(unix)]
    if !std::path::Path::new(SUSPEND_PROGRAM).is_file() {
        return Err("The system suspend command is unavailable.".into());
    }

    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut worker = Command::new(executable);
    worker
        .arg(WORKER_ARG)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        worker.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        worker.process_group(0);
    }
    worker
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Couldn't schedule system sleep: {e}"))
}

pub(crate) fn run() -> i32 {
    std::thread::sleep(Duration::from_secs(6));
    match suspend() {
        Ok(()) => 0,
        Err(error) => {
            if let Some(data) = crate::data_dir() {
                let _ = std::fs::create_dir_all(data.join("watchdog"));
                let _ = std::fs::write(data.join("watchdog").join("sleep-error.log"), error);
            }
            1
        }
    }
}

#[cfg(unix)]
fn suspend() -> Result<(), String> {
    let args: &[&str] = if cfg!(target_os = "macos") {
        &["sleepnow"]
    } else {
        &["--no-ask-password", "suspend"]
    };
    let status = Command::new(SUSPEND_PROGRAM)
        .args(args)
        .status()
        .map_err(|e| e.to_string())?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| format!("System sleep failed with {status}."))
}

#[cfg(windows)]
fn suspend() -> Result<(), String> {
    use windows_sys::Win32::System::Power::SetSuspendState;
    windows::prepare()?;
    // Use normal suspend and leave existing wake sources enabled.
    if unsafe { SetSuspendState(false, false, false) } {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error().to_string())
    }
}

#[cfg(windows)]
mod windows {
    use std::ptr::{null, null_mut};
    use windows_sys::core::w;
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_SUCCESS, HANDLE, LUID};
    use windows_sys::Win32::Security::{
        AdjustTokenPrivileges, LookupPrivilegeValueW, LUID_AND_ATTRIBUTES, SE_PRIVILEGE_ENABLED,
        TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
    };
    use windows_sys::Win32::System::Power::IsPwrSuspendAllowed;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    struct Token(HANDLE);

    impl Drop for Token {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
    }

    pub(super) fn prepare() -> Result<(), String> {
        if !unsafe { IsPwrSuspendAllowed() } {
            return Err("Windows does not currently allow sleep.".into());
        }

        let mut luid = LUID {
            LowPart: 0,
            HighPart: 0,
        };
        let mut handle = null_mut();
        // Access only this process token. This enables a privilege already granted to the account.
        if unsafe { LookupPrivilegeValueW(null(), w!("SeShutdownPrivilege"), &mut luid) } == 0
            || unsafe {
                OpenProcessToken(
                    GetCurrentProcess(),
                    TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
                    &mut handle,
                )
            } == 0
        {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let token = Token(handle);
        let privileges = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: SE_PRIVILEGE_ENABLED,
            }],
        };
        let adjusted =
            unsafe { AdjustTokenPrivileges(token.0, 0, &privileges, 0, null_mut(), null_mut()) };
        let status = unsafe { GetLastError() };
        if adjusted == 0 || status != ERROR_SUCCESS {
            return Err("The Windows account is not allowed to suspend this computer.".into());
        }
        Ok(())
    }
}
