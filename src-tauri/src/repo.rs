//! GitHub repo ingest: a sandboxed shallow clone into Consilium's data folder, then a read-only
//! inspection (languages, size, licence, build files, risky spots). Nothing from the repo is
//! ever executed: git runs no hooks on clone, submodules, LFS smudging, local-file transports
//! and symlinks are switched off, and the walk below only reads metadata and a few small files.

use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const CLONE_TIMEOUT: Duration = Duration::from_secs(120);
/// Stop walking after this many files so a huge monorepo can't stall the app.
const MAX_FILES: usize = 50_000;

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct LangCount {
    pub language: String,
    pub files: u32,
    pub bytes: u64,
}

#[derive(Serialize, Debug, Clone, Default)]
pub struct RepoReport {
    /// "owner/name".
    pub slug: String,
    pub path: String,
    pub commit: Option<String>,
    pub files: u32,
    pub bytes: u64,
    /// True when the walk stopped at MAX_FILES.
    pub truncated: bool,
    pub languages: Vec<LangCount>,
    pub license: Option<String>,
    pub build_files: Vec<String>,
    pub risks: Vec<String>,
}

/// Accepts only https://github.com/<owner>/<name>[.git][/], or "owner/name". Returns (owner, name).
pub fn parse_github(input: &str) -> Result<(String, String), String> {
    let s = input.trim();
    let rest = s
        .strip_prefix("https://github.com/")
        .or_else(|| s.strip_prefix("github.com/"))
        .unwrap_or(s);
    let rest = rest.trim_end_matches('/');
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let mut parts = rest.split('/');
    let (Some(owner), Some(name), None) = (parts.next(), parts.next(), parts.next()) else {
        return Err("Use a GitHub link like https://github.com/owner/repo".into());
    };
    let ok = |p: &str| {
        !p.is_empty()
            && p.len() <= 100
            && !p.starts_with('.')
            && !p.starts_with('-')
            && p.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    };
    if !ok(owner) || !ok(name) || s.contains("://") && !s.starts_with("https://github.com/") {
        return Err("Only public github.com repositories over https are supported".into());
    }
    Ok((owner.to_string(), name.to_string()))
}

fn repos_dir() -> Result<PathBuf, String> {
    crate::data_dir().map(|d| d.join("repos")).ok_or_else(|| "No data folder".to_string())
}

fn run_git(args: &[&str], dir: Option<&Path>) -> Result<String, String> {
    let mut cmd = Command::new("git");
    if let Some(d) = dir {
        cmd.arg("-C").arg(d);
    }
    let mut child = cmd
        .args([
            "-c", "core.hooksPath=/dev/null",
            "-c", "core.symlinks=false",
            "-c", "protocol.file.allow=never",
            "-c", "protocol.ext.allow=never",
            "-c", "submodule.recurse=false",
            "-c", "credential.helper=",
        ])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_LFS_SKIP_SMUDGE", "1")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("git not available: {e}"))?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                let out = child.wait_with_output().map_err(|e| e.to_string())?;
                if out.status.success() {
                    return Ok(String::from_utf8_lossy(&out.stdout).trim().to_string());
                }
                let err = String::from_utf8_lossy(&out.stderr);
                return Err(crate::redact::scrub(err.lines().last().unwrap_or("git failed")));
            }
            Ok(None) if start.elapsed() < CLONE_TIMEOUT => std::thread::sleep(Duration::from_millis(100)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("git took longer than 2 minutes and was stopped".into());
            }
        }
    }
}

