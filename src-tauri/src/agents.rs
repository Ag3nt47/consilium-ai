//! Local CLI agents: ChatGPT through OpenAI's Codex CLI, and Claude through Anthropic's Claude Code CLI.
//!
//! Both use the person's own sign-in to those tools, so Consilium never handles an API key or a
//! credential. Rules every run follows:
//! - the executable is started directly, never through a shell, so no text is interpreted as a command;
//! - it runs in a fresh, empty temporary folder that is deleted afterwards, so it sees no project files;
//! - tools are off (Claude: `--tools ""`; Codex: read-only sandbox in the empty folder);
//! - only documented non-interactive output is read (Codex `-o FILE`, Claude `--output-format json`);
//! - Stop, or the time limit, kills the whole process group.

use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

pub const RUN_TIMEOUT: Duration = Duration::from_secs(600);
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Codex,
    ClaudeCode,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentInfo {
    pub id: String,
    pub name: String,
    /// Always "local-cli" for now; API-backed agents will say "api".
    pub transport: String,
    pub tool: String,
    pub available: bool,
    pub signed_in: Option<bool>,
    pub version: Option<String>,
    pub path: Option<String>,
    /// What to do if it isn't usable yet.
    pub hint: String,
}

pub struct Agent {
    pub id: &'static str,
    pub name: &'static str,
    pub kind: Kind,
}

pub const AGENTS: [Agent; 2] = [
    Agent { id: "gpt", name: "ChatGPT", kind: Kind::Codex },
    Agent { id: "claude", name: "Claude", kind: Kind::ClaudeCode },
];

pub fn by_id(id: &str) -> Option<&'static Agent> {
    AGENTS.iter().find(|a| a.id == id)
}

fn exe_names(base: &str) -> Vec<String> {
    if cfg!(windows) {
        vec![format!("{base}.exe"), format!("{base}.cmd"), base.to_string()]
    } else {
        vec![base.to_string()]
    }
}

/// Apps started from the Dock or Start menu don't get the shell's PATH, so well-known install
/// locations are checked as well as PATH.
fn candidates(kind: Kind) -> Vec<PathBuf> {
    let home = dirs::home_dir().unwrap_or_default();
    let mut dirs_: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    dirs_.extend(
        [".local/bin", ".local/node/bin", ".npm-global/bin", ".volta/bin", "bin"]
            .iter()
            .map(|d| home.join(d)),
    );
    dirs_.extend(["/usr/local/bin", "/opt/homebrew/bin", "/usr/bin"].iter().map(PathBuf::from));
    if let Some(appdata) = std::env::var_os("APPDATA") {
        dirs_.push(PathBuf::from(appdata).join("npm"));
    }
    let base = match kind {
        Kind::Codex => "codex",
        Kind::ClaudeCode => "claude",
    };
    let mut out: Vec<PathBuf> = Vec::new();
    match kind {
        Kind::Codex => {
            // The ChatGPT desktop app ships a copy of Codex.
            for app in ["/Applications/ChatGPT.app", "/Applications/Codex.app"] {
                out.push(PathBuf::from(app).join("Contents/Resources/codex-cli/bin/codex"));
                out.push(home.join(app.trim_start_matches('/')).join("Contents/Resources/codex-cli/bin/codex"));
            }
            out.push(PathBuf::from("/Applications/Codex.app/Contents/Resources/codex"));
        }
        Kind::ClaudeCode => {
            out.push(home.join(".claude/local/claude"));
        }
    }
    for d in dirs_ {
        for n in exe_names(base) {
            out.push(d.join(n));
        }
    }
    out
}

pub fn find(kind: Kind) -> Option<PathBuf> {
    candidates(kind).into_iter().find(|p| is_executable(p))
}

fn is_executable(p: &Path) -> bool {
    match std::fs::metadata(p) {
        Ok(m) if m.is_file() => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                m.permissions().mode() & 0o111 != 0
            }
            #[cfg(not(unix))]
            {
                true
            }
        }
        _ => false,
    }
}

fn command(path: &Path) -> Command {
    let mut cmd = Command::new(path);
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0); // its own group, so Stop can kill everything it started
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Runs a short status command and returns its combined output, or None.
async fn probe(path: &Path, args: &[&str]) -> Option<String> {
    let mut cmd = command(path);
    cmd.args(args);
    let out = tokio::time::timeout(PROBE_TIMEOUT, cmd.output()).await.ok()?.ok()?;
    let mut s = String::from_utf8_lossy(&out.stdout).to_string();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    Some(s.trim().to_string())
}

