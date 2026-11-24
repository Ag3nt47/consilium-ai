//! Where to look for programs: the search path the human's own terminal would have.
//!
//! Consilium inherits its search path from whatever started it. After an automatic update it is
//! started by the previous copy, so it inherits that copy's search path, which can be days old:
//! tools installed since (or folders added to the human's PATH since) were missing from it, and
//! installed AIs and tools looked uninstalled after every update. So the search path is read fresh,
//! at most once a minute: on Windows the PATH saved for the user and the machine (what a new
//! terminal gets), on macOS and Linux the login shell's PATH, plus the folders tools usually
//! install into. Finding AIs, the tool list, and the commands the AIs and the human run all use it.

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const REFRESH: Duration = Duration::from_secs(60);

struct PathCache {
    snapshot: Mutex<Option<(Instant, Vec<PathBuf>)>>,
    refreshing: AtomicBool,
}

impl PathCache {
    const fn new() -> Self { Self { snapshot: Mutex::new(None), refreshing: AtomicBool::new(false) } }

    fn dirs(&self, current: Vec<PathBuf>, usual: Vec<PathBuf>, probe: impl FnOnce() -> Option<Vec<PathBuf>>) -> Vec<PathBuf> {
        let previous = self.snapshot.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some((at, folders)) = &previous {
            if at.elapsed() < REFRESH { return folders.clone(); }
        }
        if self.refreshing.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() {
            return merge(current, previous.map(|(_, folders)| folders).unwrap_or_default(), usual);
        }
        struct Refresh<'a>(&'a AtomicBool);
        impl Drop for Refresh<'_> { fn drop(&mut self) { self.0.store(false, Ordering::Release); } }
        let _refresh = Refresh(&self.refreshing);
        // A different refresh can finish between our snapshot and gate acquisition.
        if let Some((at, folders)) = self.snapshot.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            if at.elapsed() < REFRESH { return folders.clone(); }
        }
        // Never run a shell or registry query while holding the shared snapshot lock.
        let saved = probe().unwrap_or_else(|| previous.map(|(_, folders)| folders).unwrap_or_default());
        let folders = merge(current, saved, usual);
        *self.snapshot.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), folders.clone()));
        folders
    }
}

static CACHE: PathCache = PathCache::new();

/// Every folder to search, in order, without duplicates.
pub fn dirs() -> Vec<PathBuf> {
    let current = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).collect();
    CACHE.dirs(current, usual_dirs(), || {
        #[cfg(windows)]
        { Some(saved_path()) }
        #[cfg(not(windows))]
        { saved_path() }
    })
}

/// `dirs`, joined for a PATH variable.
pub fn joined() -> Option<std::ffi::OsString> {
    std::env::join_paths(dirs()).ok()
}

fn merge(current: Vec<PathBuf>, saved: Vec<PathBuf>, usual: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut add = |p: PathBuf| {
        if !p.as_os_str().is_empty() && !out.iter().any(|o| same(o, &p)) {
            out.push(p);
        }
    };
    for p in current {
        add(p);
    }
    for p in saved {
        add(p);
    }
    for p in usual {
        add(p);
    }
    out
}

fn same(a: &std::path::Path, b: &std::path::Path) -> bool {
    if cfg!(windows) {
        a.to_string_lossy().trim_end_matches(['\\', '/']).eq_ignore_ascii_case(b.to_string_lossy().trim_end_matches(['\\', '/']))
    } else {
        a == b
    }
}

/// Folders tools commonly install into, whether or not they are on any PATH.
fn usual_dirs() -> Vec<PathBuf> {
    let home = dirs::home_dir().unwrap_or_default();
    let mut d: Vec<PathBuf> = [".local/bin", ".local/node/bin", ".cargo/bin", ".npm-global/bin", ".volta/bin", "go/bin", "scoop/shims", "bin"]
        .iter()
        .map(|p| home.join(p))
        .collect();
    d.extend(["/usr/local/bin", "/opt/homebrew/bin", "/usr/bin", "/bin", "/snap/bin"].iter().map(PathBuf::from));
    if let Some(appdata) = std::env::var_os("APPDATA") {
        d.push(PathBuf::from(appdata).join("npm"));
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let l = PathBuf::from(local);
        d.push(l.join("Microsoft").join("WindowsApps"));
        d.push(l.join("Programs").join("OpenAI").join("Codex").join("bin"));
        d.push(l.join("agy").join("bin"));
    }
    if let Some(pf) = std::env::var_os("ProgramFiles") {
        let p = PathBuf::from(pf);
        d.push(p.join("nodejs"));
        d.push(p.join("Git").join("cmd"));
    }
    d
}

/// The PATH a new terminal would get.
#[cfg(windows)]
fn saved_path() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for key in [r"HKCU\Environment", r"HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment"] {
        if let Some(v) = reg_path(key) {
            out.extend(std::env::split_paths(&expand(&v)));
        }
    }
    out
}

#[cfg(windows)]
fn reg_path(key: &str) -> Option<String> {
    use std::os::windows::process::CommandExt;
    let out = std::process::Command::new("reg")
        .args(["query", key, "/v", "Path"])
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .output()
        .ok()?;
    parse_reg(&String::from_utf8_lossy(&out.stdout))
}

