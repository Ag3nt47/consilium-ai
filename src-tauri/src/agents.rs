//! Local CLI agents: ChatGPT through OpenAI's Codex CLI, Claude through Anthropic's Claude Code CLI,
//! and Gemini through Google's Antigravity CLI (`agy`). Google stopped serving the Gemini CLI to
//! personal accounts in June 2026; it is still used for Gemini when `agy` isn't installed, for
//! company plans and API keys that keep access.
//!
//! All use the person's own sign-in to those tools, so Consilium never handles an API key or a
//! credential. Rules every run follows:
//! - the executable is started directly, never through a shell, so no text is interpreted as a command;
//! - only documented non-interactive output is read (Codex `-o FILE`, Claude and Gemini JSON);
//! - Stop, or the time limit, kills the whole process group.
//!
//! What the AI may touch depends on its access level (see `config::Access`). At "chat", or when the
//! conversation has no project folder, it runs in a fresh empty temporary folder with tools off.
//! Higher levels run it in the project folder with the matching permissions.

use crate::config::Access;
use serde::Serialize;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

pub const RUN_TIMEOUT: Duration = Duration::from_secs(900);
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Codex,
    ClaudeCode,
    GeminiCli,
    Antigravity,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentInfo {
    pub id: String,
    pub name: String,
    pub company: String,
    /// Always "local-cli" for now; API-backed agents will say "api".
    pub transport: String,
    pub tool: String,
    pub member: bool,
    pub access: Access,
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
    pub company: &'static str,
    pub kind: Kind,
}

pub const AGENTS: [Agent; 3] = [
    Agent { id: "gpt", name: "ChatGPT", company: "OpenAI", kind: Kind::Codex },
    Agent { id: "claude", name: "Claude", company: "Anthropic", kind: Kind::ClaudeCode },
    Agent { id: "gemini", name: "Gemini", company: "Google", kind: Kind::Antigravity },
];

pub fn by_id(id: &str) -> Option<&'static Agent> {
    AGENTS.iter().find(|a| a.id == id)
}

fn base_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Codex => "codex",
        Kind::ClaudeCode => "claude",
        Kind::GeminiCli => "gemini",
        Kind::Antigravity => "agy",
    }
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
        Kind::ClaudeCode => out.push(home.join(".claude/local/claude")),
        Kind::GeminiCli => {}
        Kind::Antigravity => {
            out.push(home.join(".local/bin/agy"));
            if let Some(local) = std::env::var_os("LOCALAPPDATA") {
                out.push(PathBuf::from(local).join("agy").join("bin").join("agy.exe"));
            }
        }
    }
    for d in dirs_ {
        for n in exe_names(base_name(kind)) {
            out.push(d.join(n));
        }
    }
    out
}

pub fn find(kind: Kind) -> Option<PathBuf> {
    candidates(kind).into_iter().find(|p| is_executable(p))
}

