//! Attended OS bonding. Codes live only in the current in-memory attempt and its
//! private inherited pipes; PeerLink trust and transport tests remain separate.
#[cfg(any(windows, test))]
use serde::Deserialize;
use serde::Serialize;
#[cfg(any(windows, test))]
use std::io::{self, BufRead, Write};
use std::sync::Arc;
#[cfg(any(windows, test))]
use std::time::{Duration, Instant};

#[cfg_attr(not(windows), allow(dead_code))]
pub struct PairingTarget {
    pub node_id: String,
    pub peer_name: String,
    pub address: String,
    pub valid: Arc<dyn Fn() -> bool + Send + Sync>,
}

// Deliberately no Debug: neither entered nor OS-generated digits belong in logs.
#[derive(Clone, Serialize)]
pub struct PairingStatus {
    pub attempt_id: String,
    pub node_id: String,
    pub peer_name: String,
    pub challenge_id: Option<String>,
    pub state: String,
    pub code: Option<String>,
    pub remaining_ms: u64,
    pub authenticated: bool,
    pub remembered: bool,
    pub detail: String,
}

#[cfg(any(windows, test))]
const ATTEMPT_LIMIT: Duration = Duration::from_secs(90);
#[cfg(any(windows, test))]
const FRAME_LIMIT: usize = 2048;

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[cfg(any(windows, test))]
pub(super) enum WorkerCommand {
    Start { address: String },
    Confirm { challenge_id: String, code: String },
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[cfg(any(windows, test))]
pub(super) enum WorkerEvent {
    Ready {},
    Challenge {
        challenge_id: String,
        code: String,
        remaining_ms: u64,
    },
    Confirming {},
    Paired {
        authenticated: bool,
        remembered: bool,
    },
    Failed {
        code: u32,
    },
    Invalidated {},
}

#[cfg(any(windows, test))]
pub(super) fn valid_code(code: &str) -> bool {
    code.len() == 6 && code.bytes().all(|byte| byte.is_ascii_digit())
}

#[cfg(any(windows, test))]
pub(super) fn read_frame<T: serde::de::DeserializeOwned>(
    reader: &mut impl BufRead,
) -> io::Result<Option<T>> {
    let mut frame = Vec::with_capacity(256);
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            return if frame.is_empty() {
                Ok(None)
            } else {
                Err(io::Error::from(io::ErrorKind::UnexpectedEof))
            };
        }
        let count = buffer
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(buffer.len(), |index| index + 1);
        if frame.len() + count > FRAME_LIMIT {
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }
        let complete = buffer[count - 1] == b'\n';
        frame.extend_from_slice(&buffer[..count]);
        reader.consume(count);
        if complete {
            return serde_json::from_slice(&frame)
                .map(Some)
                .map_err(|_| io::Error::from(io::ErrorKind::InvalidData));
        }
    }
}

#[cfg(any(windows, test))]
pub(super) fn write_frame(writer: &mut impl Write, frame: &impl Serialize) -> io::Result<()> {
    let mut bytes =
        serde_json::to_vec(frame).map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
    if bytes.len() + 1 > FRAME_LIMIT {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }
    bytes.push(b'\n');
    writer.write_all(&bytes)?;
    writer.flush()
}

#[cfg(any(windows, test))]
struct Snapshot {
    status: PairingStatus,
    deadline: Instant,
    challenge_deadline: Option<Instant>,
}

#[cfg(any(windows, test))]
impl Snapshot {
    fn new(target: &PairingTarget, now: Instant) -> Self {
        Self {
            status: PairingStatus {
                attempt_id: uuid::Uuid::new_v4().to_string(),
                node_id: target.node_id.clone(),
                peer_name: target.peer_name.clone(),
                challenge_id: None,
                state: "starting".into(),
                code: None,
                remaining_ms: ATTEMPT_LIMIT.as_millis() as u64,
                authenticated: false,
                remembered: false,
                detail:
                    "Starting Windows Bluetooth pairing. Keep the Mac's Bluetooth settings open."
                        .into(),
            },
            deadline: now + ATTEMPT_LIMIT,
            challenge_deadline: None,
        }
    }

    fn terminal(&self) -> bool {
        matches!(
            self.status.state.as_str(),
            "paired" | "cancelled" | "expired" | "failed"
        )
    }

    fn end(&mut self, state: &str, detail: &str) {
        self.status.state = state.into();
        self.status.detail = detail.into();
        self.status.code = None;
        self.status.challenge_id = None;
        self.status.remaining_ms = 0;
        self.challenge_deadline = None;
    }

