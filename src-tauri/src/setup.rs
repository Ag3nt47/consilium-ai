//! One-click setup for a missing or signed-out agent.
//!
//! Opens a terminal window that shows the exact official command, waits for the person to press
//! Enter, then runs it. Sign-ins happen in the official CLI's own flow (usually a browser page), so
//! Consilium never sees a password, token or key. Commands are fixed strings chosen here; nothing the
//! UI sends is ever put into a command line.

use crate::agents::{self, Kind};
use std::path::Path;

#[derive(Debug)]
pub struct Step {
    pub title: &'static str,
    pub command: String,
}

/// Single-quotes a path for a POSIX shell.
fn sh_quote(p: &Path) -> String {
    format!("'{}'", p.display().to_string().replace('\'', r"'\''"))
}

/// Finds npm (needed to install the Codex and Gemini CLIs) where agents::find looks for tools.
fn npm() -> Option<std::path::PathBuf> {
    let home = dirs::home_dir().unwrap_or_default();
    let mut dirs_: Vec<std::path::PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    dirs_.extend(
        [".local/node/bin", ".volta/bin", ".npm-global/bin"]
            .iter()
            .map(|d| home.join(d)),
    );
    dirs_.extend(
        ["/usr/local/bin", "/opt/homebrew/bin"]
            .iter()
            .map(std::path::PathBuf::from),
    );
    if let Some(pf) = std::env::var_os("ProgramFiles") {
        dirs_.push(std::path::PathBuf::from(pf).join("nodejs"));
    }
    let names: &[&str] = if cfg!(windows) {
        &["npm.cmd", "npm"]
    } else {
        &["npm"]
    };
    dirs_
        .into_iter()
        .flat_map(|d| names.iter().map(move |n| d.join(n)))
        .find(|p| p.is_file())
}

/// A command that runs `exe` with its own folder first on PATH, so Node-based tools
/// (`#!/usr/bin/env node`) find Node even when the person's terminal doesn't have it on PATH.
fn with_own_path(exe: &Path, args: &str) -> String {
    let dir = exe.parent().unwrap_or(Path::new("."));
    if cfg!(windows) {
        format!(
            "$env:Path = \"{};\" + $env:Path; & \"{}\" {args}",
            dir.display(),
            exe.display()
        )
    } else {
        format!("PATH={}:\"$PATH\" {} {args}", sh_quote(dir), sh_quote(exe))
    }
}

fn npm_install(package: &str) -> Result<String, String> {
    let npm = npm().ok_or("Node.js is needed first: install the LTS version from nodejs.org, then press Install again")?;
    Ok(with_own_path(&npm, &format!("install -g {package}")))
}

pub fn step(agent_id: &str, action: &str) -> Result<Step, String> {
    let agent = agents::by_id(agent_id).ok_or("unknown agent")?;
    let kind = agents::effective_kind(agent);
    let found = agents::find(kind);
    let windows = cfg!(windows);
    let step = match (kind, action) {
        (Kind::Codex, "signin") => {
            let exe = found.ok_or("Codex isn't installed yet")?;
            Step {
                title: "Sign in to ChatGPT (Codex)",
                command: if windows {
                    format!("& \"{}\" login", exe.display())
                } else {
                    format!("{} login", sh_quote(&exe))
                },
            }
        }
        (Kind::Codex, "install") => Step {
            title: "Install Codex (ChatGPT's command-line tool)",
            command: npm_install("@openai/codex")?,
        },
        (Kind::ClaudeCode, "signin") => {
            let exe = found.ok_or("Claude Code isn't installed yet")?;
            Step {
                title: "Sign in to Claude (Claude Code)",
                command: if windows {
                    format!("& \"{}\" auth login", exe.display())
                } else {
                    format!("{} auth login", sh_quote(&exe))
                },
            }
        }
        (Kind::ClaudeCode, "install") => Step {
            title: "Install Claude Code",
            command: if windows {
                "irm https://claude.ai/install.ps1 | iex".into()
            } else {
                "curl -fsSL https://claude.ai/install.sh | bash".into()
            },
        },
        (Kind::GeminiCli, "signin") => {
            let exe = found.ok_or("the Gemini CLI isn't installed yet")?;
            Step {
                title: "Sign in to Gemini: choose \"Sign in with Google\", then type /quit",
                command: with_own_path(&exe, ""),
            }
        }
        (Kind::GeminiCli, "install") => Step {
            title: "Install the Gemini CLI (Google's command-line tool)",
            command: npm_install("@google/gemini-cli")?,
        },
        (Kind::Antigravity, "signin") => {
            let exe = found.ok_or("the Antigravity CLI isn't installed yet")?;
            Step {
                title: "Sign in to Gemini (Antigravity): choose Google OAuth, sign in in the browser, then type /quit",
                command: if windows { format!("& \"{}\"", exe.display()) } else { sh_quote(&exe) },
            }
        }
        (Kind::Antigravity, "install") => Step {
            title: "Install Google's Antigravity CLI (for Gemini)",
            command: if windows {
                "irm https://antigravity.google/cli/install.ps1 | iex".into()
            } else {
                "curl -fsSL https://antigravity.google/cli/install.sh | bash".into()
            },
        },
        _ => return Err("unknown setup action".into()),
    };
    Ok(step)
}