/// The tool an agent actually uses: Gemini runs through Antigravity when it's installed, and
/// falls back to the Gemini CLI (still served to company plans and API keys) when it isn't.
pub fn effective_kind(agent: &Agent) -> Kind {
    if agent.kind == Kind::Antigravity && find(Kind::Antigravity).is_none() && find(Kind::GeminiCli).is_some() {
        Kind::GeminiCli
    } else {
        agent.kind
    }
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
    // Node-based CLIs (Gemini, npm installs) start with `#!/usr/bin/env node`, and apps launched from
    // the Dock have a minimal PATH; putting the tool's own folder first lets it find its runtime.
    if let Some(dir) = path.parent() {
        let mut parts: Vec<PathBuf> = vec![dir.to_path_buf()];
        if let Some(existing) = std::env::var_os("PATH") {
            parts.extend(std::env::split_paths(&existing));
        }
        if let Ok(joined) = std::env::join_paths(parts) {
            cmd.env("PATH", joined);
        }
    }
    // Claude Code finds its saved sign-in by user name; some launchers start apps without USER.
    #[cfg(unix)]
    if std::env::var_os("USER").is_none() {
        if let Some(name) = dirs::home_dir().and_then(|h| h.file_name().map(|n| n.to_os_string())) {
            cmd.env("USER", &name).env("LOGNAME", &name);
        }
    }
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

/// Gemini CLI records the chosen sign-in method in ~/.gemini/settings.json; for a Google account
/// sign-in it also keeps a credentials file, whose presence (never its contents) is checked here.
fn gemini_signed_in() -> bool {
    let Some(home) = dirs::home_dir() else { return false };
    let dir = home.join(".gemini");
    let Ok(text) = std::fs::read_to_string(dir.join("settings.json")) else { return false };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { return false };
    let method = v["security"]["auth"]["selectedType"]
        .as_str()
        .or_else(|| v["selectedAuthType"].as_str())
        .unwrap_or("");
    match method {
        "" => false,
        "oauth-personal" => dir.join("oauth_creds.json").is_file(),
        "gemini-api-key" => std::env::var_os("GEMINI_API_KEY").is_some(),
        _ => true, // Vertex AI or another method configured in the CLI itself
    }
}

/// Antigravity has no "am I signed in?" command, and starting it while signed out opens a browser
/// sign-in page, so it is never launched just to check. Signing in creates a small marker file
/// (`antigravity/credential-user-scope` in the user config folder) next to the token it keeps in the
/// OS keyring; its presence, never its contents, is the check. Signing out (`/logout`) removes it.
fn antigravity_signed_in() -> bool {
    [dirs::config_dir(), dirs::data_dir()]
        .into_iter()
        .flatten()
        .any(|d| d.join("antigravity").join("credential-user-scope").is_file())
}

pub async fn status(agent: &Agent, member: bool, access: Access) -> AgentInfo {
    let kind = effective_kind(agent);
    let (tool, install_hint, login_hint) = match kind {
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
        Kind::GeminiCli => (
            "Gemini CLI",
            "Install the Gemini CLI from github.com/google-gemini/gemini-cli (needs Node.js).",
            "Run `gemini` in a terminal, choose Sign in with Google, then type /quit. (Personal Google accounts now need the Antigravity CLI instead.)",
        ),
        Kind::Antigravity => (
            "Antigravity CLI",
            "Install Google's Antigravity CLI from antigravity.google.",
            "Run `agy` in a terminal, choose Google OAuth and sign in in your browser, then type /quit.",
        ),
    };
    let mut info = AgentInfo {
        id: agent.id.into(),
        name: agent.name.into(),
        company: agent.company.into(),
        transport: "local-cli".into(),
        tool: tool.into(),
        member,
        access,
        available: false,
        signed_in: None,
        version: None,
        path: None,
        hint: install_hint.into(),
    };
    let Some(path) = find(kind) else { return info };
    info.path = Some(path.display().to_string());
    info.version = probe(&path, &["--version"]).await.and_then(|v| v.lines().next().map(str::to_string));
    let signed_in = match kind {
        Kind::Codex => probe(&path, &["login", "status"]).await.map(|s| s.contains("Logged in")),
        Kind::ClaudeCode => probe(&path, &["auth", "status"]).await.and_then(|s| {
            serde_json::from_str::<serde_json::Value>(&s).ok().and_then(|v| v["loggedIn"].as_bool())
        }),
        Kind::GeminiCli => Some(gemini_signed_in()),
        Kind::Antigravity => Some(antigravity_signed_in()),
    };
    info.signed_in = signed_in;
    info.available = signed_in == Some(true);
    info.hint = if info.available { String::new() } else { login_hint.into() };
    info
}

/// Where and how an agent runs for one turn.
pub struct Plan {
    pub args: Vec<OsString>,
    /// What is written to the tool's standard input.
    pub input: String,
}

/// The exact command-line arguments for one turn. `workdir` is the project folder when the AI has
/// access to one, otherwise an empty temporary folder; `access` is already "chat" in that case.
pub fn plan(kind: Kind, access: Access, workdir: &Path, reply_file: &Path, system: &str, prompt: &str) -> Plan {
    let mut args: Vec<OsString> = Vec::new();
    let mut push = |a: &str| args.push(a.into());
    let input = match kind {
        Kind::Codex => {
            push("exec");
            push("--ephemeral");
            push("--skip-git-repo-check");
            match access {
                Access::Chat | Access::Read => {
                    push("-s");
                    push("read-only");
                }
                Access::Edit => {
                    push("-s");
                    push("workspace-write");
                }
                Access::Own => {} // the person's own Codex configuration decides
            }
            push("--color");
            push("never");
            push("-C");
            args.push(workdir.into());
            args.push("-o".into());
            args.push(reply_file.into());
            args.push("-".into());
            format!("{system}\n\n{prompt}")
        }
        Kind::ClaudeCode => {
            for a in ["-p", "--output-format", "json", "--no-session-persistence"] {
                push(a);
            }
            match access {
                Access::Chat => {
                    push("--tools");
                    push("");
                    push("--strict-mcp-config");
                }
                Access::Read => {
                    push("--tools");
                    push("Read,Grep,Glob");
                    push("--strict-mcp-config");
                }
                Access::Edit => {
                    push("--tools");
                    push("Read,Grep,Glob,Edit,Write");
                    push("--permission-mode");
                    push("acceptEdits");
                    push("--strict-mcp-config");
                }
                Access::Own => {} // the person's own Claude Code settings and allow-lists decide
            }
            push("--append-system-prompt");
            args.push(system.into());
            prompt.to_string()
        }
        Kind::GeminiCli => {
            push("-p");
            push("");
            push("-o");
            push("json");
            push("--skip-trust");
            match access {
                Access::Chat | Access::Read => {
                    push("--approval-mode");
                    push("plan");
                }
                Access::Edit => {
                    push("--approval-mode");
                    push("auto_edit");
                }
                Access::Own => {} // the Gemini CLI's own approval settings decide
            }
            format!("{system}\n\n{prompt}")
        }
        Kind::Antigravity => {
            // The conversation goes over stdin as one stream-json message, not as an argument:
            // transcripts can exceed Linux's 128 KB argument and Windows' 32 K command-line limits.
            for a in ["--input-format", "stream-json", "--output-format", "stream-json", "--print-timeout", "0s"] {
                push(a);
            }
            match access {
                Access::Chat | Access::Read => {
                    push("--mode");
                    push("plan");
                }
                Access::Edit => {
                    push("--mode");
                    push("accept-edits");
                }
                Access::Own => {} // Antigravity's own permission settings decide
            }
            push("-p");
            push("");
            let message = serde_json::json!({
                "event": "user",
                "message": { "content": format!("{system}\n\n{prompt}") }
            });
            format!("{message}\n")
        }
    };
    Plan { args, input }
}

/// Asks one agent for its next message. `workspace` is the conversation's project folder, used
/// only when the agent's access is above "chat".
pub async fn run(
    agent: &Agent,
    access: Access,
    workspace: Option<&Path>,
    system: &str,
    prompt: &str,
    cancel: &CancellationToken,
) -> Result<String, String> {
    let kind = effective_kind(agent);
    let path = find(kind).ok_or_else(|| format!("{} isn't installed", agent.name))?;
    run_at(&path, kind, access, workspace, system, prompt, cancel, RUN_TIMEOUT).await
}

#[allow(clippy::too_many_arguments)]
pub async fn run_at(
    path: &Path,
    kind: Kind,
    access: Access,
    workspace: Option<&Path>,
    system: &str,
    prompt: &str,
    cancel: &CancellationToken,
    timeout: Duration,
) -> Result<String, String> {
    let work = std::env::temp_dir().join(format!("consilium-{}", uuid::Uuid::new_v4()));
    let empty = work.join("empty");
    std::fs::create_dir_all(&empty).map_err(|e| format!("couldn't create a work folder: {e}"))?;
    let (dir, access) = match workspace {
        Some(ws) if access != Access::Chat && ws.is_dir() => (ws.to_path_buf(), access),
        _ => (empty.clone(), Access::Chat),
    };
    let reply_file = work.join("reply.txt");
    let plan = plan(kind, access, &dir, &reply_file, system, prompt);
    let result = execute(path, kind, &dir, &reply_file, plan, cancel, timeout).await;
    let _ = std::fs::remove_dir_all(&work);
    result
}

async fn execute(
    path: &Path,
    kind: Kind,
    dir: &Path,
    reply_file: &Path,
    plan: Plan,
    cancel: &CancellationToken,
    timeout: Duration,
) -> Result<String, String> {
    let mut cmd = command(path);
    cmd.current_dir(dir).stdin(Stdio::piped()).args(&plan.args);
    let mut child = cmd.spawn().map_err(|e| format!("couldn't start {}: {e}", path.display()))?;
    let pid = child.id();
    let mut stdin = child.stdin.take().expect("stdin is piped");
    let mut stdout = child.stdout.take().expect("stdout is piped");
    let mut stderr = child.stderr.take().expect("stderr is piped");
    let input = plan.input;
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
        Kind::Codex => std::fs::read_to_string(reply_file).unwrap_or_default(),
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
        Kind::GeminiCli => match serde_json::from_str::<serde_json::Value>(stdout.trim()) {
            Ok(v) => {
                if let Some(msg) = v["error"]["message"].as_str() {
                    return Err(tail(msg));
                }
                v["response"].as_str().unwrap_or_default().to_string()
            }
            Err(_) => String::new(),
        },
        Kind::Antigravity => antigravity_reply(&stdout)?,
    };
    if !status.success() || reply.trim().is_empty() {
        let detail = if stderr.trim().is_empty() { stdout } else { stderr };
        return Err(format!("exit {}: {}", status.code().unwrap_or(-1), tail(&detail)));
    }
    Ok(reply.trim().to_string())
}