    fn current_deadline(&self) -> Instant {
        self.challenge_deadline
            .unwrap_or(self.deadline)
            .min(self.deadline)
    }

    fn view(&self, now: Instant) -> PairingStatus {
        let mut status = self.status.clone();
        if !self.terminal() && now >= self.current_deadline() {
            status.state = "expired".into();
            status.code = None;
            status.challenge_id = None;
            status.remaining_ms = 0;
            status.detail = "Bluetooth pairing expired. Start a fresh comparison.".into();
            return status;
        }
        status.remaining_ms = if self.terminal() {
            0
        } else {
            self.current_deadline()
                .saturating_duration_since(now)
                .as_millis() as u64
        };
        status
    }

    fn apply(&mut self, event: WorkerEvent, now: Instant) -> Result<(), ()> {
        if self.terminal() || now >= self.current_deadline() {
            return Err(());
        }
        match event {
            WorkerEvent::Ready {} if self.status.state == "starting" => {}
            WorkerEvent::Challenge { challenge_id, code, remaining_ms } => {
                if self.status.state != "starting" || !valid_code(&code)
                    || uuid::Uuid::parse_str(&challenge_id).is_err() || remaining_ms == 0 || remaining_ms > 30_000 {
                    return Err(());
                }
                self.status.state = "awaiting_code".into();
                self.status.challenge_id = Some(challenge_id);
                self.status.code = Some(code);
                self.challenge_deadline = Some(now + Duration::from_millis(remaining_ms));
                self.status.detail = "Compare the Bluetooth code on both computers, enter the Mac's six digits here, and confirm on the Mac.".into();
            }
            WorkerEvent::Confirming {} if self.status.state == "confirming" => {}
            WorkerEvent::Paired { authenticated: true, remembered: true } => {
                if !matches!(self.status.state.as_str(), "starting" | "confirming") { return Err(()); }
                self.status.authenticated = true;
                self.status.remembered = true;
                self.end("paired", "Windows confirms the computers are paired. Test the selected Bluetooth connection next.");
            }
            WorkerEvent::Failed { code } => self.end("failed", &format!("Windows Bluetooth pairing did not complete ({code}). Cancel any old Mac prompt before trying again.")),
            WorkerEvent::Invalidated {} => self.end("cancelled", "The Bluetooth challenge changed. Cancel the old Mac prompt and start a fresh comparison."),
            _ => return Err(()),
        }
        Ok(())
    }

    fn confirm(
        &mut self,
        challenge_id: &str,
        code: &str,
        now: Instant,
    ) -> Result<WorkerCommand, String> {
        if now >= self.current_deadline() {
            return Err("The Bluetooth comparison expired. Start a fresh attempt.".into());
        }
        if self.status.state != "awaiting_code"
            || self.status.challenge_id.as_deref() != Some(challenge_id)
        {
            return Err("This Bluetooth comparison is no longer current.".into());
        }
        if !valid_code(code) || self.status.code.as_deref() != Some(code) {
            return Err("Enter the six digits shown on the Mac's current Bluetooth prompt.".into());
        }
        self.status.state = "confirming".into();
        self.status.code = None;
        // Keep the opaque challenge identity until completion, never its digits.
        self.challenge_deadline = None;
        self.status.detail = "Waiting for Windows and the Mac to finish Bluetooth pairing.".into();
        Ok(WorkerCommand::Confirm {
            challenge_id: challenge_id.into(),
            code: code.into(),
        })
    }
}

#[cfg(windows)]
#[path = "pairing_process.rs"]
mod process;

pub fn start(target: PairingTarget) -> Result<PairingStatus, String> {
    #[cfg(windows)]
    {
        process::start(target)
    }
    #[cfg(not(windows))]
    {
        let _ = target;
        Err("Start this attended Bluetooth pairing on the Windows computer.".into())
    }
}

pub fn status(attempt_id: &str) -> Result<PairingStatus, String> {
    #[cfg(windows)]
    {
        process::status(attempt_id)
    }
    #[cfg(not(windows))]
    {
        let _ = attempt_id;
        Err("Windows Bluetooth pairing is unavailable on this computer.".into())
    }
}

pub fn confirm(attempt_id: &str, challenge_id: &str, code: &str) -> Result<PairingStatus, String> {
    #[cfg(windows)]
    {
        process::confirm(attempt_id, challenge_id, code)
    }
    #[cfg(not(windows))]
    {
        let _ = (attempt_id, challenge_id, code);
        Err("Windows Bluetooth pairing is unavailable on this computer.".into())
    }
}

