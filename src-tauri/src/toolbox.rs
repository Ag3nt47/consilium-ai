//! Tool acquisition for AIs with shell access.
//!
//! AIs may look for whatever tools, CLIs, MCP servers or skills a task needs, but installing
//! software onto the human's computer is a separate boundary: it needs one exact, approved
//! `REQUEST` carrying `INSTALL_COMMAND_JSON`, exactly like administrator elevation. This module
//! recognises system-wide installs, describes the package managers present on this host, and
//! tells the AIs how to ask.

use std::path::PathBuf;

pub const INSTALL_COMMAND_MARKER: &str = "INSTALL_COMMAND_JSON:";

/// Package managers and helper tools worth telling the AIs about, by OS.
fn known_tools(os: &str) -> &'static [&'static str] {
    match os {
        "windows" => &["winget", "scoop", "choco", "npm", "npx", "pip", "pipx", "uv", "cargo", "go", "gh", "git", "rea"],
        "macos" => &["brew", "npm", "npx", "pip3", "pipx", "uv", "cargo", "go", "gh", "git", "rea", "docker"],
        _ => &["apt", "dnf", "pacman", "zypper", "snap", "flatpak", "npm", "npx", "pip3", "pipx", "uv", "cargo", "go", "gh", "git", "rea", "docker"],
    }
}

/// Tools the human wants Consilium to know about but not ship: listed to every AI with shell
/// access so it can recognise when one fits and ask to install it. Nothing here is downloaded.
/// (name, binary to look for, source, what it is for and how to install it)
const CATALOG: &[(&str, &str, &str, &str)] = &[
    (
        "AnonRouter",
        "anonrouter",
        "https://anonrouter.ai",
        "privacy router (\"VPN for AI\") that relays requests to AI model APIs without exposing the caller; \
use it when the human wants AI traffic kept private. Python SDK: PyPI `anonrouter-confidential` (published \
by AnonRouter, source github.com/anonrouter/anonrouter-sdk); check for it with `pip3 show anonrouter-confidential` \
(it has no CLI, so the status here reads not installed) and install with \
`pip3 install --user anonrouter-confidential==0.1.3.post1`. Never send keys or the human's details to it \
without approval",
    ),
    (
        "Zero Breach Toolkit",
        "zero-breach",
        "https://github.com/interplains/Zero-Breach-Toolkit",
        "offline spyware and compromise scanner for macOS, Windows and Linux; run it only when the computer is \
idle, and pause it while AIs or builds are busy",
    ),
    (
        "Ghidra",
        "ghidraRun",
        "https://github.com/NationalSecurityAgency/ghidra",
        "decompiler REA uses for compiled native programs; request it only when a task needs native analysis",
    ),
    (
        "JADX",
        "jadx",
        "https://github.com/skylot/jadx",
        "Android APK and DEX decompiler REA uses; request it only when a task needs Android analysis",
    ),
];

fn catalog_note(dirs_: &[PathBuf]) -> String {
    let lines: Vec<String> = CATALOG
        .iter()
        .map(|(name, bin, source, about)| {
            let state = match present(bin, dirs_) {
                Some(path) => format!("installed at {}", path.display()),
                None => "not installed".to_string(),
            };
            format!("- {name} ({source}, {state}): {about}.")
        })
        .collect();
    format!(
        "\nCatalog of tools the human has asked Consilium to know about (not bundled; request an install with \
INSTALL_COMMAND_JSON when a task calls for one):\n{}",
        lines.join("\n")
    )
}

fn search_dirs() -> Vec<PathBuf> {
    let home = dirs::home_dir().unwrap_or_default();
    let mut dirs_: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    dirs_.extend(
        [".local/bin", ".local/node/bin", ".cargo/bin", ".npm-global/bin", ".volta/bin", "go/bin", "scoop/shims"]
            .iter()
            .map(|d| home.join(d)),
    );
    dirs_.extend(["/usr/local/bin", "/opt/homebrew/bin", "/usr/bin", "/snap/bin"].iter().map(PathBuf::from));
    if let Some(appdata) = std::env::var_os("APPDATA") {
        dirs_.push(PathBuf::from(appdata).join("npm"));
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        dirs_.push(PathBuf::from(local).join("Microsoft").join("WindowsApps"));
    }
    dirs_
}

fn present(name: &str, dirs_: &[PathBuf]) -> Option<PathBuf> {
    let names: Vec<String> = if cfg!(windows) {
        vec![format!("{name}.exe"), format!("{name}.cmd"), format!("{name}.bat")]
    } else {
        vec![name.to_string()]
    };
    dirs_.iter().flat_map(|d| names.iter().map(move |n| d.join(n))).find(|p| p.is_file())
}

