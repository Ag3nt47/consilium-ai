//! Files the human attaches to a message: documents, pictures, code, anything.
//!
//! Each file is copied into the conversation's own folder (`<id>.files` next to its log), so the
//! conversation keeps working if the original moves. How the AIs get it depends on what it is:
//! - Text (code, notes, CSV, JSON, …) and documents whose text can be read out (Word .docx,
//!   OpenDocument .odt, PowerPoint .pptx) go into the transcript itself, so every AI reads them,
//!   whatever its access level.
//! - Pictures and PDFs are given to the AIs that can open them: Codex as images on the command
//!   line, Claude Code and Antigravity by letting them read the conversation's files folder.
//! - Anything else is named with its location, for an AI with access to open it.

use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::{Path, PathBuf};

/// Largest file that can be attached.
pub const MAX_BYTES: u64 = 50 * 1024 * 1024;
/// Most text of one attachment put into the transcript.
const MAX_INLINE_CHARS: usize = 40_000;
/// Most pictures handed to Codex at once (the newest).
const MAX_IMAGES: usize = 8;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Attachment {
    /// The file's name as attached.
    pub name: String,
    /// Consilium's copy, in the conversation's files folder.
    pub path: String,
    /// "image", "pdf", "text" (readable as text, including text read out of a document), or "file".
    pub kind: String,
    pub size: u64,
    /// For "text": the text the AIs read (the copy itself, or text read out of a document).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_path: Option<String>,
}

const IMAGE: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp", "heic", "heif", "tif", "tiff"];
const TEXT: &[&str] = &[
    "txt", "md", "markdown", "rst", "csv", "tsv", "json", "jsonl", "ndjson", "xml", "yaml", "yml", "toml", "ini", "cfg", "conf",
    "log", "html", "htm", "css", "scss", "js", "mjs", "cjs", "ts", "tsx", "jsx", "py", "rs", "go", "java", "kt", "kts", "c", "h",
    "cc", "cpp", "hpp", "cs", "rb", "php", "sh", "bash", "zsh", "fish", "ps1", "bat", "sql", "swift", "m", "mm", "scala", "r",
    "lua", "pl", "vue", "svelte", "dart", "ex", "exs", "erl", "hs", "clj", "graphql", "proto", "tex", "srt", "vtt", "env", "gitignore",
    "dockerfile", "makefile",
];

fn extension(name: &str) -> String {
    Path::new(name).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase()
}

/// A file name that is safe to use in the files folder: no folders, no odd characters.
fn safe_name(name: &str) -> String {
    let base = Path::new(name).file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let cleaned: String = base
        .chars()
        .map(|c| if c.is_alphanumeric() || " ._-()".contains(c) { c } else { '_' })
        .collect::<String>()
        .trim_matches(|c: char| c == '.' || c.is_whitespace())
        .chars()
        .take(120)
        .collect();
    if cleaned.is_empty() {
        "file".into()
    } else {
        cleaned
    }
}

/// Copies `bytes`, attached as `name`, into `dir`, and works out how the AIs can read it.
pub fn save(dir: &Path, name: &str, bytes: &[u8]) -> Result<Attachment, String> {
    if bytes.len() as u64 > MAX_BYTES {
        return Err(format!("{name} is larger than {} MB", MAX_BYTES / (1024 * 1024)));
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("couldn't create {}: {e}", dir.display()))?;
    let name = safe_name(name);
    let id = uuid::Uuid::new_v4().simple().to_string();
    let path = dir.join(format!("{}-{name}", &id[..8]));
    std::fs::write(&path, bytes).map_err(|e| format!("couldn't save {name}: {e}"))?;
    let ext = extension(&name);
    let (kind, text_path) = if IMAGE.contains(&ext.as_str()) {
        ("image", None)
    } else if ext == "pdf" {
        ("pdf", None)
    } else if let Some(text) = document_text(&ext, bytes) {
        let t = path.with_extension(format!("{ext}.txt"));
        std::fs::write(&t, text).map_err(|e| format!("couldn't save the text of {name}: {e}"))?;
        ("text", Some(t))
    } else if TEXT.contains(&ext.as_str()) || (bytes.len() <= 1024 * 1024 && looks_like_text(bytes)) {
        ("text", Some(path.clone()))
    } else {
        ("file", None)
    };
    Ok(Attachment {
        name,
        path: path.display().to_string(),
        kind: kind.into(),
        size: bytes.len() as u64,
        text_path: text_path.map(|p| p.display().to_string()),
    })
}