/// The reply from Antigravity's stream-json output: the final `result` event's response, or the
/// streamed text if that is empty. A run that ended without text because a tool it reached for was
/// blocked is reported as such, rather than as an empty answer.
fn antigravity_reply(stdout: &str) -> Result<String, String> {
    let mut streamed = String::new();
    let mut result = None;
    for line in stdout.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        if v["event"] == "step_update" {
            if let Some(t) = v["step_update"]["text_delta"].as_str() {
                streamed.push_str(t);
            }
        } else if v["event"] == "result" {
            result = Some(v["result"].clone());
        }
    }
    let Some(r) = result else { return Ok(streamed) };
    if r["status"] != "SUCCESS" {
        let why = r["error"].as_str().filter(|e| !e.is_empty()).unwrap_or("no response");
        return Err(tail(&format!("{} ({})", why, r["status"].as_str().unwrap_or("ERROR"))));
    }
    let text = r["response"].as_str().filter(|t| !t.trim().is_empty()).map(str::to_string).unwrap_or(streamed);
    if text.trim().is_empty() {
        if let Some(denied) = r["denied_actions"].as_array().filter(|d| !d.is_empty()) {
            let names: Vec<&str> = denied.iter().filter_map(|d| d["display_name"].as_str()).collect();
            return Err(format!(
                "Gemini tried to use a tool its access doesn't allow ({}) and wrote no reply. Ask again, or raise its access in Settings.",
                names.join(", ")
            ));
        }
    }
    Ok(text)
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