/// Clones (or refreshes) the repo into the data folder and inspects it.
pub fn ingest(url: &str) -> Result<RepoReport, String> {
    let (owner, name) = parse_github(url)?;
    let dest = repos_dir()?.join(format!("{owner}__{name}"));
    if dest.join(".git").is_dir() {
        // A fresh copy each time: simpler and safer than merging into a tree we don't trust.
        std::fs::remove_dir_all(&dest).map_err(|e| e.to_string())?;
    }
    std::fs::create_dir_all(dest.parent().unwrap()).map_err(|e| e.to_string())?;
    let remote = format!("https://github.com/{owner}/{name}.git");
    let dest_s = dest.to_string_lossy().to_string();
    run_git(
        &["clone", "--depth", "1", "--single-branch", "--no-recurse-submodules", "--", &remote, &dest_s],
        None,
    )?;
    let mut report = inspect(&dest);
    report.slug = format!("{owner}/{name}");
    report.commit = run_git(&["rev-parse", "--short", "HEAD"], Some(&dest)).ok();
    Ok(report)
}

fn language(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "rs" => "Rust",
        "ts" | "tsx" => "TypeScript",
        "js" | "jsx" | "mjs" | "cjs" => "JavaScript",
        "py" => "Python",
        "go" => "Go",
        "java" => "Java",
        "kt" | "kts" => "Kotlin",
        "swift" => "Swift",
        "c" | "h" => "C",
        "cc" | "cpp" | "cxx" | "hpp" => "C++",
        "cs" => "C#",
        "rb" => "Ruby",
        "php" => "PHP",
        "sh" | "bash" | "zsh" => "Shell",
        "ps1" => "PowerShell",
        "html" | "htm" => "HTML",
        "css" | "scss" | "sass" => "CSS",
        "md" => "Markdown",
        "lua" => "Lua",
        "dart" => "Dart",
        "zig" => "Zig",
        _ => return None,
    })
}

const BUILD_FILES: &[&str] = &[
    "package.json", "Cargo.toml", "go.mod", "pyproject.toml", "setup.py", "requirements.txt",
    "Makefile", "CMakeLists.txt", "build.gradle", "build.gradle.kts", "pom.xml", "Gemfile",
    "composer.json", "Dockerfile", "docker-compose.yml", "build.rs", "Package.swift",
];

const BINARY_EXTS: &[&str] = &["exe", "dll", "so", "dylib", "bin", "msi", "dmg", "pkg", "jar", "deb", "rpm"];

/// Read-only walk. Never follows symlinks.
pub fn inspect(root: &Path) -> RepoReport {
    let mut r = RepoReport { path: root.to_string_lossy().to_string(), ..Default::default() };
    let mut langs: HashMap<&'static str, (u32, u64)> = HashMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            let path = e.path();
            let rel = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
            let fname = e.file_name().to_string_lossy().to_string();
            if ft.is_symlink() {
                r.risks.push(format!("Symlink: {rel}"));
                continue;
            }
            if ft.is_dir() {
                if fname == ".git" || fname == "node_modules" {
                    continue;
                }
                if rel == ".github/workflows" {
                    r.risks.push("GitHub Actions workflows (.github/workflows) run code in CI".into());
                }
                stack.push(path);
                continue;
            }
            if r.files as usize >= MAX_FILES {
                r.truncated = true;
                continue;
            }
            let size = e.metadata().map(|m| m.len()).unwrap_or(0);
            r.files += 1;
            r.bytes += size;
            let ext = Path::new(&fname)
                .extension()
                .map(|x| x.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            if let Some(l) = language(&ext) {
                let c = langs.entry(l).or_default();
                c.0 += 1;
                c.1 += size;
            }
            let depth = rel.matches('/').count();
            if depth == 0 {
                let upper = fname.to_ascii_uppercase();
                if r.license.is_none() && (upper.starts_with("LICENSE") || upper.starts_with("LICENCE") || upper.starts_with("COPYING")) {
                    r.license = Some(license_name(&path).unwrap_or_else(|| fname.clone()));
                }
            }
            if depth <= 1 && BUILD_FILES.contains(&fname.as_str()) {
                r.build_files.push(rel.clone());
                if fname == "package.json" {
                    risky_npm_scripts(&path, &rel, &mut r.risks);
                }
                if fname == "setup.py" || fname == "build.rs" {
                    r.risks.push(format!("{rel} runs code at build/install time"));
                }
            }
            if BINARY_EXTS.contains(&ext.as_str()) {
                r.risks.push(format!("Prebuilt binary: {rel}"));
            }
            if size > 50 * 1024 * 1024 {
                r.risks.push(format!("Very large file ({} MB): {rel}", size / (1024 * 1024)));
            }
        }
    }
    let mut l: Vec<LangCount> = langs
        .into_iter()
        .map(|(k, (f, b))| LangCount { language: k.to_string(), files: f, bytes: b })
        .collect();
    l.sort_by(|a, b| b.bytes.cmp(&a.bytes).then(a.language.cmp(&b.language)));
    r.languages = l;
    r.build_files.sort();
    r.risks.sort();
    r.risks.truncate(50);
    r
}