/// Attaches the file at `source`.
pub fn save_file(dir: &Path, source: &Path) -> Result<Attachment, String> {
    let meta = std::fs::metadata(source).map_err(|_| format!("{} doesn't exist", source.display()))?;
    if !meta.is_file() {
        return Err(format!("{} isn't a file", source.display()));
    }
    if meta.len() > MAX_BYTES {
        return Err(format!("{} is larger than {} MB", source.display(), MAX_BYTES / (1024 * 1024)));
    }
    let bytes = std::fs::read(source).map_err(|e| format!("couldn't read {}: {e}", source.display()))?;
    let name = source.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    save(dir, name, &bytes)
}

/// Whether `a` is one of this conversation's own attachments (the UI and the terminal hand them
/// back when sending; nothing outside the files folder is accepted).
pub fn belongs(dir: &Path, a: &Attachment) -> bool {
    let (Ok(dir), Ok(file)) = (std::fs::canonicalize(dir), std::fs::canonicalize(&a.path)) else { return false };
    let text_ok = a.text_path.as_ref().is_none_or(|t| std::fs::canonicalize(t).is_ok_and(|t| t.starts_with(&dir)));
    file.starts_with(&dir) && text_ok
}

fn looks_like_text(bytes: &[u8]) -> bool {
    !bytes.is_empty() && !bytes.contains(&0) && std::str::from_utf8(bytes).is_ok()
}