#[cfg(test)]
mod plan_tests {
    use super::*;

    fn args(kind: Kind, access: Access) -> Vec<String> {
        plan(kind, access, Path::new("/ws"), Path::new("/r.txt"), "SYS", "P")
            .args
            .iter()
            .map(|a| a.to_string_lossy().to_string())
            .collect()
    }

    fn has(v: &[String], pair: &[&str]) -> bool {
        v.windows(pair.len()).any(|w| w.iter().zip(pair).all(|(a, b)| a == b))
    }

    #[test]
    fn chat_access_turns_tools_off_everywhere() {
        assert!(has(&args(Kind::Codex, Access::Chat), &["-s", "read-only"]));
        assert!(has(&args(Kind::ClaudeCode, Access::Chat), &["--tools", ""]));
        assert!(has(&args(Kind::GeminiCli, Access::Chat), &["--approval-mode", "plan"]));
    }

    #[test]
    fn edit_access_allows_file_edits_but_not_shell() {
        assert!(has(&args(Kind::Codex, Access::Edit), &["-s", "workspace-write"]));
        let c = args(Kind::ClaudeCode, Access::Edit);
        assert!(has(&c, &["--tools", "Read,Grep,Glob,Edit,Write"]));
        assert!(has(&c, &["--permission-mode", "acceptEdits"]));
        assert!(!c.iter().any(|a| a.contains("Bash")));
        assert!(has(&args(Kind::GeminiCli, Access::Edit), &["--approval-mode", "auto_edit"]));
    }