fn small_read(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    (meta.len() <= 256 * 1024).then(|| std::fs::read_to_string(path).ok()).flatten()
}

fn license_name(path: &Path) -> Option<String> {
    let text = small_read(path)?;
    let head: String = text.chars().take(2000).collect();
    for (needle, name) in [
        ("MIT License", "MIT"),
        ("Apache License", "Apache-2.0"),
        ("GNU AFFERO GENERAL PUBLIC LICENSE", "AGPL"),
        ("GNU LESSER GENERAL PUBLIC LICENSE", "LGPL"),
        ("GNU GENERAL PUBLIC LICENSE", "GPL"),
        ("Mozilla Public License", "MPL-2.0"),
        ("BSD 3-Clause", "BSD-3-Clause"),
        ("BSD 2-Clause", "BSD-2-Clause"),
        ("The Unlicense", "Unlicense"),
        ("ISC License", "ISC"),
    ] {
        if head.contains(needle) {
            return Some(name.to_string());
        }
    }
    None
}

fn risky_npm_scripts(path: &Path, rel: &str, risks: &mut Vec<String>) {
    let Some(text) = small_read(path) else { return };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else { return };
    if let Some(scripts) = json.get("scripts").and_then(|s| s.as_object()) {
        for hook in ["preinstall", "install", "postinstall", "prepare"] {
            if scripts.contains_key(hook) {
                risks.push(format!("{rel} has an npm \"{hook}\" script that runs on install"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_only_github_https_links() {
        assert_eq!(parse_github("https://github.com/a-b/c.d.git").unwrap(), ("a-b".into(), "c.d".into()));
        assert_eq!(parse_github("owner/repo/").unwrap(), ("owner".into(), "repo".into()));
        assert!(parse_github("http://github.com/a/b").is_err());
        assert!(parse_github("https://evil.com/a/b").is_err());
        assert!(parse_github("file:///etc/a/b").is_err());
        assert!(parse_github("https://github.com/../b").is_err());
        assert!(parse_github("https://github.com/a/b/tree/main").is_err());
        assert!(parse_github("https://github.com/-a/b").is_err());
    }

    #[test]
    fn inspects_languages_licence_build_files_and_risks() {
        let dir = std::env::temp_dir().join(format!("consilium-repo-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join(".github/workflows")).unwrap();
        std::fs::write(dir.join("src/main.rs"), "fn main() {}").unwrap();
        std::fs::write(dir.join("index.ts"), "export {}").unwrap();
        std::fs::write(dir.join("LICENSE"), "MIT License\n...").unwrap();
        std::fs::write(dir.join("Cargo.toml"), "[package]").unwrap();
        std::fs::write(dir.join("package.json"), r#"{"scripts":{"postinstall":"node x.js"}}"#).unwrap();
        std::fs::write(dir.join("tool.exe"), "MZ").unwrap();
        let r = inspect(&dir);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(r.files, 6);
        assert_eq!(r.license.as_deref(), Some("MIT"));
        assert_eq!(r.build_files, vec!["Cargo.toml", "package.json"]);
        assert!(r.languages.iter().any(|l| l.language == "Rust"));
        assert!(r.risks.iter().any(|x| x.contains("postinstall")));
        assert!(r.risks.iter().any(|x| x.contains("tool.exe")));
        assert!(r.risks.iter().any(|x| x.contains("workflows")));
    }
}
