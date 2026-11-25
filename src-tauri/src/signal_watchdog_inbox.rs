#[cfg(any(windows, test))]
use super::{load, receiver_config, Enrollment, ENROLLMENT_FILE};
use crate::bridge::BridgeConfig;
use crate::signal_chat::{Incoming, Origin};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};

const MAX_AGE_MS: u64 = 15 * 60_000;
const MAX_FUTURE_MS: u64 = 30_000;
const MAX_PENDING: usize = 64;
#[cfg(any(windows, test))]
const MAX_RECENT: usize = 2_048;
const MAX_MESSAGE_BYTES: usize = 48 * 1024;

/// Both subscribers can see the same ordinary message. The lock pairs each pending
/// message with a recent receipt so the app can retrieve it once across process restarts.
#[derive(Default, Deserialize, Serialize)]
struct Journal {
    identity: String,
    pending: VecDeque<Incoming>,
    dispatched: VecDeque<u64>,
}

impl Journal {
    fn new(identity: &str) -> Self {
        Self {
            identity: identity.into(),
            ..Self::default()
        }
    }

    fn prune(&mut self, now: u64) {
        self.pending.retain(|item| fresh(item.timestamp, now));
        self.dispatched.retain(|timestamp| fresh(*timestamp, now));
    }

    fn push(&mut self, msg: &Incoming, now: u64) -> Result<bool, String> {
        self.prune(now);
        if !fresh(msg.timestamp, now) {
            return Ok(false);
        }
        if self.dispatched.contains(&msg.timestamp)
            || self
                .pending
                .iter()
                .any(|item| item.timestamp == msg.timestamp)
        {
            return Ok(true);
        }
        if self.pending.len() >= MAX_PENDING {
            return Ok(false);
        }
        let size = serde_json::to_vec(msg)
            .map_err(|_| "Couldn't encode a queued Signal message.")?
            .len();
        if size > MAX_MESSAGE_BYTES {
            return Ok(false);
        }
        self.pending.push_back(msg.clone());
        Ok(true)
    }

    #[cfg(any(windows, test))]
    fn take(&mut self, cfg: &BridgeConfig, now: u64) -> Option<Incoming> {
        self.prune(now);
        // Never evict a recent receipt to make room for another delivery during its replay window.
        if self.dispatched.len() >= MAX_RECENT {
            return None;
        }
        while let Some(msg) = self.pending.pop_front() {
            if valid_group(cfg, &msg) && !self.dispatched.contains(&msg.timestamp) {
                self.dispatched.push_back(msg.timestamp);
                return Some(msg);
            }
        }
        None
    }
}

fn fresh(timestamp: u64, now: u64) -> bool {
    timestamp != 0
        && now.saturating_sub(timestamp) <= MAX_AGE_MS
        && timestamp.saturating_sub(now) <= MAX_FUTURE_MS
}

fn valid_group(cfg: &BridgeConfig, msg: &Incoming) -> bool {
    !msg.group.is_empty()
        && match &msg.origin {
            Origin::Council => msg.group == cfg.groups.main,
            Origin::Agent(agent) => cfg.groups.agents.get(agent) == Some(&msg.group),
        }
}

pub(crate) struct CouncilInbox {
    data: PathBuf,
    identity: String,
}

impl CouncilInbox {
    #[cfg(any(windows, test))]
    pub(crate) fn from_config(data: &Path, cfg: &BridgeConfig) -> Option<Self> {
        let enrollment = load::<Enrollment>(&data.join(ENROLLMENT_FILE))
            .ok()
            .flatten()?;
        let group = enrollment.group?;
        receiver_config(cfg, &enrollment.identity, &group).then_some(Self {
            data: data.to_owned(),
            identity: enrollment.identity,
        })
    }

    pub(super) fn for_watchdog(data: &Path, identity: &str) -> Self {
        Self {
            data: data.to_owned(),
            identity: identity.into(),
        }
    }

