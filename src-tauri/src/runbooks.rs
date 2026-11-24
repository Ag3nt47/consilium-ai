use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

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
    list_in(&directory()?)
}

fn list_in(directory: &Path) -> Result<Vec<String>, String> {
    let entries = match fs::read_dir(directory) {
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
    read_in(&directory()?, &id)
}

fn read_in(directory: &Path, id: &str) -> Result<String, String> {
    if !valid_id(id) {
        return Err("Invalid runbook name.".into());
    }
    let path = directory.join(format!("{id}.md"));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_names_and_validates_reads() {
        let directory =
            std::env::temp_dir().join(format!("consilium-runbooks-{}", uuid::Uuid::new_v4()));
        assert!(list_in(&directory).unwrap().is_empty());
        fs::create_dir(&directory).unwrap();
        let result = (|| {
            fs::write(directory.join("check.md"), "  # Check release\n").unwrap();
            fs::write(
                directory.join("large.md"),
                vec![b'x'; MAX_BYTES as usize + 1],
            )
            .unwrap();
            fs::write(directory.join("ignored.txt"), "ignore").unwrap();
            fs::create_dir(directory.join("folder.md")).unwrap();
            (
                list_in(&directory),
                read_in(&directory, "check"),
                read_in(&directory, "../check"),
                read_in(&directory, "large"),
                read_in(&directory, "folder"),
            )
        })();
        for name in ["check.md", "large.md", "ignored.txt"] {
            fs::remove_file(directory.join(name)).unwrap();
        }
        fs::remove_dir(directory.join("folder.md")).unwrap();
        fs::remove_dir(&directory).unwrap();
        assert_eq!(result.0.unwrap(), ["check", "large"]);
        assert_eq!(result.1.unwrap(), "# Check release");
        assert!(result.2.is_err());
        assert!(result.3.is_err());
        assert!(result.4.is_err());
    }
}