/// The known tools installed on this host, with where they live. Cheap: a few `stat` calls.
pub fn inventory() -> Vec<(String, PathBuf)> {
    let dirs_ = search_dirs();
    known_tools(std::env::consts::OS)
        .iter()
        .filter_map(|name| present(name, &dirs_).map(|path| (name.to_string(), path)))
        .collect()
}

/// Prompt text for CLI-backed AIs with shell access.
pub fn capability(os: &str, installed: &[(String, PathBuf)]) -> String {
    let have = if installed.is_empty() {
        "none of the usual ones were found".to_string()
    } else {
        installed
            .iter()
            .map(|(name, path)| format!("{name} ({})", path.display()))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let missing: Vec<&str> = known_tools(os)
        .iter()
        .copied()
        .filter(|name| !installed.iter().any(|(have, _)| have == name))
        .collect();
    let missing = if missing.is_empty() { String::new() } else { format!(" Not installed: {}.", missing.join(", ")) };
    format!(
        "Tools and skills: seek out what you need. When a task would go better with a tool, CLI, library, MCP \
server or agent skill you don't have, look for it yourself (web search, GitHub, npm, PyPI, crates.io, Homebrew, \
winget, MCP registries) without asking; reading, searching and comparing need no approval. Prefer official, \
maintained, widely used sources; check the publisher, licence, recent releases and install scripts first. \
Project-local dependencies inside the project (npm install, cargo add, pip into the project's venv) are ordinary \
work. Installing anything onto the computer itself (global npm/pip/cargo/go/gem installs, brew, winget, scoop, \
choco, apt and similar, gh extensions, MCP servers or skills registered with an AI, curl|sh installers) needs one \
exact request first, in this form: `REQUEST: Install <tool> from <source> to <purpose>? INSTALL_COMMAND_JSON: \
<JSON string containing the exact command>`. Pin a version when you can and prefer flags that skip install \
scripts. Consilium blocks an unapproved install and lets the approved command run once, in the turn after the \
human's `Approved:`. After installing, verify the tool runs and record it with a REMEMBER: line (name, source, \
path). If an install needs administrator rights, use the elevated-command request instead.\n\
Package managers and tools on this {os} host: {have}.{missing}{catalog}",
        catalog = catalog_note(&search_dirs())
    )
}

fn words(segment: &str) -> Vec<String> {
    segment
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| matches!(c, '\'' | '"' | ',' | '(' | ')')).to_ascii_lowercase())
        .collect()
}

/// Whether one shell segment installs software system-wide (outside the project).
pub fn install_segment(segment: &str) -> bool {
    let words = words(segment);
    let mut start = 0;
    while start < words.len() && matches!(words[start].as_str(), "sudo" | "doas" | "env" | "command" | "gsudo") {
        start += 1;
        while start < words.len() && (words[start].starts_with('-') || words[start].contains('=')) {
            start += 1;
        }
    }
    let Some(first) = words.get(start) else { return false };
    let program = first.rsplit(['/', '\\']).next().unwrap_or(first);
    let program = program.trim_end_matches(".exe").trim_end_matches(".cmd");
    let args = &words[start + 1..];
    let sub = args.iter().find(|a| !a.starts_with('-')).map(String::as_str).unwrap_or("");
    let global = args.iter().any(|a| matches!(a.as_str(), "-g" | "--global" | "--location=global"));
    match program {
        "brew" => matches!(sub, "install" | "reinstall" | "tap" | "upgrade"),
        "npm" | "pnpm" => global && matches!(sub, "install" | "i" | "add" | "update" | "link"),
        "yarn" => sub == "global" && args.iter().any(|a| a == "add"),
        "bun" => global && matches!(sub, "add" | "install" | "i"),
        "pip" | "pip3" | "python" | "python3" | "py" => {
            let pip_args: &[String] = if matches!(program, "python" | "python3" | "py") {
                match args.iter().position(|a| a == "pip") {
                    Some(i) if args.get(i.saturating_sub(1)).is_some_and(|a| a == "-m") => &args[i + 1..],
                    _ => return false,
                }
            } else {
                args
            };
            pip_args.first().is_some_and(|a| a == "install")
                && !pip_args.iter().any(|a| matches!(a.as_str(), "-r" | "--requirement" | "-e" | "--editable" | "--target" | "-t"))
        }
        "pipx" => matches!(sub, "install" | "inject" | "upgrade"),
        "uv" => sub == "tool" && args.iter().any(|a| a == "install"),
        "cargo" => matches!(sub, "install" | "binstall"),
        "go" | "gem" => sub == "install",
        "winget" | "scoop" | "choco" | "snap" | "flatpak" | "port" => matches!(sub, "install" | "upgrade" | "add"),
        "apt" | "apt-get" | "dnf" | "yum" | "zypper" => matches!(sub, "install" | "reinstall"),
        "pacman" => args.iter().any(|a| a.starts_with("-s") && !a.starts_with("-ss") && !a.starts_with("-si")),
        "gh" => sub == "extension" && args.iter().any(|a| a == "install"),
        "claude" | "codex" | "agy" | "gemini" => {
            sub == "mcp" && args.iter().any(|a| a == "add" || a == "add-json")
                || sub == "plugin" && args.iter().any(|a| a == "install")
        }
        "rea" => sub == "setup",
        "install-module" | "install-package" => true,
        "powershell" | "pwsh" | "sh" | "bash" | "zsh" | "cmd" => args
            .iter()
            .position(|a| matches!(a.as_str(), "-c" | "/c" | "-command" | "-lc"))
            .is_some_and(|i| install_command_inner(&args[i + 1..].join(" "))),
        _ => false,
    }
}

