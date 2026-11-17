//! Cleans up AI turns left running after Consilium was force-quit or froze.
//!
//! Each turn runs in its own process group so Stop can end everything it started. The flip side
//! is that when Consilium dies without a clean exit, those groups survive as orphans (parent
//! pid 1) and keep running whatever they were doing, such as a full Rust build, with nobody
//! reading their output. On a small machine that load is enough to freeze the next session.
//! On Windows the same happens to process trees whose parent Consilium is gone.
//! Every turn's group leader is recorded in `children.pids`; on startup, any recorded leader that
//! is now an orphan and is still an AI CLI has its whole group ended.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

const FILE: &str = "children.pids";
/// Program names a recorded pid must still have before its group is ended, so a pid the system
/// has since reused for something else is never touched.
#[cfg(unix)]
const AGENT_NAMES: [&str; 6] = ["claude", "codex", "agy", "gemini", "node", "CodexCLI"];

static LOCK: Mutex<()> = Mutex::new(());

fn file() -> Option<PathBuf> {
    crate::data_dir().map(|d| d.join(FILE))
}

fn read(path: &Path) -> Vec<u32> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect()
}

fn write(path: &Path, pids: &[u32]) {
    let body: String = pids.iter().map(|p| format!("{p}\n")).collect();
    let _ = std::fs::write(path, body);
}

/// Records a running turn until it is dropped.
pub struct Registered(Option<u32>);

impl Registered {
    pub fn new(pid: Option<u32>) -> Self {
        if let (Some(pid), Some(path)) = (pid, file()) {
            let _g = LOCK.lock();
            let mut pids = read(&path);
            if !pids.contains(&pid) {
                pids.push(pid);
                write(&path, &pids);
            }
        }
        Registered(pid)
    }
}

impl Drop for Registered {
    fn drop(&mut self) {
        if let (Some(pid), Some(path)) = (self.0, file()) {
            let _g = LOCK.lock();
            let pids: Vec<u32> = read(&path).into_iter().filter(|p| *p != pid).collect();
            write(&path, &pids);
        }
    }
}

/// True when `ps` shows the process orphaned (parent 1) and running a known AI CLI.
#[cfg(unix)]
fn orphaned_agent(pid: u32) -> bool {
    let Ok(out) = std::process::Command::new("ps")
        .args(["-o", "ppid=,command=", "-p", &pid.to_string()])
        .output()
    else {
        return false;
    };
    let line = String::from_utf8_lossy(&out.stdout);
    let line = line.trim();
    let mut parts = line.splitn(2, char::is_whitespace);
    let ppid = parts.next().unwrap_or("").trim();
    let cmd = parts.next().unwrap_or("").trim();
    let program = cmd.split_whitespace().next().unwrap_or("");
    let name = Path::new(program).file_name().and_then(|n| n.to_str()).unwrap_or("");
    ppid == "1" && AGENT_NAMES.contains(&name)
}

/// Whether a Windows process is an AI CLI: its image is one (`claude.exe`, `node.exe`, …), or it
/// is `cmd.exe` or `powershell.exe` that runs an npm shim or script for one.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
fn windows_agent(image: &str, cmdline: &str) -> bool {
    let image = image.to_ascii_lowercase();
    let stem = image.strip_suffix(".exe").unwrap_or(&image);
    let direct_agent = ["claude", "codex", "agy", "gemini", "codexcli"]
        .iter()
        .any(|n| n.eq_ignore_ascii_case(stem));
    if direct_agent {
        return true;
    }
    let line = cmdline.to_ascii_lowercase();
    let is_wrapper = matches!(stem, "cmd" | "powershell" | "pwsh" | "node");
    is_wrapper
        && ["claude", "codex", "agy", "gemini"]
            .iter()
            .any(|n| line.contains(n))
}