/// Opens a terminal that explains the step, waits for Enter, and runs it.
pub fn open_in_terminal(step: &Step) -> Result<(), String> {
    let dir = std::env::temp_dir();
    #[cfg(target_os = "macos")]
    {
        let script = dir.join(format!("consilium-setup-{}.command", uuid::Uuid::new_v4()));
        std::fs::write(&script, posix_script(step)).map_err(|e| e.to_string())?;
        set_executable(&script)?;
        std::process::Command::new("open")
            .args(["-a", "Terminal"])
            .arg(&script)
            .spawn()
            .map_err(|e| format!("couldn't open Terminal: {e}"))?;
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    {
        let script = dir.join(format!("consilium-setup-{}.sh", uuid::Uuid::new_v4()));
        std::fs::write(&script, posix_script(step)).map_err(|e| e.to_string())?;
        set_executable(&script)?;
        for (term, flag) in [
            ("x-terminal-emulator", "-e"),
            ("gnome-terminal", "--"),
            ("konsole", "-e"),
            ("xterm", "-e"),
        ] {
            if std::process::Command::new(term)
                .arg(flag)
                .arg(&script)
                .spawn()
                .is_ok()
            {
                return Ok(());
            }
        }
        return Err(format!(
            "no terminal app found; run this yourself: {}",
            step.command
        ));
    }
    #[cfg(windows)]
    {
        let script = dir.join(format!("consilium-setup-{}.ps1", uuid::Uuid::new_v4()));
        let body = format!(
            "Write-Host 'Consilium setup: {title}'\nWrite-Host ''\nWrite-Host 'This will run:'\nWrite-Host '  {cmd_shown}'\nRead-Host 'Press Enter to continue, or close this window to cancel'\n{cmd}\nWrite-Host ''\nWrite-Host 'Done. Close this window and press refresh in Consilium.'\n",
            title = step.title.replace('\'', "''"),
            cmd_shown = step.command.replace('\'', "''"),
            cmd = step.command
        );
        std::fs::write(&script, body).map_err(|e| e.to_string())?;
        std::process::Command::new("cmd")
            .args([
                "/c",
                "start",
                "Consilium setup",
                "powershell",
                "-NoExit",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
            ])
            .arg(&script)
            .spawn()
            .map_err(|e| format!("couldn't open PowerShell: {e}"))?;
        return Ok(());
    }
    #[allow(unreachable_code)]
    Err("setup isn't supported on this system".into())
}

#[cfg(unix)]
fn posix_script(step: &Step) -> String {
    let shown = step.command.replace('\'', r"'\''");
    format!(
        "#!/bin/bash\nclear\necho 'Consilium setup: {title}'\necho\necho 'This will run:'\necho '  {shown}'\necho\nread -r -p 'Press Enter to continue, or close this window to cancel. ' _\n{cmd}\necho\necho 'Done. You can close this window and press refresh in Consilium.'\nrm -f \"$0\"\n",
        title = step.title.replace('\'', r"'\''"),
        cmd = step.command,
    )
}

#[cfg(unix)]
fn set_executable(p: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_steps_use_the_official_commands() {
        let s = step("claude", "install").unwrap();
        if cfg!(windows) {
            assert_eq!(s.command, "irm https://claude.ai/install.ps1 | iex");
        } else {
            assert_eq!(s.command, "curl -fsSL https://claude.ai/install.sh | bash");
        }
        // Codex installs through npm, run with Node's folder on PATH.
        if npm().is_some() {
            assert!(step("gpt", "install")
                .unwrap()
                .command
                .ends_with("install -g @openai/codex"));
        }
        // Gemini installs through Google's Antigravity installer (unless only the Gemini CLI is present).
        if agents::find(Kind::GeminiCli).is_none() || agents::find(Kind::Antigravity).is_some() {
            assert!(step("gemini", "install")
                .unwrap()
                .command
                .contains("antigravity.google/cli/install"));
        }
    }

    #[test]
    fn unknown_agents_and_actions_are_refused() {
        assert!(step("evil; rm -rf ~", "install").is_err());
        assert!(step("claude", "run-anything").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn node_tools_run_with_their_own_folder_on_path() {
        assert_eq!(
            with_own_path(Path::new("/x y/bin/gemini"), ""),
            r#"PATH='/x y/bin':"$PATH" '/x y/bin/gemini' "#
        );
    }

    #[cfg(unix)]
    #[test]
    fn paths_are_quoted_for_the_shell() {
        assert_eq!(sh_quote(Path::new("/a b/it's")), r"'/a b/it'\''s'");
    }
}