fn install_command_inner(command: &str) -> bool {
    command.split(['&', ';', '\n', '|']).any(install_segment)
}

/// Whether a command installs software onto the computer (not a project-local dependency).
/// `curl … | sh` style installers count too.
pub fn install_command(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    let piped_installer = ["curl", "wget", "iwr", "invoke-webrequest", "irm", "invoke-restmethod"]
        .iter()
        .any(|fetch| lower.contains(fetch))
        && lower.split('|').skip(1).any(|part| {
            let w = words(part);
            w.first().is_some_and(|p| {
                let p = p.rsplit(['/', '\\']).next().unwrap_or(p);
                matches!(p, "sh" | "bash" | "zsh" | "iex" | "invoke-expression" | "python" | "python3")
            }) || w.iter().take(3).any(|p| p == "sudo") && w.iter().any(|p| matches!(p.as_str(), "sh" | "bash"))
        });
    piped_installer || install_command_inner(command)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_installs_need_approval_and_project_work_does_not() {
        for command in [
            "brew install ghidra",
            "brew install --cask jadx",
            "npm install -g rea-agents@6.9.0",
            "npm i --global typescript",
            "pip3 install requests",
            "python3 -m pip install frida-tools",
            "pipx install poetry",
            "uv tool install ruff",
            "cargo install ripgrep",
            "go install golang.org/x/tools/gopls@latest",
            "winget install --id Git.Git -e",
            "scoop install jq",
            "choco install -y nodejs",
            "sudo apt-get install -y jq",
            "sudo pacman -S jq",
            "gh extension install dlvhdr/gh-dash",
            "claude mcp add rea -- rea mcp",
            "rea setup --yes",
            "curl -fsSL https://example.com/install.sh | sh",
            "iwr https://example.com/install.ps1 | iex",
            "powershell -Command Install-Module PSReadLine",
            "bash -lc 'brew install jq'",
            "cd /tmp && cargo install sd",
        ] {
            assert!(install_command(command), "should need approval: {command}");
        }
        for command in [
            "npm install",
            "npm install react",
            "npm ci",
            "cargo add serde",
            "cargo build --release",
            "pip install -r requirements.txt",
            "pip install -e .",
            "brew search ghidra",
            "brew info jadx",
            "winget search Git",
            "npm view rea-agents version",
            "gh extension list",
            "curl -fsSL https://example.com/data.json -o data.json",
            "pacman -Ss jq",
            "claude mcp list",
        ] {
            assert!(!install_command(command), "ordinary work should be allowed: {command}");
        }
    }

    #[test]
    fn capability_names_the_request_form_and_the_host_tools() {
        let text = capability("macos", &[("brew".into(), PathBuf::from("/opt/homebrew/bin/brew"))]);
        assert!(text.contains(INSTALL_COMMAND_MARKER));
        assert!(text.contains("brew (/opt/homebrew/bin/brew)"));
        assert!(text.contains("Not installed:") && text.contains("pipx"));
        assert!(text.contains("without asking"));
        assert!(text.contains("AnonRouter (https://anonrouter.ai"));
        assert!(text.contains("not bundled"));
    }
}
