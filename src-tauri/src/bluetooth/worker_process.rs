//! Hidden workers receive their first request only after joining an owned job.
use std::io;
use std::mem::size_of;
use std::os::windows::{io::AsRawHandle, process::CommandExt};
use std::process::{Child, Command, Stdio};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};

pub(super) struct Job(usize);
impl Job {
    pub(super) fn assign(child: &Child) -> io::Result<Self> {
        // SAFETY: the unnamed job is private and uses the SDK type layouts.
        let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if raw.is_null() {
            return Err(io::Error::last_os_error());
        }
        let job = Self(raw as usize);
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: limits is the structure selected by this information class.
        if unsafe {
            SetInformationJobObject(
                raw,
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: both handles remain owned for this call.
        if unsafe { AssignProcessToJobObject(raw, child.as_raw_handle() as HANDLE) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: this is the only owner of the job handle.
        unsafe { CloseHandle(self.0 as HANDLE); }
    }
}

pub(super) struct OwnedChild {
    pub(super) child: Child,
    _job: Job,
}

impl OwnedChild {
    pub(super) fn spawn(command: &str) -> io::Result<Self> {
        let mut child = Command::new(std::env::current_exe()?)
            .arg(command)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW)
            .spawn()?;
        // The worker waits on its private input until the job assignment succeeds.
        match Job::assign(&child) {
            Ok(job) => Ok(Self { child, _job: job }),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                Err(error)
            }
        }
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
