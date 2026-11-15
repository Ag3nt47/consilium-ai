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
    hash(names)
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
    hash(names)
}

fn hash(mut names: Vec<(String, PathBuf)>) -> String {
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
        if name == "tauri.conf.json" {
            let text = String::from_utf8_lossy(&body).replace("\r\n", "\n");
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

fn main() {
    for part in PARTS {
        println!("cargo:rerun-if-changed={part}");
    }
    println!("cargo:rerun-if-changed=../dist");
    println!("cargo:rustc-env=CONSILIUM_BACKEND_FINGERPRINT={}", fingerprint());
    println!("cargo:rustc-env=CONSILIUM_PAGE_FINGERPRINT={}", page_fingerprint());
    tauri_build::build()
}