/// The text of a Word, OpenDocument or PowerPoint file.
fn document_text(ext: &str, bytes: &[u8]) -> Option<String> {
    let parts: Vec<String> = match ext {
        "docx" | "docm" => vec!["word/document.xml".into()],
        "odt" | "odp" => vec!["content.xml".into()],
        "pptx" => {
            let mut slides: Vec<(u32, String)> = zip_names(bytes)?
                .into_iter()
                .filter_map(|n| {
                    let num = n.strip_prefix("ppt/slides/slide")?.strip_suffix(".xml")?.parse().ok()?;
                    Some((num, n))
                })
                .collect();
            slides.sort();
            slides.into_iter().map(|(_, n)| n).collect()
        }
        _ => return None,
    };
    let mut out = String::new();
    for (i, part) in parts.iter().enumerate() {
        let xml = zip_file(bytes, part)?;
        if ext == "pptx" {
            out.push_str(&format!("--- Slide {} ---\n", i + 1));
        }
        out.push_str(&xml_text(&String::from_utf8_lossy(&xml)));
        out.push('\n');
    }
    let text = out.trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// Text from an office document's XML: paragraphs and line breaks kept, tags dropped.
fn xml_text(xml: &str) -> String {
    let mut out = String::with_capacity(xml.len() / 4);
    let mut rest = xml;
    while let Some(lt) = rest.find('<') {
        out.push_str(&unescape(&rest[..lt]));
        let Some(gt) = rest[lt..].find('>') else { break };
        let tag = &rest[lt + 1..lt + gt];
        let name = tag.trim_start_matches('/').split(|c: char| c.is_whitespace() || c == '/').next().unwrap_or("");
        let closing = tag.starts_with('/');
        match name {
            // Word, OpenDocument and PowerPoint paragraphs.
            "w:p" | "text:p" | "text:h" | "a:p" if closing => out.push('\n'),
            "w:br" | "w:cr" | "text:line-break" | "a:br" => out.push('\n'),
            "w:tab" | "text:tab" => out.push('\t'),
            _ => {}
        }
        rest = &rest[lt + gt + 1..];
    }
    out.push_str(&unescape(rest));
    // No runs of blank lines.
    let mut tidy = String::new();
    let mut blank = 0;
    for line in out.lines() {
        if line.trim().is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        tidy.push_str(line.trim_end());
        tidy.push('\n');
    }
    tidy
}

fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp..];
        let Some(semi) = after.find(';').filter(|&i| i <= 10) else {
            out.push('&');
            rest = &after[1..];
            continue;
        };
        let entity = &after[1..semi];
        let ch = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            e if e.starts_with("#x") || e.starts_with("#X") => u32::from_str_radix(&e[2..], 16).ok().and_then(char::from_u32),
            e if e.starts_with('#') => e[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &after[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &after[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

// A minimal zip reader (office documents are zip files): the central directory lists each entry's
// name, compression, size and where its data starts.

fn u16_at(b: &[u8], i: usize) -> Option<usize> {
    Some(u16::from_le_bytes(b.get(i..i + 2)?.try_into().ok()?) as usize)
}

fn u32_at(b: &[u8], i: usize) -> Option<usize> {
    Some(u32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?) as usize)
}

/// (name, method, compressed size, local header offset) for each entry.
fn zip_entries(b: &[u8]) -> Option<Vec<(String, usize, usize, usize)>> {
    // The end-of-central-directory record is in the last 64 KB + 22 bytes.
    let from = b.len().saturating_sub(65_557);
    let eocd = (from..b.len().saturating_sub(21)).rev().find(|&i| b[i..].starts_with(&[0x50, 0x4b, 0x05, 0x06]))?;
    let count = u16_at(b, eocd + 10)?;
    let mut at = u32_at(b, eocd + 16)?;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        if !b.get(at..)?.starts_with(&[0x50, 0x4b, 0x01, 0x02]) {
            return None;
        }
        let method = u16_at(b, at + 10)?;
        let compressed = u32_at(b, at + 20)?;
        let (name_len, extra_len, comment_len) = (u16_at(b, at + 28)?, u16_at(b, at + 30)?, u16_at(b, at + 32)?);
        let offset = u32_at(b, at + 42)?;
        let name = String::from_utf8_lossy(b.get(at + 46..at + 46 + name_len)?).to_string();
        out.push((name, method, compressed, offset));
        at += 46 + name_len + extra_len + comment_len;
    }
    Some(out)
}

fn zip_names(b: &[u8]) -> Option<Vec<String>> {
    Some(zip_entries(b)?.into_iter().map(|e| e.0).collect())
}

/// The contents of entry `name`, at most 20 MB uncompressed.
fn zip_file(b: &[u8], name: &str) -> Option<Vec<u8>> {
    let (_, method, compressed, offset) = zip_entries(b)?.into_iter().find(|e| e.0 == name)?;
    if !b.get(offset..)?.starts_with(&[0x50, 0x4b, 0x03, 0x04]) {
        return None;
    }
    let start = offset + 30 + u16_at(b, offset + 26)? + u16_at(b, offset + 28)?;
    let data = b.get(start..start + compressed)?;
    match method {
        0 => Some(data.to_vec()),
        8 => {
            let mut out = Vec::new();
            flate2::read::DeflateDecoder::new(data).take(20 * 1024 * 1024).read_to_end(&mut out).ok()?;
            Some(out)
        }
        _ => None,
    }
}

fn size(bytes: u64) -> String {
    match bytes {
        b if b >= 1024 * 1024 => format!("{:.1} MB", b as f64 / (1024.0 * 1024.0)),
        b if b >= 1024 => format!("{} KB", b / 1024),
        b => format!("{b} bytes"),
    }
}

/// Whether AI `me` can look at pictures and PDFs here: Codex gets pictures on its command line and
/// Claude Code reads both with its Read tool; Antigravity has no supported way to take a picture in.
fn can_view(me: &str) -> bool {
    me != "gemini"
}

/// How a message's attachments read in AI `me`'s transcript.
pub fn for_transcript(attachments: &[Attachment], me: &str) -> String {
    let mut out = String::new();
    let cant = |what: &str, a: &Attachment| {
        format!(
            "\n\n[Attached {what}: {} ({}). You can't open {what}s in Consilium, so don't try (no shell commands for it); \
if it matters, say so and let another AI describe it.]",
            a.name,
            size(a.size)
        )
    };
    for a in attachments {
        match a.kind.as_str() {
            "text" => {
                let text = a.text_path.as_ref().and_then(|p| std::fs::read(p).ok()).map(|b| String::from_utf8_lossy(&b).to_string());
                match text {
                    Some(t) => {
                        let shown: String = t.chars().take(MAX_INLINE_CHARS).collect();
                        let cut = if shown.len() < t.len() {
                            format!("\n[… cut here; the whole file is at {}]", a.text_path.as_deref().unwrap_or(&a.path))
                        } else {
                            String::new()
                        };
                        out.push_str(&format!(
                            "\n\n[Attached file: {} ({})]\n<<<FILE {}\n{shown}{cut}\nFILE>>>",
                            a.name,
                            size(a.size),
                            a.name
                        ));
                    }
                    None => out.push_str(&format!("\n\n[Attached file: {} ({}) — no longer available]", a.name, size(a.size))),
                }
            }
            "image" if !can_view(me) => out.push_str(&cant("picture", a)),
            "pdf" if !can_view(me) => out.push_str(&cant("PDF", a)),
            "image" => out.push_str(&format!(
                "\n\n[Attached picture: {} ({}), at {}. View it with your file-reading tool (not a shell command); if you can't view pictures, say so.]",
                a.name,
                size(a.size),
                a.path
            )),
            "pdf" => out.push_str(&format!(
                "\n\n[Attached PDF: {} ({}), at {}. Read it with your file-reading tool (not a shell command); if you can't, say so.]",
                a.name,
                size(a.size),
                a.path
            )),
            _ => out.push_str(&format!(
                "\n\n[Attached file: {} ({}), at {}. Open it if you can; it isn't plain text.]",
                a.name,
                size(a.size),
                a.path
            )),
        }
    }
    out
}

/// What an AI needs to open a conversation's attachments: the folder to let it read, and the
/// newest pictures (for Codex, which takes pictures on its command line).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Files {
    pub dir: Option<PathBuf>,
    pub images: Vec<PathBuf>,
}