    #[test]
    fn own_access_adds_no_permission_flags() {
        let x = args(Kind::Codex, Access::Own);
        assert!(!x.iter().any(|a| a == "-s"));
        let c = args(Kind::ClaudeCode, Access::Own);
        assert!(!c.iter().any(|a| a == "--tools" || a == "--permission-mode"));
        let g = args(Kind::GeminiCli, Access::Own);
        assert!(!g.iter().any(|a| a == "--approval-mode"));
    }

    #[test]
    fn antigravity_sends_the_conversation_on_stdin_with_matching_modes() {
        let p = plan(Kind::Antigravity, Access::Chat, Path::new("/ws"), Path::new("/r"), "SYS", "P");
        let a: Vec<String> = p.args.iter().map(|x| x.to_string_lossy().to_string()).collect();
        assert!(has(&a, &["--input-format", "stream-json"]) && has(&a, &["--output-format", "stream-json"]));
        assert!(has(&a, &["--mode", "plan"]) && has(&a, &["-p", ""]));
        let line: serde_json::Value = serde_json::from_str(p.input.trim()).unwrap();
        assert_eq!(line["event"], "user");
        assert_eq!(line["message"]["content"], "SYS\n\nP");
        assert!(has(&args(Kind::Antigravity, Access::Edit), &["--mode", "accept-edits"]));
        assert!(!args(Kind::Antigravity, Access::Own).iter().any(|x| x == "--mode"));
    }

    #[test]
    fn antigravity_reply_falls_back_to_streamed_text_and_explains_blocked_tools() {
        let streamed = r#"{"event":"step_update","step_update":{"text_delta":"Hello "}}
{"event":"step_update","step_update":{"text_delta":"council"}}
{"event":"result","result":{"status":"SUCCESS","response":""}}"#;
        assert_eq!(antigravity_reply(streamed).unwrap(), "Hello council");
        let blocked = r#"{"event":"result","result":{"status":"SUCCESS","response":"","denied_actions":[{"action":"command","display_name":"Bash"}]}}"#;
        let e = antigravity_reply(blocked).unwrap_err();
        assert!(e.contains("Bash") && e.contains("access"), "{e}");
        let failed = r#"{"event":"result","result":{"status":"ERROR","response":"","error":"authentication failed or timed out"}}"#;
        assert_eq!(antigravity_reply(failed).unwrap_err(), "authentication failed or timed out (ERROR)");
    }

    #[test]
    fn instructions_reach_every_tool() {
        let p = plan(Kind::ClaudeCode, Access::Chat, Path::new("/ws"), Path::new("/r"), "SYS", "P");
        assert!(p.args.iter().any(|a| a == "SYS") && p.input == "P");
        let p = plan(Kind::GeminiCli, Access::Chat, Path::new("/ws"), Path::new("/r"), "SYS", "P");
        assert!(p.input.starts_with("SYS") && p.input.ends_with("P"));
    }
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

    async fn go(p: &Path, kind: Kind, access: Access, ws: Option<&Path>, system: &str) -> Result<String, String> {
        run_at(p, kind, access, ws, system, "convo", &CancellationToken::new(), RUN_TIMEOUT).await
    }

    #[tokio::test]
    async fn codex_reply_comes_from_the_output_file_and_runs_in_an_empty_folder() {
        let p = fake(
            r#"while [ "$1" != "-o" ]; do shift; done; out="$2"
input=$(cat)
printf 'cwd=%s files=%s first=%s' "$(basename "$PWD")" "$(ls -A | wc -l | tr -d ' ')" "$(printf '%s' "$input" | head -1)" > "$out""#,
        );
        let r = go(&p, Kind::Codex, Access::Chat, None, "SYSTEM LINE").await.unwrap();
        assert_eq!(r, "cwd=empty files=0 first=SYSTEM LINE");
    }

