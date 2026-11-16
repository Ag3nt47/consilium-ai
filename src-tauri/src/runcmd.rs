//! Run buttons on AI-suggested commands.
//!
//! An AI writes a fenced code block tagged as a shell (```bash, ```powershell, ```cmd …). The
//! screen shows it as a command box with Run, Deny and Copy. Run comes here with only the
//! message id and the block's position: the command text is read back from the stored message,
//! so the screen can never run text that no AI actually wrote. Each block runs at most once; the
//! human's "Ran command …" or "Denied command …" message carries a reference line that marks it
//! handled, which survives restarts.
//!
//! The command runs in the conversation's project folder, with no terminal to type into, and the
//! output goes back to the AI that suggested it as the human's message. Admin rights use the
//! computer's own prompt: the macOS administrator password box, `pkexec` on Linux, and the UAC
//! prompt from `Start-Process -Verb RunAs` on Windows. Consilium never sees a password.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::store::Event;

/// Longest a command may run before it is stopped.
const TIMEOUT: Duration = Duration::from_secs(600);
/// Output kept for the AI; longer output keeps its start and end.
const MAX_OUTPUT: usize = 12_000;

/// A fenced code block in a message: its language tag (lowercase, may be empty) and contents.
#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub lang: String,
    pub code: String,
    /// False when the message ended before the closing fence (a cut-off reply). Such a block is
    /// still listed, so later positions don't shift, but it can never be run.
    pub closed: bool,
}

/// Every fenced block in `text`, in order. Must match `fencedBlocks` in `src/commands.ts`.
pub fn fenced_blocks(text: &str) -> Vec<Block> {
    let mut out = Vec::new();
    let mut open: Option<(String, Vec<&str>)> = None;
    for line in text.split('\n') {
        let t = line.trim();
        match open.take() {
            None => {
                if let Some(rest) = t.strip_prefix("```") {
                    let lang = rest.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
                    open = Some((lang, Vec::new()));
                }
            }
            Some((lang, mut body)) => {
                if t == "```" {
                    out.push(Block { lang, code: body.join("\n"), closed: true });
                } else {
                    body.push(line);
                    open = Some((lang, body));
                }
            }
        }
    }
    if let Some((lang, body)) = open {
        out.push(Block { lang, code: body.join("\n"), closed: false });
    }
    out
}

/// The shell a block's language tag asks for, if it is one Run supports.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Shell {
    Posix,
    Zsh,
    Pwsh,
    Cmd,
}

pub fn shell_for(lang: &str) -> Option<Shell> {
    match lang {
        "bash" | "sh" | "shell" | "console" | "terminal" | "shell-session" => Some(Shell::Posix),
        "zsh" => Some(Shell::Zsh),
        "powershell" | "pwsh" | "ps1" | "ps" => Some(Shell::Pwsh),
        "cmd" | "bat" | "batch" | "bat-cmd" => Some(Shell::Cmd),
        _ => None,
    }
}

/// The reference line that ties the human's Ran/Denied message to one block.
pub fn reference(event_id: &str, index: usize) -> String {
    format!("(command ref {event_id}#{index})")
}

/// Whether a human message already ran or denied this block.
pub fn handled(events: &[Event], event_id: &str, index: usize) -> bool {
    let r = reference(event_id, index);
    events.iter().any(|e| e.sender == "human" && e.kind == "message" && e.text.contains(&r))
}

