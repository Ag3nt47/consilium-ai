//! AIs switched off in one conversation with the Stop button in their council box. A switched-off
//! AI isn't started there by the box, an @mention, another AI's @mention or an auto-continue
//! round, until the human presses Start again. Saved in `switched-off.json` in the data folder.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;

const FILE: &str = "switched-off.json";
static WRITE: Mutex<()> = Mutex::new(());

fn load(dir: &Path) -> BTreeMap<String, Vec<String>> {
    std::fs::read(dir.join(FILE)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// The AIs switched off in `conversation`.
pub fn off(dir: impl AsRef<Path>, conversation: &str) -> Vec<String> {
    load(dir.as_ref()).remove(conversation).unwrap_or_default()
}

pub fn is_off(dir: impl AsRef<Path>, conversation: &str, agent: &str) -> bool {
    off(dir, conversation).iter().any(|a| a == agent)
}

/// Switches `agent` on or off in `conversation` only. Returns the AIs now off there.
pub fn set(dir: impl AsRef<Path>, conversation: &str, agent: &str, on: bool) -> Result<Vec<String>, String> {
    let dir = dir.as_ref();
    let _guard = WRITE.lock().unwrap_or_else(|e| e.into_inner());
    let mut all = load(dir);
    let list = all.entry(conversation.to_string()).or_default();
    list.retain(|a| a != agent);
    if !on {
        list.push(agent.to_string());
    }
    let now = list.clone();
    if now.is_empty() {
        all.remove(conversation);
    }
    let path = dir.join(FILE);
    let tmp = dir.join(format!("{FILE}.tmp"));
    std::fs::write(&tmp, serde_json::to_vec_pretty(&all).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    crate::store::replace_file(&tmp, &path).map_err(|e| e.to_string())?;
    Ok(now)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_in_one_chat_only_and_back_on() {
        let d = std::env::temp_dir().join(format!("switched-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        assert!(!is_off(&d, "a", "gemini"));
        assert_eq!(set(&d, "a", "gemini", false).unwrap(), vec!["gemini".to_string()]);
        assert!(is_off(&d, "a", "gemini"));
        assert!(!is_off(&d, "b", "gemini"), "other chats are unaffected");
        assert!(!is_off(&d, "a", "claude"));
        set(&d, "a", "gemini", false).unwrap();
        assert_eq!(off(&d, "a"), vec!["gemini".to_string()], "no duplicates");
        assert!(set(&d, "a", "gemini", true).unwrap().is_empty());
        assert!(!is_off(&d, "a", "gemini"));
        let _ = std::fs::remove_dir_all(&d);
    }
}