/// Windows has no reparenting to pid 1: a turn is orphaned when the process that started it
/// (an earlier Consilium) is gone.
#[cfg(windows)]
fn orphaned(pids: &[u32]) -> Vec<u32> {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    if pids.is_empty() {
        return Vec::new();
    }
    // One process snapshot for every pid: reading every process's command line is slow on Windows
    // (it held up startup for each pid when taken one at a time).
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_cmd(UpdateKind::Always),
    );
    let my_pid = std::process::id();
    pids.iter()
        .copied()
        .filter(|&pid| {
            let Some(p) = sys.process(Pid::from_u32(pid)) else { return false };
            let parent_alive = p.parent().is_some_and(|pp| {
                if pp.as_u32() == my_pid {
                    return false;
                }
                if let Some(parent) = sys.process(pp) {
                    let name = parent.name().to_string_lossy().to_ascii_lowercase();
                    let is_consilium = name == "consilium.exe" || name == "consilium";
                    is_consilium && parent.start_time() <= p.start_time()
                } else {
                    false
                }
            });
            let cmdline = p.cmd().iter().map(|a| a.to_string_lossy()).collect::<Vec<_>>().join(" ");
            !parent_alive && windows_agent(&p.name().to_string_lossy(), &cmdline)
        })
        .collect()
}

#[cfg(unix)]
fn orphaned(pids: &[u32]) -> Vec<u32> {
    pids.iter().copied().filter(|&pid| orphaned_agent(pid)).collect()
}

#[cfg(not(any(unix, windows)))]
fn orphaned(_pids: &[u32]) -> Vec<u32> {
    Vec::new()
}

/// Ends a turn and everything it started: its process group on Unix, its process tree on Windows.
fn end_turn(pid: u32, force: bool) {
    #[cfg(unix)]
    // SAFETY: plain signal calls on a process group this app created.
    unsafe {
        libc::killpg(pid as i32, if force { libc::SIGKILL } else { libc::SIGTERM });
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let mut tk = std::process::Command::new("taskkill");
        tk.args(["/PID", &pid.to_string(), "/T"]);
        if force {
            tk.arg("/F");
        }
        let _ = tk
            .creation_flags(CREATE_NO_WINDOW)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    #[cfg(not(any(unix, windows)))]
    let _ = (pid, force);
}

/// Ends every orphaned turn recorded by an earlier run. Called once at startup, before any new
/// turn starts, so everything in the file belongs to a previous session. Returns how many
/// turns were ended.
pub fn reap_orphans(data: &Path) -> usize {
    let path = data.join(FILE);
    let _g = LOCK.lock();
    let found = orphaned(&read(&path));
    for &pid in &found {
        end_turn(pid, cfg!(windows)); // Windows console programs ignore a polite taskkill
    }
    let ended = found.len();
    if ended > 0 {
        std::thread::sleep(std::time::Duration::from_millis(1500));
        for pid in orphaned(&found) {
            end_turn(pid, true); // it ignored the polite request
        }
    }
    let _ = std::fs::remove_file(&path);
    ended
}

#[cfg(test)]
mod portable_tests {
    use super::windows_agent;

    #[test]
    fn recognises_windows_agent_processes_only() {
        assert!(windows_agent("claude.exe", "claude.exe -p"));
        assert!(windows_agent("Node.EXE", "node codex.js"));
        assert!(windows_agent("cmd.exe", r"C:\Windows\system32\cmd.exe /c C:\Users\username\AppData\Roaming\npm\codex.cmd exec")); // consilium-allow-secret-scan (dummy Windows test path)
        assert!(windows_agent("cmd.exe", r"C:\Windows\system32\cmd.exe /c C:\Users\username\claude.bat")); // consilium-allow-secret-scan
        assert!(windows_agent("powershell.exe", r"powershell -File C:\Users\username\agy.ps1")); // consilium-allow-secret-scan
        assert!(!windows_agent("cmd.exe", "cmd.exe /c build.bat"));
        assert!(!windows_agent("node.exe", "node vite.js --port 3000"));
        assert!(!windows_agent("explorer.exe", "explorer.exe"));
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn leaves_live_non_orphans_and_clears_the_file() {
        let dir = std::env::temp_dir().join(format!("reaper-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // This test process is not an orphan and is not an AI CLI, so it must survive.
        write(&dir.join(FILE), &[std::process::id(), 999_999]);
        assert_eq!(reap_orphans(&dir), 0);
        assert!(!dir.join(FILE).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_reused_pid_running_something_else_is_not_an_agent() {
        assert!(!orphaned_agent(std::process::id()));
        assert!(!orphaned_agent(1));
    }
}