/// `console` blocks show a prompt before each command; drop it.
fn strip_prompts(code: &str) -> String {
    code.lines()
        .map(|l| {
            let t = l.trim_start();
            t.strip_prefix("$ ").or_else(|| t.strip_prefix("% ")).unwrap_or(l)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Lines that start with `sudo `, with that word removed; `None` when no line uses sudo.
fn without_sudo(code: &str) -> Option<String> {
    let mut any = false;
    let lines: Vec<String> = code
        .lines()
        .map(|l| {
            let indent = &l[..l.len() - l.trim_start().len()];
            match l.trim_start().strip_prefix("sudo ") {
                Some(rest) => {
                    any = true;
                    format!("{indent}{}", rest.trim_start())
                }
                None => l.to_string(),
            }
        })
        .collect();
    any.then(|| lines.join("\n"))
}

/// Refuses commands that would need a password typed into a terminal, or that carry a secret.
fn refuse(code: &str) -> Option<&'static str> {
    let lower = code.to_ascii_lowercase();
    if lower.contains("sudo -s") && code.contains("-S") || lower.contains("sudo --stdin") {
        return Some("it pipes a password into sudo; Consilium never handles passwords");
    }
    if crate::redact::scrub(code) != code {
        return Some("it contains what looks like a password, key or token");
    }
    None
}

/// What to start: the program, its arguments, and the script file's extension.
#[derive(Debug, PartialEq)]
struct Plan {
    program: String,
    args: Vec<String>,
    script: String,
    ext: &'static str,
}

/// How to run `code` in `shell` on this computer. `{script}` in args is the script file's path.
fn plan(shell: Shell, code: &str, os: &str) -> Result<Plan, String> {
    let code = if shell == Shell::Posix { strip_prompts(code) } else { code.to_string() };
    let p = |program: &str, args: &[&str], script: String, ext| Plan {
        program: program.into(),
        args: args.iter().map(|s| s.to_string()).collect(),
        script,
        ext,
    };
    match (shell, os) {
        (Shell::Pwsh, "windows") => Ok(p("powershell.exe", &["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", "{script}"], code, "ps1")),
        (Shell::Pwsh, _) => Ok(p("pwsh", &["-NoProfile", "-NonInteractive", "-File", "{script}"], code, "ps1")),
        (Shell::Cmd, "windows") => Ok(p("cmd.exe", &["/d", "/c", "{script}"], format!("@echo off\r\n{}", code.replace('\n', "\r\n")), "cmd")),
        (Shell::Cmd, _) => Err("this is a Windows Command Prompt command, and this computer isn't Windows".into()),
        (Shell::Posix | Shell::Zsh, "windows") => Err("this is a Mac/Linux terminal command; on Windows, ask for a PowerShell version".into()),
        (shell, os) => {
            let interp = if shell == Shell::Zsh { "/bin/zsh" } else { "/bin/bash" };
            match (without_sudo(&code), os) {
                // One macOS administrator password box for the whole script.
                (Some(script), "macos") => Ok(p("/usr/bin/osascript", &["-e", "{applescript}"], script, "sh")),
                // Linux: the graphical polkit prompt instead of a terminal password.
                (Some(_), _) => {
                    let script = code
                        .lines()
                        .map(|l| match l.trim_start().strip_prefix("sudo ") {
                            Some(rest) => format!("{}pkexec {}", &l[..l.len() - l.trim_start().len()], rest.trim_start()),
                            None => l.to_string(),
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    Ok(p(interp, &["{script}"], script, "sh"))
                }
                (None, _) => Ok(p(interp, &["{script}"], code, "sh")),
            }
        }
    }
}

fn applescript_quote(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Keeps the start and end of long output.
fn clip(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= MAX_OUTPUT {
        return text.to_string();
    }
    let head: String = chars[..MAX_OUTPUT / 3].iter().collect();
    let tail: String = chars[chars.len() - MAX_OUTPUT * 2 / 3..].iter().collect();
    format!("{head}\n… ({} characters left out) …\n{tail}", chars.len() - MAX_OUTPUT)
}

/// The outcome of one run.
pub struct Outcome {
    pub code: Option<i32>,
    pub output: String,
    pub elapsed: Duration,
    pub timed_out: bool,
}

/// Blocks currently running, so a double click can't start one twice.
static RUNNING: Mutex<Option<HashSet<String>>> = Mutex::new(None);

#[derive(Debug)]
pub struct RunGuard(String);
impl Drop for RunGuard {
    fn drop(&mut self) {
        if let Some(set) = RUNNING.lock().unwrap().as_mut() {
            set.remove(&self.0);
        }
    }
}

fn claim(key: &str) -> Option<RunGuard> {
    let mut guard = RUNNING.lock().unwrap();
    let set = guard.get_or_insert_with(HashSet::new);
    set.insert(key.to_string()).then(|| RunGuard(key.to_string()))
}

/// Everything that can refuse a block before anything starts.
pub fn check(block: &Block) -> Result<(), String> {
    if !block.closed {
        return Err("this command box was never closed (the message may have been cut off), so it can't be run".into());
    }
    let shell = shell_for(&block.lang).ok_or("only shell commands can be run")?;
    if block.code.trim().is_empty() {
        return Err("the command box is empty".into());
    }
    if let Some(why) = refuse(&block.code) {
        return Err(format!("Consilium won't run this command: {why}"));
    }
    plan(shell, &block.code, std::env::consts::OS).map(|_| ())
}

fn receipt_path(dir: &Path, conversation: &str, event_id: &str, index: usize) -> PathBuf {
    let safe = |s: &str| s.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect::<String>();
    dir.join(format!("{}_{}_{index}", safe(conversation), safe(event_id)))
}

/// Records on disk, before the command starts, that this block was started. Fails if it already
/// was, so a crash or restart mid-run can never lead to the same command running a second time.
pub fn mark_started(dir: &Path, conversation: &str, event_id: &str, index: usize) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("couldn't record the run: {e}"))?;
    let path = receipt_path(dir, conversation, event_id, index);
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            "this command was already started once (Consilium may have closed while it ran), so it won't run again; copy it to run it yourself".to_string()
        } else {
            format!("couldn't record the run: {e}")
        }
    })?;
    use std::io::Write;
    f.write_all(b"started").and_then(|_| f.sync_all()).map_err(|e| format!("couldn't record the run: {e}"))
}