/// The value from `reg query ... /v Path` output: "    Path    REG_EXPAND_SZ    C:\a;C:\b".
#[cfg_attr(not(windows), allow(dead_code))]
fn parse_reg(text: &str) -> Option<String> {
    text.lines().find_map(|l| {
        let l = l.trim_start();
        if !l.to_ascii_lowercase().starts_with("path ") {
            return None;
        }
        let at = l.find("REG_")?;
        let rest = &l[at..];
        let value_at = rest.find(char::is_whitespace)?;
        Some(rest[value_at..].trim().to_string())
    })
}

/// Expands %NAME% from the environment, as Windows does for a saved PATH.
#[cfg_attr(not(windows), allow(dead_code))]
fn expand(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) => {
                let name = &after[..end];
                match std::env::var(name) {
                    Ok(v) if !name.is_empty() => out.push_str(&v),
                    _ => out.push_str(&rest[start..start + end + 2]),
                }
                rest = &after[end + 1..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// The login shell's PATH (what a new Terminal window gets), or nothing if it takes too long.
#[cfg(not(windows))]
fn saved_path() -> Option<Vec<PathBuf>> {
    let shell = std::env::var("SHELL").ok().filter(|s| s.starts_with('/')).unwrap_or_else(|| "/bin/sh".into());
    let mut command = std::process::Command::new(shell);
    command.args(["-l", "-c", "printf '%s' \"$PATH\""]);
    let output = path_output(command, Duration::from_secs(3), 128 * 1024)?;
    let text = String::from_utf8(output).ok()?;
    Some(std::env::split_paths(text.trim()).collect())
}

/// Drain while the child runs, including after it exits if descendants still own
/// its pipe. No blocking read or reader thread survives the operation deadline.
#[cfg(unix)]
fn path_output(mut command: std::process::Command, budget: Duration, limit: usize) -> Option<Vec<u8>> {
    use std::io::Read;
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;
    let started = Instant::now();
    let mut child = command.process_group(0)
        .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null())
        .spawn().ok()?;
    let group = child.id() as libc::pid_t;
    let mut completed = false;
    let result = (|| {
        let mut stdout = child.stdout.take()?;
        let fd = stdout.as_raw_fd();
        // SAFETY: fd is the live, owned pipe descriptor; no flags or other
        // processes are changed outside this newly spawned process group.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags == -1 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1 { return None; }
        let mut output = Vec::new();
        let mut eof = false;
        let mut status = None;
        let mut buffer = [0u8; 8192];
        loop {
            if started.elapsed() >= budget { return None; }
            if !eof {
                loop {
                    if started.elapsed() >= budget { return None; }
                    let capacity = buffer.len().min(limit.saturating_sub(output.len()).saturating_add(1));
                    match stdout.read(&mut buffer[..capacity]) {
                        Ok(0) => { eof = true; break; }
                        Ok(count) => {
                            output.extend_from_slice(&buffer[..count]);
                            if output.len() > limit { return None; }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => return None,
                    }
                }
            }
            if status.is_none() { status = child.try_wait().ok()?; }
            if eof {
                if let Some(status) = status {
                    completed = status.success();
                    return completed.then_some(output);
                }
            }
            std::thread::sleep(Duration::from_millis(5).min(budget.saturating_sub(started.elapsed())));
        }
    })();
    if !completed {
        // SAFETY: process_group(0) created a group whose ID is this exact
        // child's PID. Never signal the app's or another user's process group.
        unsafe { libc::kill(-group, libc::SIGKILL); }
        let _ = child.kill();
    }
    let _ = child.wait();
    result
}

#[cfg(all(not(windows), not(unix)))]
fn path_output(_: std::process::Command, _: Duration, _: usize) -> Option<Vec<u8>> { None }

#[cfg(test)]
#[path = "searchpath_tests.rs"]
mod focused_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_saved_path_from_reg_output() {
        let out = "\r\nHKEY_CURRENT_USER\\Environment\r\n    Path    REG_EXPAND_SZ    C:\\Users\\a\\.cargo\\bin;%LOCALAPPDATA%\\agy\\bin\r\n\r\n";
        assert_eq!(parse_reg(out).as_deref(), Some("C:\\Users\\a\\.cargo\\bin;%LOCALAPPDATA%\\agy\\bin"));
        assert_eq!(parse_reg("ERROR: The system was unable to find the specified registry key"), None);
    }

    #[test]
    fn expands_environment_names_and_leaves_unknown_ones() {
        std::env::set_var("CONSILIUM_TEST_DIR", "C:\\tools");
        assert_eq!(expand("%CONSILIUM_TEST_DIR%\\bin;%NO_SUCH_VAR_X%\\x;50%"), "C:\\tools\\bin;%NO_SUCH_VAR_X%\\x;50%");
    }

    #[test]
    fn includes_the_current_path_and_the_usual_tool_folders_once() {
        let d = dirs();
        let home = dirs::home_dir().unwrap();
        assert!(d.iter().any(|p| p == &home.join(".cargo/bin")));
        let n = d.len();
        let mut uniq = d.clone();
        uniq.dedup();
        assert_eq!(uniq.len(), n);
        assert!(joined().is_some());
    }
}
