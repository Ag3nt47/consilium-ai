use std::fs::{self, File};
use std::io::{self, Read};
use std::path::PathBuf;

const MAX_BYTES: u64 = 16 * 1024;

fn directory() -> Result<PathBuf, String> {
    dirs::home_dir()
        .map(|home| home.join(".consilium").join("skills"))
        .ok_or_else(|| "Cannot find the home folder.".into())
}

fn valid_id(id: &str) -> bool {
    let lower_alphanumeric = |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
    id.len() <= 64
        && id.bytes().next().is_some_and(lower_alphanumeric)
        && id
            .bytes()
            .all(|byte| lower_alphanumeric(byte) || byte == b'-')
}

#[tauri::command]
pub async fn list_runbooks() -> Result<Vec<String>, String> {
    let entries = match fs::read_dir(directory()?) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.to_string()),
    };
    let mut ids = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        if !entry
            .file_type()
            .map_err(|error| error.to_string())?
            .is_file()
        {
            continue;
        }
        if let Some(id) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.strip_suffix(".md"))
        {
            if valid_id(id) {
                ids.push(id.to_owned());
            }
        }
    }
    ids.sort_unstable();
    Ok(ids)
}

#[tauri::command]
pub async fn read_runbook(id: String) -> Result<String, String> {
    if !valid_id(&id) {
        return Err("Invalid runbook name.".into());
    }
    let path = directory()?.join(format!("{id}.md"));
    let metadata = fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() > MAX_BYTES {
        return Err("A runbook must be a regular Markdown file up to 16 KiB.".into());
    }
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|file| file.take(MAX_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("Runbook exceeds 16 KiB.".into());
    }
    Ok(String::from_utf8_lossy(&bytes).trim().to_owned())
}
