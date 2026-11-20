//! One long-lived Antigravity (`agy`) process per chat and AI (#3).
//!
//! Started with `--input-format stream-json`, `agy` reads one NDJSON user message per turn and
//! ends each turn with an `{"event":"result",...}` line; closing stdin makes it exit with code 0
//! (checked on Windows, 2026-11-18). The process remembers the conversation, so a reused one is
//! sent only what is new since its last turn. A changed command line (access, folder, model) or
//! working folder starts a fresh process; Stop, a timeout or an unexpected exit drops it.
#![allow(dead_code)] // wired into agents::execute in the next commit

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// What makes a running process reusable for the next turn: same program, folder and arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spec {
    pub exe: PathBuf,
    pub dir: PathBuf,
    pub args: Vec<OsString>,
}

/// Starts the process for a spec, with stdin, stdout and stderr piped.
pub type Starter = dyn Fn(&Spec) -> std::io::Result<Child> + Send + Sync;

/// One turn for the pool.
pub struct Turn<'a> {
    pub conversation: &'a str,
    pub agent: &'a str,
    pub spec: Spec,
    /// The newest conversation event included in this turn.
    pub seq: u64,
    /// The whole prompt as one NDJSON line, for a new process.
    pub full_input: &'a str,
    /// The NDJSON line with what is new after the given seq, for a reused process.
    pub delta_input: &'a (dyn Fn(u64) -> String + Send + Sync),
}

struct Session {
    spec: Spec,
    child: Child,
    stdin: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
    stderr_tail: Arc<StdMutex<String>>,
    last_seq: Option<u64>,
    _registered: crate::reaper::Registered,
}

type Slot = Arc<Mutex<Option<Session>>>;

fn pool() -> &'static StdMutex<HashMap<(String, String), Slot>> {
    static POOL: OnceLock<StdMutex<HashMap<(String, String), Slot>>> = OnceLock::new();
    POOL.get_or_init(Default::default)
}

fn slot(conversation: &str, agent: &str) -> Slot {
    pool().lock().unwrap().entry((conversation.to_string(), agent.to_string())).or_default().clone()
}

/// The stdin line for one user message.
pub fn user_line(text: &str) -> String {
    serde_json::json!({ "event": "user", "message": { "content": text } }).to_string()
}

/// Whether a stream-json line is the one that ends a turn.
pub fn is_turn_end(line: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(line).map(|v| v["event"] == "result").unwrap_or(false)
}

fn pick_input(reused: bool, last_seq: Option<u64>, full: &str, delta: &(dyn Fn(u64) -> String + Send + Sync)) -> String {
    match (reused, last_seq) {
        (true, Some(seq)) => delta(seq),
        _ => full.to_string(),
    }
}

fn keep_tail(s: &mut String, max: usize) {
    if s.len() > max {
        let cut = (s.len() - max..=s.len()).find(|&i| s.is_char_boundary(i)).unwrap_or(s.len());
        s.drain(..cut);
    }
}

enum Failure {
    Exited { saw_output: bool, why: String },
    Other(String),
}

impl Failure {
    fn into_message(self) -> String {
        match self {
            Failure::Exited { why, .. } | Failure::Other(why) => why,
        }
    }
}

/// Runs one turn, reusing the chat's live process when its spec still matches. Returns the
/// turn's stdout lines through its `result` line, for `antigravity_reply`.
pub async fn run_turn<F>(
    turn: Turn<'_>,
    start: &Starter,
    cancel: &CancellationToken,
    timeout: Duration,
    mut on_line: F,
) -> Result<String, String>
where
    F: FnMut(&str) -> Result<(), String> + Send,
{
    let slot = slot(turn.conversation, turn.agent);
    let mut guard = slot.lock().await;
    let stale = match guard.as_mut() {
        Some(s) => s.spec != turn.spec || s.child.try_wait().map(|st| st.is_some()).unwrap_or(true),
        None => false,
    };
    if stale {
        if let Some(s) = guard.take() {
            close(s).await;
        }
    }
    let mut retried = false;
    loop {
        let reused = guard.is_some();
        if !reused {
            *guard = Some(spawn(&turn.spec, start)?);
        }
        let s = guard.as_mut().expect("a session was just ensured");
        let input = pick_input(reused, s.last_seq, turn.full_input, turn.delta_input);
        match one_turn(s, &input, cancel, timeout, &mut on_line).await {
            Ok(out) => {
                s.last_seq = Some(turn.seq);
                return Ok(out);
            }
            Err(Failure::Exited { saw_output: false, .. }) if reused && !retried => {
                retried = true;
                if let Some(s) = guard.take() {
                    kill(s).await;
                }
            }
            Err(f) => {
                if let Some(s) = guard.take() {
                    kill(s).await;
                }
                return Err(f.into_message());
            }
        }
    }
}

