//! Also records a fingerprint of the engine's sources, so the app can tell whether a page staged by
//! `scripts/stage-ui.mjs` was built against this engine. `scripts/backend-fingerprint.mjs` computes
//! the same value; keep the two in step.
use std::path::{Path, PathBuf};

const PARTS: [&str; 7] = [
    "src",
    "capabilities",
    "tauri.conf.json",
    "Cargo.toml",
    "Cargo.lock",
    "build.rs",
    "Info.plist",
];

fn files(p: &Path, out: &mut Vec<PathBuf>) {
    if p.is_dir() {
        for e in std::fs::read_dir(p).unwrap().flatten() {
            files(&e.path(), out);
        }
    } else if p.is_file() {
        out.push(p.to_path_buf());
    }
}

/// FNV-1a (64-bit) over "path\0contents\0" for each file in sorted order, with `/` separators and
/// the app's version line left out of tauri.conf.json (a version bump alone changes nothing).
fn fingerprint() -> String {
    let mut list = Vec::new();
    for part in PARTS {
        files(Path::new(part), &mut list);
    }
    let names: Vec<(String, PathBuf)> = list
        .into_iter()
        .map(|p| (p.to_string_lossy().replace('\\', "/"), p))
        .collect();
    hash(names, true)
}

/// The same FNV-1a over the built page in `../dist` (paths relative to it, `ui.json` left out), so
/// the app knows which page it carries and a staged copy of that same page isn't offered.
fn page_fingerprint() -> String {
    let root = Path::new("../dist");
    let mut list = Vec::new();
    files(root, &mut list);
    let names: Vec<(String, PathBuf)> = list
        .into_iter()
        .filter_map(|p| {
            let rel = p.strip_prefix(root).ok()?.to_string_lossy().replace('\\', "/");
            (rel != "ui.json").then_some((rel, p))
        })
        .collect();
    hash(names, false)
}

/// `text_eol`: CRLF becomes LF in text files (engine sources, which checkouts may convert); the
/// built page is hashed byte for byte.
fn hash(mut names: Vec<(String, PathBuf)>, text_eol: bool) -> String {
    names.sort();
    let mut h: u64 = 0xcbf29ce484222325;
    let mut feed = |bytes: &[u8]| {
        for b in bytes {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
    };
    for (name, path) in names {
        let mut body = std::fs::read(&path).unwrap();
        if text_eol && is_text(&body) {
            body = lf_only(&body);
        }
        if name == "tauri.conf.json" {
            let text = String::from_utf8_lossy(&body).into_owned();
            body = text
                .split('\n')
                .filter(|l| !l.trim_start().starts_with("\"version\""))
                .collect::<Vec<_>>()
                .join("\n")
                .into_bytes();
        }
        feed(name.as_bytes());
        feed(&[0]);
        feed(&body);
        feed(&[0]);
    }
    format!("{h:016x}")
}

/// Git's own test for text: no NUL byte in the first 8000 bytes. Binary files are never converted.
fn is_text(bytes: &[u8]) -> bool {
    !bytes[..bytes.len().min(8000)].contains(&0)
}

/// The bytes with every CRLF turned into LF. Windows checkouts (GitHub's Windows runners among them)
/// convert line endings, so without this the same commit fingerprints differently per checkout and
/// an instant page update is refused against an engine that is really the same.
fn lf_only(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'\r' && bytes.get(i + 1) == Some(&b'\n') {
            continue;
        }
        out.push(*b);
    }
    out
}

/// Output of `git <args>` in this folder, trimmed; None without git or outside a checkout.
fn git(args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git").args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string()).filter(|s| !s.is_empty())
}

/// Stamps the build with its number (commits in its history, so a later commit on the same
/// branch always has a higher one) and commit, so linked computers can tell two builds of one
/// version apart and never install an older one over a newer one (see `build_id`). A build made
/// with uncommitted changes is marked "+dirty": nobody else can rebuild it, so nobody follows it.
/// CI can set CONSILIUM_BUILD_NUMBER and CONSILIUM_COMMIT instead.
fn stamp_build() {
    println!("cargo:rerun-if-env-changed=CONSILIUM_BUILD_NUMBER");
    println!("cargo:rerun-if-env-changed=CONSILIUM_COMMIT");
    // Rebuilt when the checked-out commit changes.
    let head = git(&["rev-parse", "--git-path", "HEAD"]);
    let branch = git(&["rev-parse", "--symbolic-full-name", "HEAD"]).and_then(|r| git(&["rev-parse", "--git-path", &r]));
    for path in [head, branch].into_iter().flatten() {
        println!("cargo:rerun-if-changed={path}");
    }
    let number = std::env::var("CONSILIUM_BUILD_NUMBER")
        .ok()
        .or_else(|| git(&["rev-list", "--count", "HEAD"]))
        .unwrap_or_else(|| "0".into());
    let commit = std::env::var("CONSILIUM_COMMIT").ok().or_else(|| {
        let sha = git(&["rev-parse", "--short=12", "HEAD"])?;
        let dirty = git(&["status", "--porcelain", "--untracked-files=no"]).is_some();
        Some(if dirty { format!("{sha}+dirty") } else { sha })
    });
    println!("cargo:rustc-env=CONSILIUM_BUILD_NUMBER={number}");
    println!("cargo:rustc-env=CONSILIUM_COMMIT={}", commit.unwrap_or_default());
}

fn main() {
    stamp_build();
    for part in PARTS {
        println!("cargo:rerun-if-changed={part}");
    }
    println!("cargo:rerun-if-changed=../dist");
    println!("cargo:rustc-env=CONSILIUM_BACKEND_FINGERPRINT={}", fingerprint());
    println!("cargo:rustc-env=CONSILIUM_PAGE_FINGERPRINT={}", page_fingerprint());
    tauri_build::build()
}
