//! Council settings: which AIs are members, and how much access each one has.
//!
//! Stored as `council.json` in the app's data folder. Access above "chat" only matters in a
//! conversation that has a project folder; without one every AI runs in an empty folder.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Access {
    /// Sees only the conversation; runs in an empty folder with tools off.
    #[default]
    Chat,
    /// Can read the conversation's project folder.
    Read,
    /// Can read and edit files in the project folder.
    Edit,
    /// Runs in the project folder with the permissions the person already approved in that
    /// AI's own tool (Codex config, Claude Code settings, Gemini approvals). Consilium adds none.
    Own,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Council {
    /// Agent ids in the council, in answering order.
    pub members: Vec<String>,
    #[serde(default)]
    pub access: HashMap<String, Access>,
}

impl Default for Council {
    fn default() -> Self {
        Council { members: vec!["gpt".into(), "claude".into()], access: HashMap::new() }
    }
}

pub struct Config {
    path: PathBuf,
    council: Mutex<Council>,
}

impl Config {
    pub fn load(path: PathBuf) -> Self {
        let council = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<Council>(&s).ok())
            .map(|mut c| {
                c.members.retain(|m| crate::agents::by_id(m).is_some());
                c.members.dedup();
                c
            })
            .unwrap_or_default();
        Config { path, council: Mutex::new(council) }
    }

    pub fn get(&self) -> Council {
        self.council.lock().unwrap().clone()
    }

    pub fn access(&self, agent: &str) -> Access {
        self.council.lock().unwrap().access.get(agent).copied().unwrap_or_default()
    }

    pub fn is_member(&self, agent: &str) -> bool {
        self.council.lock().unwrap().members.iter().any(|m| m == agent)
    }

    fn save(&self, c: &Council) -> Result<(), String> {
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(c).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &self.path).map_err(|e| e.to_string())
    }

    pub fn set_member(&self, agent: &str, member: bool) -> Result<Council, String> {
        if crate::agents::by_id(agent).is_none() {
            return Err("unknown AI".into());
        }
        let mut c = self.council.lock().unwrap();
        let mut next = c.clone();
        if member {
            if !next.members.iter().any(|m| m == agent) {
                next.members.push(agent.into());
            }
        } else {
            next.members.retain(|m| m != agent);
            if next.members.is_empty() {
                return Err("the council needs at least one AI".into());
            }
        }
        self.save(&next)?;
        *c = next.clone();
        Ok(next)
    }

    pub fn set_access(&self, agent: &str, access: Access) -> Result<Council, String> {
        if crate::agents::by_id(agent).is_none() {
            return Err("unknown AI".into());
        }
        let mut c = self.council.lock().unwrap();
        let mut next = c.clone();
        next.access.insert(agent.into(), access);
        self.save(&next)?;
        *c = next.clone();
        Ok(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> PathBuf {
        std::env::temp_dir().join(format!("consilium-council-{}.json", uuid::Uuid::new_v4()))
    }

    #[test]
    fn defaults_then_persists_members_and_access() {
        let p = temp();
        let c = Config::load(p.clone());
        assert_eq!(c.get().members, vec!["gpt", "claude"]);
        assert_eq!(c.access("gpt"), Access::Chat);
        c.set_member("gemini", true).unwrap();
        c.set_access("gpt", Access::Own).unwrap();
        let again = Config::load(p.clone());
        assert_eq!(again.get().members, vec!["gpt", "claude", "gemini"]);
        assert_eq!(again.access("gpt"), Access::Own);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn unknown_agents_and_an_empty_council_are_refused() {
        let p = temp();
        let c = Config::load(p.clone());
        assert!(c.set_member("evil", true).is_err());
        c.set_member("gpt", false).unwrap();
        assert!(c.set_member("claude", false).is_err());
        let _ = std::fs::remove_file(p);
    }
}
