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
    /// Runs in the project folder with every approval and sandbox check turned off, using each
    /// tool's own bypass switch: it can run any command, install software and change any file.
    Unrestricted,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Council {
    /// Agent ids in the council, in answering order.
    pub members: Vec<String>,
    #[serde(default)]
    pub access: HashMap<String, Access>,
    /// Project folder for conversations that don't set their own.
    #[serde(default)]
    pub default_workspace: Option<String>,
    /// Access for every AI without its own setting, including AIs added later.
    #[serde(default)]
    pub default_access: Access,
    /// Show codex-reset.com's unofficial forecast of the next global Codex usage reset (to the
    /// human and the AIs). Off unless the person turns it on, since it asks a third-party site.
    #[serde(default)]
    pub reset_forecast: bool,
}

impl Default for Council {
    fn default() -> Self {
        Council {
            members: vec!["gpt".into(), "claude".into()],
            access: HashMap::new(),
            default_workspace: None,
            default_access: Access::Chat,
            reset_forecast: false,
        }
    }
}

pub struct Config {
    path: PathBuf,
    council: Mutex<Council>,
}

impl Config {
    pub fn load(path: PathBuf) -> Self {
        let council = Self::read(&path).unwrap_or_default();
        Config { path, council: Mutex::new(council) }
    }

    fn read(path: &std::path::Path) -> Option<Council> {
        std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str::<Council>(&s).ok()).map(|mut c| {
            c.members.retain(|m| crate::agents::by_id(m).is_some());
            c.members.dedup();
            c
        })
    }

    /// The settings as saved, re-read each time, so a change made in the app shows up in the
    /// terminal command and the other way round. (Kept in memory too, for when the file can't be read.)
    fn current(&self) -> std::sync::MutexGuard<'_, Council> {
        let mut c = self.council.lock().unwrap();
        if let Some(saved) = Self::read(&self.path) {
            *c = saved;
        }
        c
    }

    pub fn get(&self) -> Council {
        self.current().clone()
    }

    pub fn access(&self, agent: &str) -> Access {
        let c = self.current();
        c.access.get(agent).copied().unwrap_or(c.default_access)
    }

    /// Sets the council-wide default and clears per-AI settings, so every AI uses it.
    pub fn set_default_access(&self, access: Access) -> Result<Council, String> {
        let mut c = self.current();
        let mut next = c.clone();
        next.default_access = access;
        next.access.clear();
        self.save(&next)?;
        *c = next.clone();
        Ok(next)
    }

    pub fn is_member(&self, agent: &str) -> bool {
        self.current().members.iter().any(|m| m == agent)
    }

    fn save(&self, c: &Council) -> Result<(), String> {
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(c).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        crate::store::replace_file(&tmp, &self.path).map_err(|e| e.to_string())
    }

    pub fn set_member(&self, agent: &str, member: bool) -> Result<Council, String> {
        if crate::agents::by_id(agent).is_none() {
            return Err("unknown AI".into());
        }
        let mut c = self.current();
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

    pub fn set_default_workspace(&self, folder: Option<&str>) -> Result<Council, String> {
        let folder = match folder {
            None => None,
            Some(f) => {
                let p = std::fs::canonicalize(f).map_err(|_| format!("{f} doesn't exist"))?;
                if !p.is_dir() {
                    return Err(format!("{f} isn't a folder"));
                }
                Some(p.display().to_string())
            }
        };
        let mut c = self.current();
        let mut next = c.clone();
        next.default_workspace = folder;
        self.save(&next)?;
        *c = next.clone();
        Ok(next)
    }

    pub fn set_reset_forecast(&self, on: bool) -> Result<Council, String> {
        let mut c = self.current();
        let mut next = c.clone();
        next.reset_forecast = on;
        self.save(&next)?;
        *c = next.clone();
        Ok(next)
    }

    pub fn set_access(&self, agent: &str, access: Access) -> Result<Council, String> {
        if crate::agents::by_id(agent).is_none() {
            return Err("unknown AI".into());
        }
        let mut c = self.current();
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
    fn default_access_applies_to_every_ai_including_new_ones() {
        let p = temp();
        let c = Config::load(p.clone());
        c.set_access("gpt", Access::Read).unwrap();
        c.set_default_access(Access::Unrestricted).unwrap();
        assert_eq!(c.access("gpt"), Access::Unrestricted, "per-AI settings are cleared");
        c.set_member("gemini", true).unwrap();
        assert_eq!(c.access("gemini"), Access::Unrestricted);
        c.set_access("claude", Access::Chat).unwrap();
        assert_eq!(Config::load(p.clone()).access("claude"), Access::Chat, "a per-AI setting still overrides");
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