/// Runs `block` in `cwd` and collects its output.
pub async fn run(block: &Block, cwd: &Path, scratch: &Path) -> Result<Outcome, String> {
    check(block)?;
    let shell = shell_for(&block.lang).ok_or("only shell commands can be run")?;
    let plan = plan(shell, &block.code, std::env::consts::OS)?;
    std::fs::create_dir_all(scratch).map_err(|e| format!("couldn't prepare the command: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(scratch, std::fs::Permissions::from_mode(0o700));
    }
    let file: PathBuf = scratch.join(format!("{}.{}", uuid::Uuid::new_v4(), plan.ext));
    std::fs::write(&file, &plan.script).map_err(|e| format!("couldn't prepare the command: {e}"))?;
    let path = file.to_string_lossy().to_string();
    let applescript = format!(
        "do shell script \"cd {} && /bin/bash {} 2>&1\" with administrator privileges",
        applescript_quote(&shell_quote(&cwd.to_string_lossy())),
        applescript_quote(&shell_quote(&path))
    );
    let args: Vec<String> = plan.args.iter().map(|a| a.replace("{script}", &path).replace("{applescript}", &applescript)).collect();

    let mut cmd = tokio::process::Command::new(&plan.program);
    cmd.args(&args).current_dir(cwd).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let started = Instant::now();
    let child = cmd.spawn().map_err(|e| format!("couldn't start {}: {e}", plan.program));
    let child = match child {
        Ok(c) => c,
        Err(e) => {
            let _ = std::fs::remove_file(&file);
            return Err(e);
        }
    };
    let pid = child.id();
    let result = tokio::time::timeout(TIMEOUT, child.wait_with_output()).await;
    let _ = std::fs::remove_file(&file);
    let elapsed = started.elapsed();
    match result {
        Ok(Ok(out)) => {
            let mut text = String::from_utf8_lossy(&out.stdout).to_string();
            let err = String::from_utf8_lossy(&out.stderr);
            if !err.trim().is_empty() {
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push_str(&err);
            }
            Ok(Outcome { code: out.status.code(), output: clip(&crate::redact::scrub(text.trim_end())), elapsed, timed_out: false })
        }
        Ok(Err(e)) => Err(format!("the command failed to run: {e}")),
        Err(_) => {
            if let Some(pid) = pid {
                kill_tree(pid);
            }
            Ok(Outcome { code: None, output: String::new(), elapsed, timed_out: true })
        }
    }
}

fn kill_tree(pid: u32) {
    #[cfg(unix)]
    unsafe {
        libc::killpg(pid as i32, libc::SIGKILL);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let _ = std::process::Command::new("taskkill").args(["/PID", &pid.to_string(), "/T", "/F"]).creation_flags(CREATE_NO_WINDOW).status();
    }
}

/// The human's message reporting a run back to the AI that suggested it.
pub fn report(who: &str, event_id: &str, index: usize, outcome: &Outcome) -> String {
    let status = if outcome.timed_out {
        format!("stopped after {} minutes without finishing", TIMEOUT.as_secs() / 60)
    } else {
        match outcome.code {
            Some(0) => "exit 0, succeeded".to_string(),
            Some(c) => format!("exit {c}, failed"),
            None => "ended by a signal".to_string(),
        }
    };
    let output = if outcome.output.trim().is_empty() { "(no output)".to_string() } else { outcome.output.replace("```", "``\u{200b}`") };
    format!(
        "Ran command from {who}'s message ({status}, {:.1} s):\n```text\n{output}\n```\n{}",
        outcome.elapsed.as_secs_f64(),
        reference(event_id, index)
    )
}

/// Finds block `index` of AI message `event_id`, checking it can still be run, and claims it.
pub fn take(events: &[Event], event_id: &str, index: usize) -> Result<(Event, Block, RunGuard), String> {
    let event = events.iter().find(|e| e.id == event_id).ok_or("that message isn't in this conversation")?;
    if event.sender == "human" || (event.kind != "message" && event.kind != "error") {
        return Err("only commands an AI suggested can be run from here".into());
    }
    let block = fenced_blocks(&event.text).into_iter().nth(index).ok_or("that command box isn't in the message")?;
    check(&block)?;
    if handled(events, event_id, index) {
        return Err("this command was already run or denied".into());
    }
    let guard = claim(&format!("{event_id}#{index}")).ok_or("this command is already running")?;
    Ok((event.clone(), block, guard))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(id: &str, sender: &str, text: &str) -> Event {
        serde_json::from_value(serde_json::json!({
            "id": id, "seq": 1, "ts": 0, "conversation": "c", "sender": sender, "kind": "message", "to": [], "text": text
        }))
        .unwrap()
    }

    #[test]
    fn finds_blocks_in_order_including_unclosed() {
        let text = "Try:\n```bash\nls -la\necho hi\n```\nthen\n```PowerShell extra\nGet-Date\n```\n```\nraw";
        let b = fenced_blocks(text);
        assert_eq!(b.len(), 3);
        assert_eq!(b[0], Block { lang: "bash".into(), code: "ls -la\necho hi".into(), closed: true });
        assert_eq!(b[1].lang, "powershell");
        assert_eq!(b[2], Block { lang: "".into(), code: "raw".into(), closed: false });
        assert_eq!(shell_for("ps1"), Some(Shell::Pwsh));
        assert_eq!(shell_for("text"), None);
    }

    #[test]
    fn plans_each_shell_per_system() {
        let mac = plan(Shell::Posix, "$ sudo softwareupdate -l\necho ok", "macos").unwrap();
        assert_eq!(mac.program, "/usr/bin/osascript");
        assert_eq!(mac.script, "softwareupdate -l\necho ok");
        let linux = plan(Shell::Posix, "  sudo apt update", "linux").unwrap();
        assert_eq!(linux.script, "  pkexec apt update");
        let plain = plan(Shell::Posix, "echo $HOME", "linux").unwrap();
        assert_eq!((plain.program.as_str(), plain.script.as_str()), ("/bin/bash", "echo $HOME"));
        assert_eq!(plan(Shell::Pwsh, "Get-Date", "windows").unwrap().program, "powershell.exe");
        assert!(plan(Shell::Cmd, "dir", "windows").unwrap().script.contains("dir"));
        assert!(plan(Shell::Cmd, "dir", "macos").is_err());
        assert!(plan(Shell::Posix, "ls", "windows").is_err());
    }

    #[test]
    fn refuses_password_pipes() {
        assert!(refuse("echo pw | sudo -S rm x").is_some());
        assert!(refuse("ls -la").is_none());
    }

    #[test]
    fn runs_once_and_only_ai_blocks() {
        let ai = ev("e1", "claude", "```bash\necho hi\n```");
        let human = ev("h1", "human", "```bash\necho hi\n```");
        let events = vec![ai.clone(), human];
        assert!(take(&events, "h1", 0).is_err());
        assert!(take(&events, "e1", 1).is_err());
        {
            let (_, block, _guard) = take(&events, "e1", 0).unwrap();
            assert_eq!(block.code, "echo hi");
            assert!(take(&events, "e1", 0).is_err(), "a second click while running is refused");
        }
        let done = ev("h2", "human", &format!("Denied command\n{}", reference("e1", 0)));
        assert!(take(&[ai, done], "e1", 0).is_err());
        let cut = ev("e2", "claude", "```bash\nrm -rf build\nmake");
        assert!(take(&[cut], "e2", 0).unwrap_err().contains("never closed"), "a cut-off block can't run");
    }

    #[test]
    fn a_started_command_never_starts_again() {
        let dir = std::env::temp_dir().join(format!("runcmd-receipt-{}", uuid::Uuid::new_v4()));
        mark_started(&dir, "conv", "e1", 0).unwrap();
        // Same block again, as after a crash and restart: refused.
        assert!(mark_started(&dir, "conv", "e1", 0).unwrap_err().contains("already started"));
        // Other blocks and conversations are unaffected; odd characters can't escape the folder.
        mark_started(&dir, "conv", "e1", 1).unwrap();
        mark_started(&dir, "other", "e1", 0).unwrap();
        mark_started(&dir, "../x", "e/1", 0).unwrap();
        assert!(dir.join("x_e1_0").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn runs_a_command_and_reports_it() {
        let dir = std::env::temp_dir().join(format!("runcmd-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let block = Block { lang: "sh".into(), code: "echo out\necho err >&2\nexit 3".into(), closed: true };
        let o = run(&block, &dir, &dir.join("scratch")).await.unwrap();
        assert_eq!(o.code, Some(3));
        assert!(o.output.contains("out") && o.output.contains("err"));
        let text = report("Claude", "e1", 0, &o);
        assert!(text.contains("exit 3, failed") && text.ends_with(&reference("e1", 0)));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
