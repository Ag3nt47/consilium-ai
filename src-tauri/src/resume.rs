//! Keeps track of turns in progress so they can carry on after the app quits or crashes.
//!
//! `turns.json` in the app's data folder holds, for each conversation with a turn running, who was
//! asked and how many rounds. How far each AI got is read back from the conversation itself (see
//! `council::progress`), so on the next launch the turn continues without repeating answers. An entry is removed
//! when the turn ends normally or is stopped; anything left over at launch was interrupted.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

/// A turn is resumed at most this many times, so one that keeps failing can't loop forever.
pub const MAX_ATTEMPTS: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Pending {
    pub conversation: String,
    pub targets: Vec<String>,
    pub rounds: u32,
    #[serde(default)]
    pub attempts: u32,
    /// Identifies this run of the turn, so a turn that was interrupted and replaced by a newer one
    /// can't remove the newer one's entry when it finally ends.
    #[serde(default)]
    pub turn: String,
}

impl Pending {
    pub fn new(conversation: &str, targets: Vec<String>, rounds: u32) -> Self {
        Pending { conversation: conversation.into(), targets, rounds, attempts: 0, turn: uuid::Uuid::new_v4().to_string() }
    }
}

pub struct Journal {
    path: PathBuf,
    items: Mutex<HashMap<String, Pending>>,
}

impl Journal {
    pub fn load(path: PathBuf) -> Self {
        let items = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<HashMap<String, Pending>>(&s).ok())
            .unwrap_or_default();
        Journal { path, items: Mutex::new(items) }
    }

    fn save(&self, items: &HashMap<String, Pending>) {
        let tmp = self.path.with_extension("json.tmp");
        if let Ok(bytes) = serde_json::to_vec_pretty(items) {
            if std::fs::write(&tmp, bytes).is_ok() {
                let _ = crate::store::replace_file(&tmp, &self.path);
            }
        }
    }

    pub fn begin(&self, p: Pending) {
        let mut items = self.items.lock().unwrap();
        items.insert(p.conversation.clone(), p);
        self.save(&items);
    }

    /// Conversations with a turn recorded as in progress.
    pub fn pending_conversations(&self) -> Vec<String> {
        self.items.lock().unwrap().keys().cloned().collect()
    }

    /// An AI answered in this turn, so it isn't stuck: restarts from here count from zero again.
    /// (The attempt limit is for turns that crash the app, not for ones resumed after updates.)
    pub fn progressed(&self, conversation: &str, turn: &str) {
        let mut items = self.items.lock().unwrap();
        if let Some(p) = items.get_mut(conversation).filter(|p| p.turn == turn && p.attempts > 0) {
            p.attempts = 0;
            self.save(&items);
        }
    }

    /// Forgets the turn `turn` in `conversation`, if it is still the one recorded there.
    pub fn finish(&self, conversation: &str, turn: &str) {
        let mut items = self.items.lock().unwrap();
        if items.get(conversation).is_some_and(|p| p.turn == turn) {
            items.remove(conversation);
            self.save(&items);
        }
    }

    /// Turns that were interrupted by a quit or crash, each with its attempt count raised.
    /// Those that already failed MAX_ATTEMPTS times are returned separately and dropped.
    pub fn interrupted(&self) -> (Vec<Pending>, Vec<Pending>) {
        let mut items = self.items.lock().unwrap();
        let (mut resume, mut give_up) = (Vec::new(), Vec::new());
        for p in items.values_mut() {
            if p.attempts >= MAX_ATTEMPTS {
                give_up.push(p.clone());
            } else {
                p.attempts += 1;
                resume.push(p.clone());
            }
        }
        for p in &give_up {
            items.remove(&p.conversation);
        }
        self.save(&items);
        (resume, give_up)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> PathBuf {
        std::env::temp_dir().join(format!("consilium-turns-{}.json", uuid::Uuid::new_v4()))
    }

    #[test]
    fn position_survives_a_restart_and_finished_turns_are_forgotten() {
        let path = temp();
        let j = Journal::load(path.clone());
        j.begin(Pending::new("c1", vec!["gpt".into(), "claude".into()], 3));
        let c2 = Pending::new("c2", vec!["gpt".into()], 1);
        j.begin(c2.clone());
        j.finish("c2", &c2.turn);

        let after_restart = Journal::load(path.clone());
        let (resume, give_up) = after_restart.interrupted();
        assert_eq!(resume.len(), 1);
        assert!(give_up.is_empty());
        assert_eq!((resume[0].conversation.as_str(), resume[0].rounds, resume[0].attempts), ("c1", 3, 1));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn an_old_turn_ending_late_leaves_the_newer_turn_recorded() {
        let path = temp();
        let j = Journal::load(path.clone());
        let old = Pending::new("c", vec!["gpt".into()], 1);
        j.begin(old.clone());
        let new = Pending::new("c", vec!["claude".into()], 1);
        j.begin(new.clone());
        j.finish("c", &old.turn);
        assert_eq!(Journal::load(path.clone()).interrupted().0[0].turn, new.turn);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn entries_from_before_this_version_still_resume() {
        let path = temp();
        std::fs::write(&path, r#"{"c":{"conversation":"c","targets":["gpt"],"rounds":3,"round":2,"index":0,"attempts":0}}"#).unwrap();
        let (resume, _) = Journal::load(path.clone()).interrupted();
        assert_eq!(resume.len(), 1);
        assert_eq!(resume[0].targets, vec!["gpt".to_string()]);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_turn_that_keeps_failing_is_given_up_after_two_tries() {
        let path = temp();
        Journal::load(path.clone()).begin(Pending::new("c", vec!["gpt".into()], 1));
        assert_eq!(Journal::load(path.clone()).interrupted().0.len(), 1);
        assert_eq!(Journal::load(path.clone()).interrupted().0.len(), 1);
        let (resume, give_up) = Journal::load(path.clone()).interrupted();
        assert!(resume.is_empty());
        assert_eq!(give_up.len(), 1);
        assert!(Journal::load(path.clone()).interrupted().1.is_empty(), "dropped after giving up");
        let _ = std::fs::remove_file(path);
    }

}