pub fn cancel(attempt_id: &str) -> Result<PairingStatus, String> {
    #[cfg(windows)]
    {
        process::cancel(attempt_id)
    }
    #[cfg(not(windows))]
    {
        let _ = attempt_id;
        Err("Windows Bluetooth pairing is unavailable on this computer.".into())
    }
}

pub fn worker_main() -> i32 {
    #[cfg(windows)]
    {
        super::pairing_worker::run()
    }
    #[cfg(not(windows))]
    {
        2
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot(now: Instant) -> Snapshot {
        Snapshot::new(
            &PairingTarget {
                node_id: "saved-peer".into(),
                peer_name: "Paired computer".into(),
                address: "A1B2C3D4E5F6".into(),
                valid: Arc::new(|| true),
            },
            now,
        )
    }
    fn challenge(snapshot: &mut Snapshot, now: Instant) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        assert!(snapshot
            .apply(
                WorkerEvent::Challenge {
                    challenge_id: id.clone(),
                    code: "012345".into(),
                    remaining_ms: 30_000
                },
                now
            )
            .is_ok());
        id
    }
    #[test]
    fn exact_current_challenge_and_digits_are_required_without_echoing_input() {
        let now = Instant::now();
        let mut state = snapshot(now);
        let id = challenge(&mut state, now);
        assert!(state.confirm("older", "012345", now).is_err());
        let error = state.confirm(&id, "654321", now).err().unwrap();
        assert!(!error.contains("654321"));
        assert!(state.confirm(&id, "012345", now).is_ok());
        assert!(state.status.code.is_none());
        assert!(state.confirm(&id, "012345", now).is_err());
    }
    #[test]
    fn expired_or_changed_challenge_cannot_approve_or_claim_bond() {
        let now = Instant::now();
        let mut state = snapshot(now);
        let id = challenge(&mut state, now);
        assert!(state
            .confirm(&id, "012345", now + Duration::from_secs(30))
            .is_err());
        assert!(state
            .apply(
                WorkerEvent::Paired {
                    authenticated: true,
                    remembered: true
                },
                now
            )
            .is_err());
        assert!(state.apply(WorkerEvent::Invalidated {}, now).is_ok());
        assert!(state.status.code.is_none());
        assert!(state.confirm(&id, "012345", now).is_err());
    }
    #[test]
    fn successful_os_flags_are_required_and_terminal_events_cannot_reopen_digits() {
        let now = Instant::now();
        let mut state = snapshot(now);
        let id = challenge(&mut state, now);
        state.confirm(&id, "012345", now).unwrap();
        assert!(state
            .apply(
                WorkerEvent::Paired {
                    authenticated: true,
                    remembered: false
                },
                now
            )
            .is_err());
        assert!(state
            .apply(
                WorkerEvent::Paired {
                    authenticated: true,
                    remembered: true
                },
                now
            )
            .is_ok());
        assert!(state
            .apply(
                WorkerEvent::Challenge {
                    challenge_id: uuid::Uuid::new_v4().to_string(),
                    code: "012345".into(),
                    remaining_ms: 30_000
                },
                now
            )
            .is_err());
        assert_eq!(state.status.state, "paired");
        assert!(state.status.code.is_none());
    }
    #[test]
    fn ipc_rejects_oversized_partial_and_unknown_frames_without_unbounded_allocation() {
        assert!(
            read_frame::<WorkerEvent>(&mut io::Cursor::new(vec![b'x'; FRAME_LIMIT + 1])).is_err()
        );
        assert!(read_frame::<WorkerEvent>(&mut io::Cursor::new(b"{\"kind\":\"ready\"}")).is_err());
        assert!(read_frame::<WorkerEvent>(&mut io::Cursor::new(
            b"{\"kind\":\"ready\",\"extra\":1}\n"
        ))
        .is_err());
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &WorkerEvent::Ready {}).unwrap();
        assert!(matches!(
            read_frame::<WorkerEvent>(&mut io::Cursor::new(bytes)).unwrap(),
            Some(WorkerEvent::Ready {})
        ));
    }

    #[test]
    fn status_crossing_deadline_never_exposes_expired_digits() {
        let now = Instant::now();
        let mut state = snapshot(now);
        challenge(&mut state, now);
        let view = state.view(now + Duration::from_secs(30));
        assert_eq!(view.state, "expired");
        assert!(view.code.is_none());
        assert!(view.challenge_id.is_none());
        assert_eq!(view.remaining_ms, 0);
    }
}