pub async fn status(agent: &Agent) -> AgentInfo {
    let (tool, install_hint, login_hint) = match agent.kind {
        Kind::Codex => (
            "Codex CLI",
            "Install the ChatGPT desktop app (it includes Codex), or Codex from developers.openai.com/codex.",
            "Sign in to the ChatGPT desktop app, or run `codex login` in a terminal.",
        ),
        Kind::ClaudeCode => (
            "Claude Code CLI",
            "Install Claude Code from claude.com/claude-code.",
            "Run `claude auth login` in a terminal and sign in with your Claude account.",
        ),
    };
    let mut info = AgentInfo {
        id: agent.id.into(),
        name: agent.name.into(),
        transport: "local-cli".into(),
        tool: tool.into(),
        available: false,
        signed_in: None,
        version: None,
        path: None,
        hint: install_hint.into(),
    };
    let Some(path) = find(agent.kind) else { return info };
    info.path = Some(path.display().to_string());
    info.version = probe(&path, &["--version"]).await.and_then(|v| v.lines().next().map(str::to_string));
    let signed_in = match agent.kind {
        Kind::Codex => probe(&path, &["login", "status"]).await.map(|s| s.contains("Logged in")),
        Kind::ClaudeCode => probe(&path, &["auth", "status"]).await.and_then(|s| {
            serde_json::from_str::<serde_json::Value>(&s).ok().and_then(|v| v["loggedIn"].as_bool())
        }),
    };
    info.signed_in = signed_in;
    info.available = signed_in == Some(true);
    info.hint = if info.available { String::new() } else { login_hint.into() };
    info
}

/// Asks one agent for its next message. `system` holds Consilium's instructions and `prompt` the
/// conversation so far.
pub async fn run(
    agent: &Agent,
    system: &str,
    prompt: &str,
    cancel: &CancellationToken,
) -> Result<String, String> {
    let path = find(agent.kind).ok_or_else(|| format!("{} isn't installed", agent.name))?;
    run_at(&path, agent.kind, system, prompt, cancel, RUN_TIMEOUT).await
}

pub async fn run_at(
    path: &Path,
    kind: Kind,
    system: &str,
    prompt: &str,
    cancel: &CancellationToken,
    timeout: Duration,
) -> Result<String, String> {
    let work = std::env::temp_dir().join(format!("consilium-{}", uuid::Uuid::new_v4()));
    let empty = work.join("empty");
    std::fs::create_dir_all(&empty).map_err(|e| format!("couldn't create a work folder: {e}"))?;
    let result = run_in(path, kind, system, prompt, cancel, timeout, &work, &empty).await;
    let _ = std::fs::remove_dir_all(&work);
    result
}

#[allow(clippy::too_many_arguments)]
async fn run_in(
    path: &Path,
    kind: Kind,
    system: &str,
    prompt: &str,
    cancel: &CancellationToken,
    timeout: Duration,
    work: &Path,
    empty: &Path,
) -> Result<String, String> {
    let reply_file = work.join("reply.txt");
    let mut cmd = command(path);
    cmd.current_dir(empty).stdin(Stdio::piped());
    let input = match kind {
        Kind::Codex => {
            cmd.args(["exec", "--ephemeral", "--skip-git-repo-check", "-s", "read-only", "--color", "never", "-C"])
                .arg(empty)
                .arg("-o")
                .arg(&reply_file)
                .arg("-");
            format!("{system}\n\n{prompt}")
        }
        Kind::ClaudeCode => {
            cmd.args([
                "-p",
                "--output-format",
                "json",
                "--tools",
                "",
                "--no-session-persistence",
                "--strict-mcp-config",
                "--system-prompt",
            ])
            .arg(system);
            prompt.to_string()
        }
    };

    let mut child = cmd.spawn().map_err(|e| format!("couldn't start {}: {e}", path.display()))?;
    let pid = child.id();
    let mut stdin = child.stdin.take().expect("stdin is piped");
    let mut stdout = child.stdout.take().expect("stdout is piped");
    let mut stderr = child.stderr.take().expect("stderr is piped");
    let writer = tokio::spawn(async move {
        let _ = stdin.write_all(input.as_bytes()).await;
        let _ = stdin.shutdown().await;
    });
    let out_task = tokio::spawn(async move {
        let mut b = Vec::new();
        let _ = stdout.read_to_end(&mut b).await;
        b
    });
    let err_task = tokio::spawn(async move {
        let mut b = Vec::new();
        let _ = stderr.read_to_end(&mut b).await;
        b
    });

    let outcome = tokio::select! {
        status = child.wait() => Ok(status),
        _ = cancel.cancelled() => Err("stopped".to_string()),
        _ = tokio::time::sleep(timeout) => Err(format!("no answer within {} seconds", timeout.as_secs())),
    };
    let status = match outcome {
        Ok(s) => s.map_err(|e| e.to_string())?,
        Err(why) => {
            kill_tree(pid, &mut child).await;
            writer.abort();
            return Err(why);
        }
    };
    let _ = writer.await;
    let stdout = String::from_utf8_lossy(&out_task.await.unwrap_or_default()).to_string();
    let stderr = String::from_utf8_lossy(&err_task.await.unwrap_or_default()).to_string();

    let reply = match kind {
        Kind::Codex => std::fs::read_to_string(&reply_file).unwrap_or_default(),
        Kind::ClaudeCode => match serde_json::from_str::<serde_json::Value>(stdout.trim()) {
            Ok(v) => {
                let text = v["result"].as_str().unwrap_or_default().to_string();
                if v["is_error"].as_bool() == Some(true) {
                    return Err(tail(if text.is_empty() { &stderr } else { &text }));
                }
                text
            }
            Err(_) => String::new(),
        },
    };
    if !status.success() || reply.trim().is_empty() {
        let detail = if stderr.trim().is_empty() { stdout } else { stderr };
        return Err(format!("exit {}: {}", status.code().unwrap_or(-1), tail(&detail)));
    }
    Ok(reply.trim().to_string())
}

