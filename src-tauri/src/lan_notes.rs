//! A separate inbox for notes from authenticated PeerLink computers.

use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub(crate) const KIND: &str = "lan_note";
const NOTE_LIMIT: usize = 1_000;
const INBOX_LIMIT: usize = 100;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Note {
    id: String,
    source: String,
    text: String,
    sent_at: u64,
}

pub(crate) struct Inbox {
    path: PathBuf,
    lock: Mutex<()>,
}

impl Inbox {
    pub(crate) fn open(link_dir: &Path) -> Self {
        Self {
            path: link_dir.join("lan-notes.json"),
            lock: Mutex::new(()),
        }
    }

    fn read(&self) -> Result<Vec<Note>, String> {
        match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| error.to_string()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => Err(error.to_string()),
        }
    }

    pub(crate) fn list(&self) -> Result<Vec<Note>, String> {
        let _guard = self.lock.lock().unwrap_or_else(|error| error.into_inner());
        self.read()
    }

    pub(crate) fn record(
        &self,
        packet: &crate::peer_protocol::PeerPacket,
        source: &str,
        text: &str,
    ) -> Result<(), String> {
        // Invalid remote records are discarded. A filesystem error leaves the packet
        // available for the existing PeerLink startup replay.
        let Ok(text) = note_text(text) else {
            return Ok(());
        };
        let _guard = self.lock.lock().unwrap_or_else(|error| error.into_inner());
        let mut notes = self.read()?;
        if notes.iter().any(|note| note.id == packet.id) {
            return Ok(());
        }
        notes.push(Note {
            id: packet.id.clone(),
            source: source.to_owned(),
            text,
            sent_at: packet.timestamp_ms,
        });
        notes.sort_by_key(|note| std::cmp::Reverse(note.sent_at));
        notes.truncate(INBOX_LIMIT);
        crate::link::write_private(
            &self.path,
            &serde_json::to_vec(&notes).map_err(|error| error.to_string())?,
        )
    }
}

pub(crate) fn note_text(text: &str) -> Result<String, String> {
    let text = text.trim();
    if text.is_empty() || text.chars().take(NOTE_LIMIT + 1).count() > NOTE_LIMIT {
        return Err("Enter a LAN note between 1 and 1,000 characters.".into());
    }
    if text
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        return Err("The LAN note contains unsupported control characters.".into());
    }
    Ok(text.to_owned())
}