pub fn files_for(events: &[crate::store::Event], dir: &Path) -> Files {
    let all: Vec<&Attachment> = events.iter().flat_map(|e| e.attachments.iter()).collect();
    if all.is_empty() {
        return Files::default();
    }
    let mut images: Vec<PathBuf> =
        all.iter().rev().filter(|a| a.kind == "image").map(|a| PathBuf::from(&a.path)).filter(|p| p.is_file()).take(MAX_IMAGES).collect();
    images.reverse();
    Files { dir: Some(dir.to_path_buf()).filter(|d| d.is_dir()), images }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> PathBuf {
        let d = std::env::temp_dir().join(format!("consilium-attach-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A zip with stored (uncompressed) entries, like a tiny .docx.
    fn zip(entries: &[(&str, &str)]) -> Vec<u8> {
        let (mut out, mut central) = (Vec::new(), Vec::new());
        for (name, data) in entries {
            let offset = out.len() as u32;
            out.extend_from_slice(&[0x50, 0x4b, 0x03, 0x04, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(data.as_bytes());
            central.extend_from_slice(&[0x50, 0x4b, 0x01, 0x02, 20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(name.len() as u16).to_le_bytes());
            central.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            central.extend_from_slice(&offset.to_le_bytes());
            central.extend_from_slice(name.as_bytes());
        }
        let at = out.len() as u32;
        let n = entries.len() as u16;
        out.extend_from_slice(&central);
        out.extend_from_slice(&[0x50, 0x4b, 0x05, 0x06, 0, 0, 0, 0]);
        out.extend_from_slice(&n.to_le_bytes());
        out.extend_from_slice(&n.to_le_bytes());
        out.extend_from_slice(&(central.len() as u32).to_le_bytes());
        out.extend_from_slice(&at.to_le_bytes());
        out.extend_from_slice(&[0, 0]);
        out
    }

    #[test]
    fn the_text_of_a_word_document_is_read_out() {
        let doc = zip(&[(
            "word/document.xml",
            r#"<w:document><w:body><w:p><w:r><w:t>Q3 plan</w:t></w:r></w:p><w:p><w:r><w:t>Ship &amp; measure</w:t><w:tab/><w:t>&#8364;5</w:t></w:r></w:p></w:body></w:document>"#,
        )]);
        let dir = temp();
        let a = save(&dir, "Plan.docx", &doc).unwrap();
        assert_eq!(a.kind, "text");
        let text = std::fs::read_to_string(a.text_path.as_ref().unwrap()).unwrap();
        assert_eq!(text, "Q3 plan\nShip & measure\t€5");
        let t = for_transcript(&[a], "gpt");
        assert!(t.contains("[Attached file: Plan.docx") && t.contains("Ship & measure"), "{t}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn slides_are_read_in_order() {
        let deck = zip(&[
            ("ppt/slides/slide10.xml", "<a:p><a:t>Ten</a:t></a:p>"),
            ("ppt/slides/slide2.xml", "<a:p><a:t>Two</a:t></a:p>"),
        ]);
        assert_eq!(document_text("pptx", &deck).unwrap(), "--- Slide 1 ---\nTwo\n\n--- Slide 2 ---\nTen");
    }

    #[test]
    fn files_are_sorted_into_what_each_ai_can_read() {
        let dir = temp();
        assert_eq!(save(&dir, "notes.md", b"# Notes").unwrap().kind, "text");
        assert_eq!(save(&dir, "photo.JPG", &[0xff, 0xd8, 0xff]).unwrap().kind, "image");
        assert_eq!(save(&dir, "paper.pdf", b"%PDF-1.7").unwrap().kind, "pdf");
        assert_eq!(save(&dir, "blob.bin", &[0, 1, 2]).unwrap().kind, "file");
        assert_eq!(save(&dir, "README", b"plain words").unwrap().kind, "text", "no extension, but text");
        let odd = save(&dir, "../../etc/pass wd?.txt", b"x").unwrap();
        assert!(Path::new(&odd.path).starts_with(&dir) && odd.name == "pass wd_.txt", "{odd:?}");
        assert!(save(&dir, "big.txt", &vec![b'a'; (MAX_BYTES + 1) as usize]).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn only_the_conversations_own_files_are_accepted() {
        let dir = temp();
        let mine = save(&dir, "a.txt", b"hi").unwrap();
        assert!(belongs(&dir, &mine));
        let elsewhere = Attachment { path: "/etc/hosts".into(), ..mine.clone() };
        assert!(!belongs(&dir, &elsewhere));
        let sneaky = Attachment { text_path: Some("/etc/hosts".into()), ..mine };
        assert!(!belongs(&dir, &sneaky));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn long_text_is_cut_and_pictures_are_pointed_to() {
        let dir = temp();
        let long = save(&dir, "big.log", "x".repeat(MAX_INLINE_CHARS + 10).as_bytes()).unwrap();
        assert!(for_transcript(&[long], "gpt").contains("cut here"));
        let pic = save(&dir, "shot.png", &[0x89, b'P', b'N', b'G']).unwrap();
        let t = for_transcript(std::slice::from_ref(&pic), "claude");
        assert!(t.contains("[Attached picture: shot.png") && t.contains(&pic.path));
        let g = for_transcript(std::slice::from_ref(&pic), "gemini");
        assert!(g.contains("You can't open pictures") && !g.contains(&pic.path), "{g}");
        let _ = std::fs::remove_dir_all(dir);
    }
}
