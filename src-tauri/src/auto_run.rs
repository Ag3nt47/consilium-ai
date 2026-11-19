//! Chats where the human turned on "Auto-run this chat" after its warning: every command box an
//! AI of this council posts there runs without a click, risky, admin and install ones included (see
//! `auto_run_commands` in lib.rs). Saved per conversation in `auto-run.json` in the data folder, so it
//! never carries over to another chat and every new chat starts with it off.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Mutex;

const FILE: &str = "auto-run.json";
static WRITE: Mutex<()> = Mutex::new(());

fn load(dir: &Path) -> BTreeSet<String> {
    std::fs::read(dir.join(FILE)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// Whether auto-run is on in `conversation`.
pub fn is_on(dir: impl AsRef<Path>, conversation: &str) -> bool {
    load(dir.as_ref()).contains(conversation)
}

/// Turns auto-run on or off in `conversation` only. Returns whether it is now on.
pub fn set(dir: impl AsRef<Path>, conversation: &str, on: bool) -> Result<bool, String> {
    let dir = dir.as_ref();
    let _guard = WRITE.lock().unwrap_or_else(|e| e.into_inner());
    let mut all = load(dir);
    if on {
        all.insert(conversation.to_string());
    } else {
        all.remove(conversation);
    }
    let data = serde_json::to_vec_pretty(&all).map_err(|e| e.to_string())?;
    crate::store::write_atomic(&dir.join(FILE), &data).map_err(|e| e.to_string())?;
    Ok(on)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn on_in_one_chat_only_off_in_new_chats_and_back_off() {
        let d = std::env::temp_dir().join(format!("auto-run-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        assert!(!is_on(&d, "a"), "off by default");
        assert!(set(&d, "a", true).unwrap());
        assert!(is_on(&d, "a"));
        assert!(!is_on(&d, "b"), "a new chat starts with it off");
        assert!(!set(&d, "a", false).unwrap());
        assert!(!is_on(&d, "a"), "turning it off sticks");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn sweeps_only_conversations_where_auto_run_is_on() {
        let d = std::env::temp_dir().join(format!("auto-run-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        set(&d, "chat-auto", true).unwrap();
        let convs = vec!["chat-manual".to_string(), "chat-auto".to_string()];
        let auto_convs: Vec<_> = convs.into_iter().filter(|c| is_on(&d, c)).collect();
        assert_eq!(auto_convs, vec!["chat-auto".to_string()]);
        let _ = std::fs::remove_dir_all(&d);
    }
}