fn spawn(spec: &Spec, start: &Starter) -> Result<Session, String> {
    let mut child = start(spec).map_err(|e| format!("couldn't start {}: {e}", spec.exe.display()))?;
    let registered = crate::reaper::Registered::new(child.id());
    let stdin = child.stdin.take().ok_or("Antigravity's stdin isn't piped")?;
    let stdout = child.stdout.take().ok_or("Antigravity's stdout isn't piped")?;
    let stderr_tail = Arc::new(StdMutex::new(String::new()));
    if let Some(mut err) = child.stderr.take() {
        let tail = stderr_tail.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            loop {
                match err.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let mut t = tail.lock().unwrap();
                        t.push_str(&String::from_utf8_lossy(&buf[..n]));
                        keep_tail(&mut t, 4096);
                    }
                }
            }
        });
    }
    Ok(Session {
        spec: spec.clone(),
        child,
        stdin,
        lines: BufReader::new(stdout).lines(),
        stderr_tail,
        last_seq: None,
        _registered: registered,
    })
}

async fn one_turn<F>(
    s: &mut Session,
    input: &str,
    cancel: &CancellationToken,
    timeout: Duration,
    on_line: &mut F,
) -> Result<String, Failure>
where
    F: FnMut(&str) -> Result<(), String> + Send,
{
    let mut line = input.trim_end().to_string();
    line.push('\n');
    let wrote = async {
        s.stdin.write_all(line.as_bytes()).await?;
        s.stdin.flush().await
    }
    .await;
    if let Err(e) = wrote {
        return Err(Failure::Exited { saw_output: false, why: format!("couldn't write to Antigravity: {e}") });
    }
    let tail = s.stderr_tail.clone();
    let mut out = String::new();
    let mut saw_output = false;
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);
    loop {
        let next = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(Failure::Other("stopped".to_string())),
            _ = &mut deadline => return Err(Failure::Other(format!("no answer within {} seconds", timeout.as_secs()))),
            next = s.lines.next_line() => next,
        };
        match next {
            Ok(Some(l)) => {
                let l = l.trim_end_matches('\r');
                saw_output = true;
                out.push_str(l);
                out.push('\n');
                on_line(l).map_err(Failure::Other)?;
                if is_turn_end(l) {
                    return Ok(out);
                }
            }
            Ok(None) | Err(_) => {
                let err = tail.lock().unwrap().trim().to_string();
                let why = if err.is_empty() {
                    "Antigravity exited in the middle of a turn".to_string()
                } else {
                    format!("Antigravity exited in the middle of a turn: {err}")
                };
                return Err(Failure::Exited { saw_output, why });
            }
        }
    }
}

async fn kill_child(child: &mut Child) {
    #[cfg(windows)]
    {
        if let Some(pid) = child.id() {
            let _ = tokio::process::Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .creation_flags(0x0800_0000)
                .output()
                .await;
        }
    }
    let _ = child.kill().await;
}

async fn kill(mut s: Session) {
    kill_child(&mut s.child).await;
}

/// Closes stdin so `agy` exits by itself; kills it if it hasn't within 5 seconds.
async fn close(s: Session) {
    let Session { mut child, stdin, .. } = s;
    drop(stdin);
    if tokio::time::timeout(Duration::from_secs(5), child.wait()).await.is_err() {
        kill_child(&mut child).await;
    }
}

/// Ends one chat's process for an AI (the chat was deleted, or the AI left the council).
pub async fn end(conversation: &str, agent: &str) {
    let slot = pool().lock().unwrap().remove(&(conversation.to_string(), agent.to_string()));
    if let Some(slot) = slot {
        if let Some(s) = slot.lock().await.take() {
            close(s).await;
        }
    }
}

/// Closes every process, for app exit.
pub async fn end_all() {
    let slots: Vec<Slot> = pool().lock().unwrap().drain().map(|(_, v)| v).collect();
    for slot in slots {
        if let Some(s) = slot.lock().await.take() {
            close(s).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_line_ends_a_turn() {
        assert!(is_turn_end(r#"{"event":"result","result":{"status":"SUCCESS"}}"#));
        assert!(!is_turn_end(r#"{"event":"step_update","step_update":{}}"#));
        assert!(!is_turn_end("not json"));
        assert!(!is_turn_end(""));
    }

    #[test]
    fn new_process_gets_everything_reused_one_gets_news() {
        let delta = |seq: u64| format!("after {seq}");
        assert_eq!(pick_input(false, None, "FULL", &delta), "FULL");
        assert_eq!(pick_input(false, Some(7), "FULL", &delta), "FULL");
        assert_eq!(pick_input(true, None, "FULL", &delta), "FULL");
        assert_eq!(pick_input(true, Some(7), "FULL", &delta), "after 7");
    }

    #[test]
    fn user_line_is_one_ndjson_message() {
        let line = user_line("say \"hi\"\nthen stop");
        assert!(!line.contains('\n'));
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["event"], "user");
        assert_eq!(v["message"]["content"], "say \"hi\"\nthen stop");
    }

    #[test]
    fn changed_arguments_mean_a_new_process() {
        let a = Spec { exe: "agy".into(), dir: "F:\\p".into(), args: vec!["--mode".into(), "plan".into()] };
        let mut b = a.clone();
        assert_eq!(a, b);
        b.args[1] = "accept-edits".into();
        assert_ne!(a, b);
    }

    #[test]
    fn stderr_tail_stays_bounded_on_char_boundaries() {
        let mut s = "Ã©".repeat(5_000);
        keep_tail(&mut s, 4096);
        assert!(s.len() <= 4096);
        assert!(s.chars().all(|c| c == 'Ã©'));
    }
}