/// The last few lines of a tool's output, for error messages.
fn tail(s: &str) -> String {
    let lines: Vec<&str> = s.trim().lines().collect();
    let start = lines.len().saturating_sub(6);
    let t = lines[start..].join("\n");
    if t.is_empty() {
        "(no output)".into()
    } else {
        t
    }
}

async fn kill_tree(pid: Option<u32>, child: &mut tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pid) = pid {
        // The child leads its own process group (process_group(0)), so this reaches its children too.
        unsafe {
            libc::killpg(pid as i32, libc::SIGTERM);
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
        unsafe {
            libc::killpg(pid as i32, libc::SIGKILL);
        }
    }
    #[cfg(windows)]
    if let Some(pid) = pid {
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill().await;
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// Writes a fake CLI (a shell script) and returns its path.
    fn fake(body: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("consilium-fake-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("tool");
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[tokio::test]
    async fn codex_reply_comes_from_the_output_file_and_runs_in_an_empty_folder() {
        // Writes the reply to the file after -o, plus where it ran and what it was sent.
        let p = fake(
            r#"while [ "$1" != "-o" ]; do shift; done; out="$2"
input=$(cat)
printf 'cwd=%s files=%s first=%s' "$(basename "$PWD")" "$(ls -A | wc -l | tr -d ' ')" "$(printf '%s' "$input" | head -1)" > "$out""#,
        );
        let r = run_at(&p, Kind::Codex, "SYSTEM LINE", "convo", &CancellationToken::new(), RUN_TIMEOUT)
            .await
            .unwrap();
        assert_eq!(r, "cwd=empty files=0 first=SYSTEM LINE");
    }

    #[tokio::test]
    async fn claude_reply_is_parsed_from_json_and_errors_are_reported() {
        let p = fake(r#"cat >/dev/null; printf '{"type":"result","is_error":false,"result":"hello from claude"}'"#);
        let r = run_at(&p, Kind::ClaudeCode, "s", "p", &CancellationToken::new(), RUN_TIMEOUT).await;
        assert_eq!(r.unwrap(), "hello from claude");

        let p = fake(r#"cat >/dev/null; printf '{"type":"result","is_error":true,"result":"Not logged in"}'"#);
        let r = run_at(&p, Kind::ClaudeCode, "s", "p", &CancellationToken::new(), RUN_TIMEOUT).await;
        assert_eq!(r.unwrap_err(), "Not logged in");
    }

    #[tokio::test]
    async fn arguments_are_passed_without_a_shell() {
        // A system prompt full of shell syntax must arrive as one literal argument.
        let p = fake(r#"cat >/dev/null; for a in "$@"; do last="$a"; done; printf '{"result":"%s"}' "$last""#);
        let tricky = "x; touch /tmp/consilium-pwned $(id) `id`";
        let r = run_at(&p, Kind::ClaudeCode, tricky, "p", &CancellationToken::new(), RUN_TIMEOUT).await;
        assert_eq!(r.unwrap(), tricky);
        assert!(!Path::new("/tmp/consilium-pwned").exists());
    }

    #[tokio::test]
    async fn stop_kills_the_whole_process_group() {
        let marker = std::env::temp_dir().join(format!("consilium-child-{}", uuid::Uuid::new_v4()));
        // The tool starts a grandchild that would create a file after 2 seconds.
        let p = fake(&format!("(sleep 2; touch '{}') & sleep 30", marker.display()));
        let cancel = CancellationToken::new();
        let c2 = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            c2.cancel();
        });
        let started = std::time::Instant::now();
        let r = run_at(&p, Kind::Codex, "s", "p", &cancel, RUN_TIMEOUT).await;
        assert_eq!(r.unwrap_err(), "stopped");
        assert!(started.elapsed() < Duration::from_secs(3));
        tokio::time::sleep(Duration::from_millis(2500)).await;
        assert!(!marker.exists(), "the grandchild survived Stop");
    }

    #[tokio::test]
    async fn time_limit_applies() {
        let p = fake("sleep 30");
        let r = run_at(&p, Kind::Codex, "s", "p", &CancellationToken::new(), Duration::from_millis(500)).await;
        assert!(r.unwrap_err().contains("no answer within"));
    }

    #[tokio::test]
    async fn failures_report_the_tool_output() {
        let p = fake("cat >/dev/null; echo 'Error: not logged in' >&2; exit 3");
        let r = run_at(&p, Kind::Codex, "s", "p", &CancellationToken::new(), RUN_TIMEOUT).await;
        let e = r.unwrap_err();
        assert!(e.contains("exit 3") && e.contains("not logged in"), "{e}");
    }
}