    pub(crate) fn push(&self, cfg: &BridgeConfig, msg: &Incoming) -> Result<bool, String> {
        if !valid_group(cfg, msg) {
            return Ok(false);
        }
        self.with_journal(|journal, now| journal.push(msg, now))
    }

    #[cfg(any(windows, test))]
    pub(crate) fn take(&self, cfg: &BridgeConfig) -> Result<Option<Incoming>, String> {
        let path = self.data.join("signal-watchdog-inbox.json");
        if !path.is_file() {
            return Ok(None);
        }
        // Record retrieval before handing a message to the engine. A repeat notification after
        // a watchdog restart cannot start the same AI turn or repeat an approval.
        self.with_journal(|journal, now| Ok(journal.take(cfg, now)))
    }

    fn with_journal<T>(
        &self,
        update: impl FnOnce(&mut Journal, u64) -> Result<T, String>,
    ) -> Result<T, String> {
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.data.join("signal-watchdog-inbox.lock"))
            .map_err(|e| format!("Couldn't open the Signal watchdog inbox lock: {e}"))?;
        // A locked transaction is only local file I/O. If another process cannot complete that
        // work promptly, keep the existing direct receive path available.
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(250);
        loop {
            match lock.try_lock() {
                Ok(()) => break,
                Err(std::fs::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(_) => return Err("The Signal watchdog inbox is busy.".into()),
            }
        }
        let path = self.data.join("signal-watchdog-inbox.json");
        let contents = std::fs::read(&path);
        let mut journal = match contents.as_ref() {
            Ok(bytes) => serde_json::from_slice::<Journal>(bytes)
                .map_err(|_| "The Signal watchdog inbox is invalid.")?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Journal::new(&self.identity),
            Err(e) => return Err(format!("Couldn't read the Signal watchdog inbox: {e}")),
        };
        if journal.identity != self.identity {
            journal = Journal::new(&self.identity);
        }
        let result = update(&mut journal, crate::store::now_ms())?;
        let bytes = serde_json::to_vec(&journal)
            .map_err(|_| "Couldn't encode the Signal watchdog inbox.")?;
        if !contents.is_ok_and(|previous| previous == bytes) {
            crate::link::write_private(&path, &bytes)?;
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::SignalGroups;

    fn config() -> BridgeConfig {
        BridgeConfig {
            groups: SignalGroups {
                main: "council".into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn message(timestamp: u64) -> Incoming {
        Incoming {
            origin: Origin::Council,
            text: "from phone".into(),
            timestamp,
            files: vec![],
            group: "council".into(),
        }
    }

    #[test]
    fn queued_messages_and_receipts_survive_a_process_restart() {
        let data =
            std::env::temp_dir().join(format!("consilium-signal-inbox-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&data).unwrap();
        let cfg = config();
        let msg = message(crate::store::now_ms());
        let first = CouncilInbox::for_watchdog(&data, "account");
        assert!(first.push(&cfg, &msg).unwrap());
        drop(first);
        let reopened = CouncilInbox::for_watchdog(&data, "account");
        assert!(reopened.push(&cfg, &msg).unwrap());
        assert_eq!(reopened.take(&cfg).unwrap(), Some(msg.clone()));
        drop(reopened);
        let later = CouncilInbox::for_watchdog(&data, "account");
        assert!(later.push(&cfg, &msg).unwrap());
        assert_eq!(later.take(&cfg).unwrap(), None);
        std::fs::remove_file(data.join("signal-watchdog-inbox.json")).unwrap();
        std::fs::remove_file(data.join("signal-watchdog-inbox.lock")).unwrap();
        std::fs::remove_dir(data).unwrap();
    }

    #[test]
    fn outdated_or_different_group_messages_are_never_drained() {
        let now = 2 * MAX_AGE_MS;
        let mut journal = Journal::new("account");
        assert!(!journal.push(&message(now - MAX_AGE_MS - 1), now).unwrap());
        assert!(journal.push(&message(now), now).unwrap());
        let mut other = config();
        other.groups.main = "new council".into();
        assert_eq!(journal.take(&other, now), None);
        assert!(journal.pending.is_empty());
    }
}