    #[tokio::test]
    async fn project_folder_is_used_only_above_chat_access() {
        let ws = std::env::temp_dir().join(format!("consilium-ws-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(ws.join("README.md"), "x").unwrap();
        let p = fake(r#"while [ "$1" != "-o" ]; do shift; done; cat >/dev/null; ls -A > "$2""#);
        assert_eq!(go(&p, Kind::Codex, Access::Edit, Some(&ws), "s").await.unwrap(), "README.md");
        assert!(go(&p, Kind::Codex, Access::Chat, Some(&ws), "s").await.is_err(), "chat access must not see the project");
        std::fs::remove_dir_all(ws).unwrap();
    }

    #[tokio::test]
    async fn claude_reply_is_parsed_from_json_and_errors_are_reported() {
        let p = fake(r#"cat >/dev/null; printf '{"type":"result","is_error":false,"result":"hello from claude"}'"#);
        assert_eq!(go(&p, Kind::ClaudeCode, Access::Chat, None, "s").await.unwrap(), "hello from claude");
        let p = fake(r#"cat >/dev/null; printf '{"type":"result","is_error":true,"result":"Not logged in"}'"#);
        assert_eq!(go(&p, Kind::ClaudeCode, Access::Chat, None, "s").await.unwrap_err(), "Not logged in");
    }

    #[tokio::test]
    async fn gemini_reply_and_errors_are_parsed_from_json() {
        let p = fake(r#"cat >/dev/null; printf '{"session_id":"x","response":"hello from gemini","stats":{}}'"#);
        assert_eq!(go(&p, Kind::GeminiCli, Access::Chat, None, "s").await.unwrap(), "hello from gemini");
        let p = fake(r#"cat >/dev/null; printf '{"session_id":"x","error":{"type":"Error","message":"Please set an Auth method","code":41}}'; exit 41"#);
        assert_eq!(go(&p, Kind::GeminiCli, Access::Chat, None, "s").await.unwrap_err(), "Please set an Auth method");
    }

    #[tokio::test]
    async fn antigravity_result_event_is_parsed_and_errors_reported() {
        let p = fake(
            r#"cat >/dev/null
echo '{"event":"init","init":{"cwd":"x"}}'
echo '{"event":"result","result":{"status":"SUCCESS","response":"hello from gemini","error":""}}'"#,
        );
        assert_eq!(go(&p, Kind::Antigravity, Access::Chat, None, "s").await.unwrap(), "hello from gemini");
        let p = fake(
            r#"cat >/dev/null
echo '{"event":"result","result":{"status":"ERROR","response":"","error":"authentication failed or timed out"}}'
exit 1"#,
        );
        assert_eq!(
            go(&p, Kind::Antigravity, Access::Chat, None, "s").await.unwrap_err(),
            "authentication failed or timed out (ERROR)"
        );
    }

    #[tokio::test]
    async fn tool_folder_is_put_on_path_for_node_based_clis() {
        let p = fake(r#"cat >/dev/null; printf '{"response":"%s"}' "$(echo "$PATH" | cut -d: -f1)""#);
        let dir = p.parent().unwrap().display().to_string();
        assert_eq!(go(&p, Kind::GeminiCli, Access::Chat, None, "s").await.unwrap(), dir);
    }

    #[tokio::test]
    async fn arguments_are_passed_without_a_shell() {
        // A system prompt full of shell syntax must arrive as one literal argument.
        let p = fake(r#"cat >/dev/null; for a in "$@"; do last="$a"; done; printf '{"result":"%s"}' "$last""#);
        let tricky = "x; touch /tmp/consilium-pwned $(id) `id`";
        assert_eq!(go(&p, Kind::ClaudeCode, Access::Chat, None, tricky).await.unwrap(), tricky);
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
        let r = run_at(&p, Kind::Codex, Access::Chat, None, "s", "p", &cancel, RUN_TIMEOUT).await;
        assert_eq!(r.unwrap_err(), "stopped");
        assert!(started.elapsed() < Duration::from_secs(3));
        tokio::time::sleep(Duration::from_millis(2500)).await;
        assert!(!marker.exists(), "the grandchild survived Stop");
    }

    #[tokio::test]
    async fn time_limit_applies() {
        let p = fake("sleep 30");
        let r = run_at(&p, Kind::Codex, Access::Chat, None, "s", "p", &CancellationToken::new(), Duration::from_millis(500)).await;
        assert!(r.unwrap_err().contains("no answer within"));
    }

    #[tokio::test]
    async fn failures_report_the_tool_output() {
        let p = fake("cat >/dev/null; echo 'Error: not logged in' >&2; exit 3");
        let e = go(&p, Kind::Codex, Access::Chat, None, "s").await.unwrap_err();
        assert!(e.contains("exit 3") && e.contains("not logged in"), "{e}");
    }
}
