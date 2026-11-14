//! Keeps track of turns in progress so they can carry on after the app quits or crashes.
//!
//! `turns.json` in the app's data folder holds, for each conversation with a turn running, who was
//! asked, how many rounds, and how far it got. Messages themselves are already saved in the event
//! log, so on the next launch Consilium only needs this position to continue. An entry is removed
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
    /// The round and the position in `targets` of the next AI to ask.
    pub round: u32,
    pub index: usize,
    #[serde(default)]
    pub attempts: u32,
}

impl Pending {
    pub fn new(conversation: &str, targets: Vec<String>, rounds: u32) -> Self {
        Pending { conversation: conversation.into(), targets, rounds, round: 1, index: 0, attempts: 0 }
    }

    /// The position after the AI at (`round`, `index`) has answered.
    pub fn after(&self, round: u32, index: usize) -> (u32, usize) {
        if index + 1 < self.targets.len() {
            (round, index + 1)
        } else {
            (round + 1, 0)
        }
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
                let _ = std::fs::rename(&tmp, &self.path);
            }
        }
    }

    pub fn begin(&self, p: Pending) {
        let mut items = self.items.lock().unwrap();
        items.insert(p.conversation.clone(), p);
        self.save(&items);
    }

    /// Records that the next AI to ask is at (`round`, `index`).
    pub fn at(&self, conversation: &str, round: u32, index: usize) {
        let mut items = self.items.lock().unwrap();
        if let Some(p) = items.get_mut(conversation) {
            p.round = round;
            p.index = index;
            self.save(&items);
        }
    }

    pub fn finish(&self, conversation: &str) {
        let mut items = self.items.lock().unwrap();
        if items.remove(conversation).is_some() {
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
        j.at("c1", 2, 1);
        j.begin(Pending::new("c2", vec!["gpt".into()], 1));
        j.finish("c2");

        let after_restart = Journal::load(path.clone());
        let (resume, give_up) = after_restart.interrupted();
        assert_eq!(resume.len(), 1);
        assert!(give_up.is_empty());
        assert_eq!((resume[0].round, resume[0].index, resume[0].attempts), (2, 1, 1));
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

    #[test]
    fn next_position_wraps_to_the_next_round() {
        let p = Pending::new("c", vec!["gpt".into(), "claude".into()], 3);
        assert_eq!(p.after(1, 0), (1, 1));
        assert_eq!(p.after(1, 1), (2, 0));
    }
}
