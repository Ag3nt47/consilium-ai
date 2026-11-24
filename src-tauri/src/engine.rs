//! Runs the council's turns for one process: the app's window, or the terminal command (see `cli`).
//!
//! The app and the terminal can be open at the same time on the same conversations, since they
//! share one data folder:
//! - A conversation's AIs are run by one process at a time, the one holding its turn lock
//!   (`Store::try_turn_lock`). The lock belongs to the process, so a quit or crash frees it.
//! - A message sent from the other process is only recorded there. The process running the turn
//!   sees it in the conversation (`watch`), and the AIs read it exactly as if it had been sent here.
//! - Stop in the other process leaves a stop request, which the running process acts on.
//! - The app's window also shows what the terminal adds to any conversation, as it happens.

use crate::agents::{self, Agent};
use crate::bench::{self, Bench};
use crate::config::{Access, Config};
use crate::council::{self, Council};
use crate::memory::{self, Memory};
use crate::prompt;
use crate::resume::{Journal, Pending};
use crate::store::{self, Event, Store, TurnLock};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// The newest council-memory characters a local model is shown.
const LOCAL_MEMORY_CHARS: usize = 1_500;

/// Upper bound on discussion rounds, whatever the UI asks for, so agents can't loop indefinitely.
pub const MAX_ROUNDS: u32 = 8;

/// Answers each AI gives in a turn started by a linked computer's message (see `from_linked`).
const LINKED_ROUNDS: u32 = 1;

/// How long Stop waits for a turn's AIs to shut down before letting it go anyway, so the
/// conversation is never stuck as "still answering".
pub const STOP_GRACE: Duration = Duration::from_secs(5);

/// How often `watch` looks for what the other process has done.
const WATCH_EVERY: Duration = Duration::from_millis(400);

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct TurnState {
    pub conversation: String,
    /// "running" or "idle".
    pub state: String,
    /// The first AI working on an answer, if any.
    pub agent: Option<String>,
    /// Every AI working on an answer right now; they answer at the same time.
    pub agents: Vec<String>,
    pub round: u32,
    pub rounds: u32,
}

/// Live, non-persistent provider telemetry for one AI. It contains observable phases and tool
/// actions only; hidden reasoning text is never requested or emitted.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct AgentActivity {
    pub conversation: String,
    pub agent: String,
    pub kind: String,
    pub label: String,
    pub detail: String,
    pub started_at: u64,
    pub updated_at: u64,
}

impl TurnState {
    pub fn idle(conversation: &str) -> Self {
        TurnState { conversation: conversation.into(), state: "idle".into(), agent: None, agents: vec![], round: 0, rounds: 0 }
    }
}

/// Where a process shows what happens: the app's window, or the terminal.
pub trait Ui: Send + Sync + 'static {
    /// An event was recorded (by this process, or, when watching, by the other one).
    fn event(&self, e: &Event);
    fn turn(&self, t: TurnState);
    fn activity(&self, activity: &AgentActivity);
}

pub type Task = Pin<Box<dyn Future<Output = ()> + Send>>;
/// Starts a background task on the process's async runtime.
pub type Spawn = dyn Fn(Task) + Send + Sync;
/// Makes the host a turn uses to reach the AIs (the real ones, or fakes in tests).
pub type MakeHost = dyn Fn(&Engine, &Pending) -> Arc<dyn council::Host> + Send + Sync;

/// What `watch` needs of a running turn: conversation, turn id, start time, council and cursor.
type Watched = (String, String, u64, Arc<Council>, Arc<Mutex<u64>>);

/// A turn this process is running.
#[derive(Clone)]
struct Running {
    /// The turn's id (see `resume::Pending::turn`).
    turn: String,
    token: CancellationToken,
    /// Takes the human's further messages while the turn runs.
    council: Arc<Council>,
    /// When it started (ms); a stop request from before then is stale.
    started: u64,
    /// This process's right to run the conversation's AIs.
    lock: Arc<TurnLock>,
    /// The newest event checked for messages sent from the other process. Locked while such a
    /// message is joined into the turn, so `watch` and the turn's end never both take it, or miss it.
    cursor: Arc<Mutex<u64>>,
}

/// One machine-wide local-AI slot. OS file locks are shared by the desktop app and terminal
/// client and are released automatically after a crash, unlike an in-memory semaphore.
struct AgentSlot {
    _file: File,
}

fn try_agent_slot(dir: &std::path::Path, limit: usize) -> Result<Option<AgentSlot>, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("couldn't create the PC-capacity gate: {e}"))?;
    for index in 0..limit {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(format!("{index}.lock")))
            .map_err(|e| format!("couldn't open the PC-capacity gate: {e}"))?;
        if file.try_lock().is_ok() {
            return Ok(Some(AgentSlot { _file: file }));
        }
    }
    Ok(None)
}

async fn wait_for_agent_slot(
    dir: PathBuf,
    limit: usize,
    cancel: &CancellationToken,
) -> Result<Option<AgentSlot>, String> {
    loop {
        match try_agent_slot(&dir, limit) {
            Ok(Some(slot)) => return Ok(Some(slot)),
            // If the safety gate itself is unavailable, fail forward: provider timeouts and Stop
            // still work, and the user is not locked out of every AI by a filesystem problem.
            Err(_) => return Ok(None),
            Ok(None) => {}
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(250)) => {}
            _ = cancel.cancelled() => return Err("stopped".into()),
        }
    }
}

pub struct Engine {
    /// The data folder.
    pub data: PathBuf,
    pub store: Arc<Store>,
    pub config: Arc<Config>,
    pub journal: Arc<Journal>,
    pub memory: Arc<Memory>,
    pub bench: Arc<Bench>,
    pub ui: Arc<dyn Ui>,
    spawn: Box<Spawn>,
    make_host: Box<MakeHost>,
    running: Mutex<HashMap<String, Running>>,
    /// Ids of the human messages sent from this process, to tell them from the other process's.
    mine: Mutex<HashSet<String>>,
    /// Shared OS-lock folder and portable CPU/RAM-based limit for local provider CLIs.
    agent_slot_dir: PathBuf,
    agent_slot_limit: usize,
}

impl Engine {
    /// An engine on the data folder `data` (the same for the app and the terminal command).
    pub fn open(data: &std::path::Path, ui: Arc<dyn Ui>, spawn: Box<Spawn>) -> std::io::Result<Self> {
        crate::apiagent::init(data.join("api-keys.json"));
        crate::customai::init(data.join("custom-ais.json"));
        let agent_slot_limit = crate::pressure::parallel_agent_limit();
        Ok(Engine {
            data: data.to_path_buf(),
            store: Arc::new(Store::new(data.join("conversations"))?),
            config: Arc::new(Config::load(data.join("council.json"))),
            journal: Arc::new(Journal::load(data.join("turns.json"))),
            memory: Arc::new(Memory::new(data.join("memory.md"))),
            bench: Arc::new(Bench::with_usage(data.to_path_buf())),
            ui,
            spawn,
            make_host: Box::new(|engine, pending| Arc::new(RealHost::new(engine, pending))),
            running: Mutex::new(HashMap::new()),
            mine: Mutex::new(HashSet::new()),
            agent_slot_dir: data.join("agent-slots"),
            agent_slot_limit,
        })
    }

    /// The same engine with other AIs (for tests).
    #[cfg(test)]
    pub fn with_host(mut self, make_host: Box<MakeHost>) -> Self {
        self.make_host = make_host;
        self
    }

    /// Records the human's message, then asks the addressed council members, who all answer at the
    /// same time and react to each other's messages as they arrive (see `council`).
    ///
    /// `targets` are agent ids. `rounds` is 1 for a single answer from each; more lets each AI
    /// answer up to that many times as the discussion goes on, up to MAX_ROUNDS.
    ///
    /// Sending while the AIs are answering doesn't interrupt them: the message joins the running
    /// turn, here or in the other process, and those it addresses who weren't taking part join in.
    #[cfg(test)]
    pub fn send(self: &Arc<Self>, id: &str, text: &str, targets: Vec<String>, rounds: u32) -> Result<Event, String> {
        self.send_with(id, text, vec![], targets, rounds)
    }

    /// `send`, with files attached (saved first with `attach`).
    pub fn send_with(
        self: &Arc<Self>,
        id: &str,
        text: &str,
        attachments: Vec<crate::attach::Attachment>,
        targets: Vec<String>,
        rounds: u32,
    ) -> Result<Event, String> {
        self.send_via(id, text, attachments, targets, rounds, None)
    }

    pub fn send_via(
        self: &Arc<Self>,
        id: &str,
        text: &str,
        attachments: Vec<crate::attach::Attachment>,
        targets: Vec<String>,
        rounds: u32,
        via: Option<&str>,
    ) -> Result<Event, String> {
        if crate::peer_sync::is_mirror(id) {
            // Another computer owns this conversation: it records the message and answers.
            if !attachments.is_empty() {
                return Err("Files can't be sent into another computer's conversation yet; attach them on that computer.".into());
            }
            let text = text.trim();
            if text.is_empty() {
                return Err("the message is empty".into());
            }
            let event = crate::peer_sync::reply(id, text, &targets)?;
            self.ui.event(&event);
            // That computer answers it, and so do this computer's AIs, here in the copy; their
            // answers go back to it (see `peer_sync`).
            self.from_linked(id, &event, &|| true);
            return Ok(event);
        }
        if via.is_none() {
            // The human is at this computer, so it should be the one that answers Signal.
            crate::link::note_local_activity();
        }
        let dir = self.store.files_dir(id);
        if let Some(a) = attachments.iter().find(|a| !crate::attach::belongs(&dir, a)) {
            return Err(format!("{} isn't one of this conversation's attachments", a.name));
        }
        let names: Vec<&str> = attachments.iter().map(|a| a.name.as_str()).collect();
        let text = match text.trim() {
            "" if !names.is_empty() => format!("(Attached: {})", names.join(", ")),
            "" => return Err("the message is empty".into()),
            t => t.to_string(),
        };
        let text = text.as_str();
        let targets = self.addressed(targets, text);
        // AIs switched off in this chat aren't asked, even when @mentioned.
        let off = crate::switched::off(self.store.dir(), id);
        let stopped: Vec<String> = targets.iter().filter(|t| off.contains(t)).cloned().collect();
        let targets: Vec<String> = targets.into_iter().filter(|t| !off.contains(t)).collect();
        // If the human message didn't explicitly @mention any agent, apply AI tiering:
        // Tier 1 flagship models take the work first; Tier 2 local models act as standby fallback.
        let targets = if prompt::mentioned_agents(text).is_empty() {
            self.tier_targets(&targets)
        } else {
            targets
        };
        let rounds = rounds.clamp(1, MAX_ROUNDS);
        // The turn table is held only to file the message and look up the turn; the window, notes,
        // linked computers and the council are told after it's let go (see #14: a freeze guard
        // restart while the table stayed taken).
        let event = {
            let _running = self.running.lock().unwrap();
            let event = self.store.append_with_attachments_via(id, targets.clone(), text, attachments, via)?;
            self.mine.lock().unwrap().insert(event.id.clone());
            event
        };
        self.ui.event(&event);
        if !stopped.is_empty() {
            let names: Vec<String> = stopped.iter().map(|s| format!("@{s}")).collect();
            let _ = self.note(id, &format!(
                "{} is switched off in this chat, so it wasn't asked. Press the green \u{25B6} next to it in the council box to switch it back on.",
                names.join(" and ")
            ));
        }
        let mut tried: Option<String> = None;
        loop {
            let mut running = self.running.lock().unwrap();
            let lock = match running.get(id) {
                // A turn is running that hasn't turned this message down yet: join it, unlocked.
                Some(r) if tried.as_deref() != Some(r.turn.as_str()) => {
                    let (council, turn) = (r.council.clone(), r.turn.clone());
                    drop(running);
                    if council.join(&event, &targets) {
                        self.journal.join(id, &turn, &targets);
                        return Ok(event);
                    }
                    // That turn has just ended; look again, since another may have replaced it.
                    tried = Some(turn);
                    continue;
                }
                // The turn that just ended is still listed; this message starts a new one, under the same lock.
                Some(r) => r.lock.clone(),
                None => {
                    if targets.is_empty() {
                        return Ok(event);
                    }
                    match self.store.try_turn_lock(id) {
                        Some(l) => Arc::new(l),
                        // The other process is running this conversation's AIs; it reads this message.
                        None => return Ok(event),
                    }
                }
            };
            if targets.is_empty() {
                return Ok(event);
            }
            self.begin(&mut running, Pending::new(id, targets, rounds), lock, event.seq);
            return Ok(event);
        }
    }

    /// Starts a background task on this process's async runtime.
    pub fn spawn_task(&self, task: Task) {
        (self.spawn)(task);
    }

    /// The model each AI last answered with, where its CLI says (see `agents::AgentReply::model`).
    pub fn last_models(&self) -> HashMap<String, String> {
        read_models(&self.data)
    }

    /// Saves files for attaching to the next message in `id`: each is copied into the conversation's
    /// files folder. `files` are paths on this computer.
    pub fn attach(&self, id: &str, files: &[PathBuf]) -> Result<Vec<crate::attach::Attachment>, String> {
        self.store.meta(id)?;
        let dir = self.store.files_dir(id);
        files.iter().map(|f| crate::attach::save_file(&dir, f)).collect()
    }

    /// Saves file contents (a pasted picture, say) for attaching to the next message in `id`.
    pub fn attach_bytes(&self, id: &str, name: &str, bytes: &[u8]) -> Result<crate::attach::Attachment, String> {
        self.store.meta(id)?;
        crate::attach::save(&self.store.files_dir(id), name, bytes)
    }

    /// Records a message, with any files attached (saved first with `attach`), without asking any
    /// AI to answer.
    pub fn say_with(&self, id: &str, text: &str, attachments: Vec<crate::attach::Attachment>) -> Result<Event, String> {
        let dir = self.store.files_dir(id);
        if let Some(a) = attachments.iter().find(|a| !crate::attach::belongs(&dir, a)) {
            return Err(format!("{} isn't one of this conversation's attachments", a.name));
        }
        let names: Vec<&str> = attachments.iter().map(|a| a.name.as_str()).collect();
        let text = match text.trim() {
            "" if !names.is_empty() => format!("(Attached: {})", names.join(", ")),
            "" => return Err("the message is empty".into()),
            t => t.to_string(),
        };
        let event = self.store.append_with_attachments(id, vec![], &text, attachments)?;
        self.mine.lock().unwrap().insert(event.id.clone());
        self.ui.event(&event);
        Ok(event)
    }

    /// A message from a linked computer was filed in `id`: the human's message in another
    /// computer's conversation copied here, or another computer's AI answering in either kind of
    /// conversation (see `peer_sync`). A turn running here takes it in, the human's message as
    /// `join` and an AI's as `heard`. Otherwise this computer's council members answer it in a
    /// turn of their own, if `may_start` allows (it limits how often AIs alone can start one).
    pub fn from_linked(self: &Arc<Self>, id: &str, event: &Event, may_start: &dyn Fn() -> bool) {
        let members: Vec<String> = self
            .config
            .get()
            .members
            .into_iter()
            .filter(|m| !crate::switched::is_off(self.store.dir(), id, m))
            .collect();
        // Another computer's AI may address AIs here by handle, e.g. "@claude-<this computer>".
        let addressed: Vec<String> = if event.sender == "human" {
            vec![]
        } else {
            prompt::mentioned_here(&event.text)
                .into_iter()
                .filter(|m| members.contains(m) && !crate::switched::is_off(self.store.dir(), id, m))
                .collect()
        };
        // As in `send_via`: the turn table is never held while the council takes the message in.
        let mut tried: Option<String> = None;
        let mut allowed: Option<bool> = None;
        loop {
            let live = self.running.lock().unwrap().get(id).map(|r| (r.council.clone(), r.turn.clone()));
            if let Some((council, turn)) = live.filter(|(_, t)| tried.as_deref() != Some(t.as_str())) {
                let taken = if event.sender == "human" {
                    let joined = council.join(event, &members);
                    if joined {
                        self.journal.join(id, &turn, &members);
                    }
                    joined
                } else if !addressed.is_empty() {
                    council.mentioned(event, &addressed)
                } else {
                    council.heard(event)
                };
                if taken {
                    return;
                }
                tried = Some(turn);
            }
            // Asked once only: `may_start` counts each yes.
            if members.is_empty() || !*allowed.get_or_insert_with(may_start) {
                return;
            }
            let mut running = self.running.lock().unwrap();
            let lock = match running.get(id) {
                // A newer turn started meanwhile: give the message to it instead.
                Some(r) if tried.as_deref() != Some(r.turn.as_str()) => continue,
                // That turn has just ended; this one starts under the same lock.
                Some(r) => r.lock.clone(),
                None => match self.store.try_turn_lock(id) {
                    Some(l) => Arc::new(l),
                    // The other process is running this conversation's AIs; it reads this message.
                    None => return,
                },
            };
            // Those addressed answer; otherwise everyone may react.
            let targets = if addressed.is_empty() { members } else { addressed };
            self.begin(&mut running, Pending::new(id, targets, LINKED_ROUNDS), lock, event.seq);
            return;
        }
    }

    /// The council members a message is for. A message that @mentions council members goes to
    /// those only, whoever is ticked in the box; otherwise it goes to the ticked ones.
    fn addressed(&self, targets: Vec<String>, text: &str) -> Vec<String> {
        route(targets, prompt::mentioned_agents(text), |a| self.config.is_member(a))
    }

    /// Whether an agent can answer right now: known, installed, and not benched / out of quota.
    pub fn is_available(&self, agent_id: &str) -> bool {
        let Some(a) = crate::agents::by_id(agent_id) else { return false };
        crate::agents::installed(a) && self.bench.until(agent_id, store::now_ms()).is_none()
    }

    /// Tiered candidate selection: flagship paid models take work first; local offline models act
    /// as silent standby fallback when all active flagship models exhaust their quota or rate limits.
    pub fn tier_targets(&self, candidates: &[String]) -> Vec<String> {
        select_tier_targets(candidates, crate::agents::is_local_id, |id| self.is_available(id))
    }

    /// Sets up the turn `pending` and runs it in the background. `cursor` is the newest event it
    /// has taken into account; messages from the other process after it are joined in by `watch`.
    fn begin(self: &Arc<Self>, running: &mut HashMap<String, Running>, pending: Pending, lock: Arc<TurnLock>, cursor: u64) {
        let host = (self.make_host)(self, &pending);
        let turn = council::Turn {
            conversation: pending.conversation.clone(),
            targets: pending.targets.clone(),
            rounds: pending.rounds,
            members: self.config.get().members,
        };
        let token = CancellationToken::new();
        let council = Council::new(turn, self.store.clone(), self.bench.clone(), host, token.clone());
        let live = Running {
            turn: pending.turn.clone(),
            token,
            council: Arc::new(council),
            started: store::now_ms(),
            lock,
            cursor: Arc::new(Mutex::new(cursor)),
        };
        running.insert(pending.conversation.clone(), live.clone());
        self.journal.begin(pending.clone());
        // So this turn's AIs, and the next one's, know where everyone's usage stands.
        crate::usage::refresh_if_stale(self);
        let engine = self.clone();
        (self.spawn)(Box::pin(async move { engine.run(pending, live).await }));
    }

    async fn run(self: Arc<Self>, pending: Pending, live: Running) {
        let conv = pending.conversation.clone();
        live.council.run().await;
        // Messages from the other process that arrived as the turn was ending get a turn of their own,
        // before the conversation is let go.
        if !live.token.is_cancelled() {
            let (unanswered, cursor) = {
                let mut c = live.cursor.lock().unwrap();
                let (messages, last) = self.sent_elsewhere(&conv, *c);
                *c = last;
                (messages, last)
            };
            let targets = self.targets_of(&unanswered, &conv);
            let mut running = self.running.lock().unwrap();
            if !targets.is_empty() && running.get(&conv).is_some_and(|r| r.turn == pending.turn) {
                self.journal.finish(&conv, &pending.turn);
                self.begin(&mut running, Pending::new(&conv, targets, pending.rounds), live.lock.clone(), cursor);
                return;
            }
        }
        let cursor = *live.cursor.lock().unwrap();
        let stopped = live.token.is_cancelled();
        drop(live);
        self.release(&conv, &pending.turn);
        if !stopped {
            self.schedule_auto_continue(&conv);
        }
        // A message sent from the other process just before the lock was let go found it still
        // taken, so its sender left it to this process.
        let (late, last) = self.sent_elsewhere(&conv, cursor);
        let targets = self.targets_of(&late, &conv);
        if targets.is_empty() || late.iter().any(|m| self.answered(&conv, m.seq)) {
            return;
        }
        let mut running = self.running.lock().unwrap();
        if running.contains_key(&conv) {
            return;
        }
        if let Some(lock) = self.store.try_turn_lock(&conv) {
            self.begin(&mut running, Pending::new(&conv, targets, pending.rounds), Arc::new(lock), last);
        }
    }

    /// After a turn ends, waits a little (so the human can step in), then starts another round if
    /// the task board still has open work. See `autocontinue`.
    fn schedule_auto_continue(self: &Arc<Self>, conv: &str) {
        if !self.config.get().auto_continue {
            return;
        }
        let engine = self.clone();
        let conv = conv.to_string();
        let wait = crate::autocontinue::delay_secs(crate::auto_run::is_on(self.store.dir(), &conv));
        (self.spawn)(Box::pin(async move {
            tokio::time::sleep(Duration::from_secs(wait)).await;
            engine.auto_continue(&conv);
        }));
    }

    /// Looks again later while a command box holds the next round back.
    fn recheck_auto_continue(self: &Arc<Self>, conv: &str) {
        let engine = self.clone();
        let conv = conv.to_string();
        (self.spawn)(Box::pin(async move {
            tokio::time::sleep(Duration::from_secs(crate::autocontinue::RECHECK_SECS)).await;
            engine.auto_continue(&conv);
        }));
    }

    fn auto_continue(self: &Arc<Self>, conv: &str) {
        if !self.config.get().auto_continue || self.busy(conv) {
            return;
        }
        let events = self.store.load(conv).unwrap_or_default();
        let board = crate::tasks::load(self.store.dir(), conv);
        let auto_run = crate::auto_run::is_on(self.store.dir(), conv);
        let pending = auto_run && crate::autocontinue::pending_boxes(&events, crate::store::now_ms());
        match crate::autocontinue::decide_with(&events, &board, auto_run, pending) {
            crate::autocontinue::Decision::Continue(note) => {
                let _ = self.note_for_council(conv, &note);
            }
            crate::autocontinue::Decision::Pause(note) => {
                let _ = self.note(conv, &note);
            }
            crate::autocontinue::Decision::Recheck => self.recheck_auto_continue(conv),
            crate::autocontinue::Decision::Stop => {}
        }
    }

    /// The human's messages sent from the other process after event `after`, and the newest event.
    fn sent_elsewhere(&self, conv: &str, after: u64) -> (Vec<Event>, u64) {
        let events = self.store.load(conv).unwrap_or_default();
        let last = events.last().map_or(after, |e| e.seq.max(after));
        let mine = self.mine.lock().unwrap();
        let messages = events
            .into_iter()
            .filter(|e| e.seq > after && e.sender == "human" && e.kind == "message" && !mine.contains(&e.id))
            .collect();
        (messages, last)
    }

    /// Council members addressed by any of `messages`.
    fn targets_of(&self, messages: &[Event], conv: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for m in messages {
            for t in &m.to {
                if self.config.is_member(t) && !out.contains(t) && !crate::switched::is_off(self.store.dir(), conv, t) {
                    out.push(t.clone());
                }
            }
        }
        out
    }

    /// Whether an AI has said anything after event `seq`.
    fn answered(&self, conv: &str, seq: u64) -> bool {
        self.store
            .load(conv)
            .unwrap_or_default()
            .iter()
            .any(|e| e.seq > seq && e.sender != "human" && e.sender != council::CONSILIUM)
    }

    /// Marks the turn `turn` as over: the conversation can take new messages, and it won't resume.
    /// Does nothing to a newer turn that has replaced it.
    fn release(&self, conversation: &str, turn: &str) {
        self.journal.finish(conversation, turn);
        let mut r = self.running.lock().unwrap();
        if r.get(conversation).is_some_and(|r| r.turn == turn) {
            r.remove(conversation);
            drop(r);
            self.ui.turn(TurnState::idle(conversation));
        }
    }

    /// Stops the AIs in a conversation, whichever process is running them. Returns at once,
    /// with whether there was anything to stop.
    pub fn stop(self: &Arc<Self>, id: &str) -> bool {
        if self.running.lock().unwrap().contains_key(id) {
            let engine = self.clone();
            let id = id.to_string();
            (self.spawn)(Box::pin(async move {
                engine.stop_and_wait(&id).await;
            }));
            return true;
        }
        if self.store.turn_locked(id) {
            return self.store.request_stop(id).is_ok();
        }
        false
    }

    /// Stops this process's turn in `conversation`, if any, and waits for its AIs to shut down. One
    /// that hasn't finished after STOP_GRACE is let go: the conversation is free again either way.
    pub async fn stop_and_wait(&self, conversation: &str) -> bool {
        let entry = self.running.lock().unwrap().get(conversation).cloned();
        let Some(Running { turn, token, .. }) = entry else { return false };
        token.cancel();
        let started = std::time::Instant::now();
        while started.elapsed() < STOP_GRACE {
            let still = self.running.lock().unwrap().get(conversation).is_some_and(|r| r.turn == turn);
            if !still {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        self.release(conversation, &turn);
        true
    }

    /// Stops every turn this process is running (when the terminal command exits).
    pub async fn stop_all(&self) {
        let ids: Vec<String> = self.running.lock().unwrap().keys().cloned().collect();
        for id in ids {
            self.stop_and_wait(&id).await;
        }
    }

    /// Whether the turn table's lock is free right now (it's only ever held for moments; see
    /// `stall`, which restarts the app when it stays taken).
    pub fn turns_lock_free(&self) -> bool {
        self.running.try_lock().is_ok()
    }

    /// Whether this process runs a turn in `id`.
    pub fn running_here(&self, id: &str) -> bool {
        self.running.lock().unwrap().contains_key(id)
    }

    /// Whether the AIs are working in `id`, in this process or the other.
    pub fn busy(&self, id: &str) -> bool {
        self.running_here(id) || self.store.turn_locked(id)
    }

    /// Conversations whose AIs are working, in this process or the other.
    pub fn running_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.running.lock().unwrap().keys().cloned().collect();
        for m in self.store.list() {
            if !ids.contains(&m.id) && self.store.turn_locked(&m.id) {
                ids.push(m.id);
            }
        }
        ids
    }

    /// Continues turns that a quit, crash, restart or update cut short, in every conversation, or
    /// only in `only`. A turn the other process is running is left to it.
    pub fn resume_interrupted(self: &Arc<Self>, only: Option<&str>) {
        let (resume, give_up) = self.journal.interrupted_where(|p| {
            only.is_none_or(|c| c == p.conversation) && !self.running_here(&p.conversation) && !self.store.turn_locked(&p.conversation)
        });
        for p in give_up {
            let _ = self.note(
                &p.conversation,
                "Couldn't resume the AIs' turn after two tries. Send your message again to continue.",
            );
        }
        for mut p in resume {
            if self.store.meta(&p.conversation).is_err() {
                self.journal.finish(&p.conversation, &p.turn);
                continue;
            }
            let Some(lock) = self.store.try_turn_lock(&p.conversation) else { continue };
            if p.turn.is_empty() {
                // Recorded by a version without turn ids.
                self.journal.finish(&p.conversation, "");
                p.turn = uuid::Uuid::new_v4().to_string();
            }
            let _ = self.note(
                &p.conversation,
                "Consilium restarted; the council is carrying on where it left off. If you were cut off \
before posting your reply, your earlier work may already be partly on disk: check `git status` and \
`git diff` in the folders you were working in, continue from there instead of starting over, and \
don't repeat changes that are already made.",
            );
            let cursor = self.store.load(&p.conversation).unwrap_or_default().last().map_or(0, |e| e.seq);
            let mut running = self.running.lock().unwrap();
            if running.contains_key(&p.conversation) {
                continue;
            }
            self.begin(&mut running, p, Arc::new(lock), cursor);
        }
    }

    /// A note from Consilium that also asks the council to act on it: posted like `note`, then,
    /// if nobody is answering in this conversation right now, it starts one round for every
    /// member. Used when GitHub's checks fail, so the AIs pick up the fix without being asked.
    /// When a turn is already running the note is only posted; the AIs read it in that turn.
    pub fn note_for_council(self: &Arc<Self>, conversation: &str, text: &str) -> Result<Event, String> {
        // Auto-continue and failed-check rounds skip AIs switched off in this chat.
        let members: Vec<String> = self
            .config
            .get()
            .members
            .into_iter()
            .filter(|m| !crate::switched::is_off(self.store.dir(), conversation, m))
            .collect();
        let targets = self.tier_targets(&members);
        let e = self.store.append(conversation, council::CONSILIUM, "system", targets.clone(), text)?;
        self.ui.event(&e);
        let mut running = self.running.lock().unwrap();
        if targets.is_empty() || running.contains_key(conversation) {
            return Ok(e);
        }
        let Some(lock) = self.store.try_turn_lock(conversation) else { return Ok(e) };
        self.begin(&mut running, Pending::new(conversation, targets, 1), Arc::new(lock), e.seq);
        Ok(e)
    }

    /// The AIs switched off in this conversation with the Stop button in their council box.
    pub fn switched_off(&self, conversation: &str) -> Vec<String> {
        crate::switched::off(self.store.dir(), conversation)
    }

    /// Switches an AI on or off in this conversation only. Returns the AIs now off there.
    pub fn set_ai_on(&self, conversation: &str, agent: &str, on: bool) -> Result<Vec<String>, String> {
        if !crate::store::valid_id(conversation) {
            return Err("unknown conversation".into());
        }
        if crate::agents::by_id(agent).is_none() {
            return Err("unknown AI".into());
        }
        let now_off = crate::switched::set(self.store.dir(), conversation, agent, on)?;
        if !on {
            // Switching it off also ends what it is doing now in this chat (its CLI or its local
            // model's request), not only its next turns.
            let council = self.running.lock().unwrap().get(conversation).map(|r| r.council.clone());
            if let Some(c) = council {
                c.stop_agent(agent);
            }
        }
        crate::peer_sync::share_switched(conversation, agent, on);
        Ok(now_off)
    }

    /// A note from Consilium itself, which the human and the AIs see.
    pub fn note(&self, conversation: &str, text: &str) -> Result<Event, String> {
        let e = self.store.append(conversation, council::CONSILIUM, "system", vec![], text)?;
        self.ui.event(&e);
        Ok(e)
    }

    /// Keeps this process in step with the other one, until the process ends: joins messages sent
    /// from there into the turns running here, acts on stop requests, and, with `mirror`, shows in
    /// the UI what the other process adds to any conversation and when its AIs are working.
    pub async fn watch(self: Arc<Self>, mirror: bool) {
        // For `mirror`: each conversation's log size and newest event when last looked at, and the
        // conversations whose AIs the other process is running.
        let mut seen: HashMap<String, (u64, u64)> = HashMap::new();
        let mut elsewhere: HashSet<String> = HashSet::new();
        if mirror {
            for m in self.store.list() {
                let last = self.store.load(&m.id).unwrap_or_default().last().map_or(0, |e| e.seq);
                seen.insert(m.id.clone(), (self.store.log_len(&m.id), last));
            }
        }
        loop {
            tokio::time::sleep(WATCH_EVERY).await;
            // Without the turn locks: holding one here could keep the conversation locked a moment
            // after its turn ends.
            let here: Vec<Watched> = self
                .running
                .lock()
                .unwrap()
                .iter()
                .map(|(k, r)| (k.clone(), r.turn.clone(), r.started, r.council.clone(), r.cursor.clone()))
                .collect();
            for (conv, turn, started, council, cursor) in &here {
                if self.store.take_stop_request(conv, *started) {
                    self.stop_and_wait(conv).await;
                    continue;
                }
                let mut c = cursor.lock().unwrap();
                let (messages, last) = self.sent_elsewhere(conv, *c);
                let mut all = true;
                for m in messages {
                    let targets = self.addressed(m.to.clone(), &m.text);
                    let targets: Vec<String> = targets.into_iter().filter(|t| !crate::switched::is_off(self.store.dir(), conv, t)).collect();
                    if targets.is_empty() {
                        *c = m.seq;
                        continue;
                    }
                    if !council.join(&m, &targets) {
                        // The turn is ending; `run` gives this message a turn of its own.
                        all = false;
                        break;
                    }
                    self.journal.join(conv, turn, &targets);
                    *c = m.seq;
                }
                if all {
                    *c = last;
                }
            }
            if !mirror {
                continue;
            }
            for m in self.store.list() {
                let len = self.store.log_len(&m.id);
                let (old_len, old_last) = seen.get(&m.id).copied().unwrap_or((0, 0));
                let changed = len != old_len;
                if changed {
                    let events = self.store.load(&m.id).unwrap_or_default();
                    let last = events.last().map_or(old_last, |e| e.seq);
                    // The window drops duplicates, so events this process already showed do no harm.
                    for e in events.iter().filter(|e| e.seq > old_last) {
                        self.ui.event(e);
                    }
                    seen.insert(m.id.clone(), (len, last));
                }
                if self.running_here(&m.id) {
                    elsewhere.remove(&m.id);
                    continue;
                }
                if changed || elsewhere.contains(&m.id) {
                    let busy = self.store.turn_locked(&m.id);
                    if busy && elsewhere.insert(m.id.clone()) {
                        self.ui.turn(TurnState {
                            conversation: m.id.clone(),
                            state: "running".into(),
                            agent: None,
                            agents: vec![],
                            round: 0,
                            rounds: 0,
                        });
                    } else if !busy && elsewhere.remove(&m.id) {
                        self.ui.turn(TurnState::idle(&m.id));
                    }
                }
            }
        }
    }
}

/// How a turn reaches the real AIs and the UI.
struct RealHost {
    data: PathBuf,
    store: Arc<Store>,
    config: Arc<Config>,
    memory: Arc<Memory>,
    journal: Arc<Journal>,
    ui: Arc<dyn Ui>,
    conversation: String,
    turn: String,
    rounds: u32,
    agent_slot_dir: PathBuf,
    agent_slot_limit: usize,
}

impl RealHost {
    fn new(engine: &Engine, pending: &Pending) -> Self {
        RealHost {
            data: engine.data.clone(),
            store: engine.store.clone(),
            config: engine.config.clone(),
            memory: engine.memory.clone(),
            journal: engine.journal.clone(),
            ui: engine.ui.clone(),
            conversation: pending.conversation.clone(),
            turn: pending.turn.clone(),
            rounds: pending.rounds,
            agent_slot_dir: engine.agent_slot_dir.clone(),
            agent_slot_limit: engine.agent_slot_limit,
        }
    }
}

/// Reads the project folder's git summary for linked computers, off the async executor and
/// bounded, so a slow or non-git folder never delays an answer.
async fn refresh_git_summary(path: PathBuf) {
    if let Ok(Ok(summary)) =
        tokio::time::timeout(Duration::from_secs(3), tokio::task::spawn_blocking(move || prompt::git_summary(&path))).await
    {
        crate::link::set_git_summary(summary);
    }
}

impl council::Host for RealHost {
    fn ask(&self, agent: &'static Agent, answer: u32, rounds: u32, events: Vec<Event>, cancel: CancellationToken) -> council::Reply {
        let conv = self.conversation.as_str();
        // Read the folder fresh each time, so a change in the UI applies from the next answer.
        // A conversation without its own folder uses the council's default one, if set.
        let workspace = self
            .store
            .meta(conv)
            .ok()
            .and_then(|m| m.workspace)
            .or_else(|| self.config.get().default_workspace)
            .filter(|w| std::path::Path::new(w).is_dir());
        let access = agents::effective_access(
            agent.id,
            if workspace.is_some() { self.config.access(agent.id) } else { Access::Chat },
            workspace.is_some(),
        );
        // The model chosen in this AI's council box (#29), read fresh like the folder.
        let chosen_model = self.config.model(agent.id);
        // Keep project notes and shared memory compact for local models' smaller context windows.
        let local = agents::is_local(agent);
        let notes = workspace.as_deref().and_then(|w| memory::project_notes(std::path::Path::new(w), local));
        let shared = if local { self.memory.for_prompt_within(LOCAL_MEMORY_CHARS) } else { self.memory.for_prompt() };
        let members = self.config.get().members;
        let ctx = prompt::Context {
            members: &members,
            workspace: workspace.as_deref(),
            access,
            memory: shared.as_deref(),
            project_notes: notes.as_deref(),
        };
        let mut system = prompt::system(agent.id, answer, rounds, &ctx);
        // Local models and other chat-API AIs have no tools: they code through command boxes.
        if agents::is_api(agents::effective_kind(agent)) {
            if let Some(w) = workspace.as_deref() {
                system.push_str(&prompt::command_box_coding(w, std::env::consts::OS, crate::auto_run::is_on(self.store.dir(), conv)));
            }
        }
        // The skill library: local AIs see its index and pull runbooks; flagship AIs write them.
        let library = crate::skills::ensure_library(&self.data);
        if agents::is_local(agent) {
            system.push_str(&crate::skills::for_local(&library, &crate::skills::index(&library), std::env::consts::OS));
        } else {
            let locals: Vec<String> = members
                .iter()
                .filter_map(|m| agents::by_id(m))
                .filter(|a| agents::is_local(a))
                .map(|a| a.name.to_string())
                .collect();
            if !locals.is_empty() {
                system.push_str(&crate::skills::for_flagship(&library, &locals, crate::skills::index(&library).len()));
            }
        }
        // Everyone's plan usage and reset times, as last read (not for local models: no plan).
        if let Some(usage) = (!local)
            .then(|| crate::usage::for_prompt(&crate::usage::load(&self.data), &members, agent.id, store::now_ms()))
            .flatten()
        {
            system.push_str(&usage);
        }
        // The conversation's shared task board, and how to change it.
        system.push_str(&crate::tasks::for_prompt(
            &crate::tasks::load(self.store.dir(), conv),
            agent.id,
            local,
        ));
        // The projects' GitHub checks, as last read by the background watcher.
        if let Some(w) = workspace.as_deref().filter(|_| !local) {
            system.push_str(&crate::ci::for_prompt(std::path::Path::new(w)));
        }
        let handoff = self.store.handoff_path(conv);
        let mut elevated_commands = prompt::elevated_command_grants(&events, agent.id);
        // The Tools page's Install button: one pinned catalog install for the AI it was sent to.
        if let Some(command) = prompt::catalog_install_grant(&events, agent.id, &self.data) {
            elevated_commands.push(format!("{}{command}", prompt::INSTALL_GRANT_PREFIX));
        }
        // "Always allow" approvals saved on this computer apply to every AI, every turn.
        let saved_grants = self.config.get().saved_grants;
        if access == Access::Unrestricted && self.config.get().auto_run_commands {
            system.push_str(
                "\n\nAlways run is on: command boxes you post run on this computer by themselves and the output comes back to you, \
so the human isn't asked to click Run. Prefer running commands yourself; admin and install commands still wait for the human. \
So does any box that deletes or overwrites files, rewrites git history, uninstalls software, stops programs or runs a \
downloaded script: it waits for the human's click, with a note saying why. Put such a step in a box of its own, after \
checking what it would remove (list the folder first), so everything else still runs by itself.",
            );
        }
        if access == Access::Unrestricted || access == Access::Own {
            // The list is long; a local model can still use the grants, it just isn't shown them.
            if !local {
                system.push_str(&prompt::saved_grants_note(&saved_grants));
            }
            for grant in saved_grants {
                if !elevated_commands.contains(&grant) {
                    elevated_commands.push(grant);
                }
            }
        }
        let files = crate::attach::files_for(&events, &self.store.files_dir(conv));
        let mut ws_path = workspace.map(PathBuf::from);
        // Full capacity with isolated copies on: the AI works in its own worktree of each repo.
        // A copy that can't be prepared fails safe: the AI is held to Edit access in the real
        // folder rather than running unrestricted where isolation was asked for.
        let mut access = access;
        if access == Access::Unrestricted && self.config.get().isolate_agents {
            if let Some(ws) = ws_path.clone() {
                match crate::worktree::prepare(&self.data, &ws, agent.id) {
                    Ok(Some(iso)) => {
                        system.push_str("\n\n");
                        system.push_str(&iso.note);
                        ws_path = Some(iso.dir);
                    }
                    Ok(None) => {}
                    Err(e) => {
                        access = Access::Edit;
                        system.push_str(&format!(
                            "\n\nYour isolated copy couldn't be prepared ({e}), so this turn has Edit access in the project folder instead of Full capacity."
                        ));
                    }
                }
            }
        }
        let state_path = (access != Access::Chat).then(|| ws_path.clone()).flatten();
        let data = self.data.clone();
        let ui = self.ui.clone();
        let conversation = self.conversation.clone();
        let id = agent.id.to_string();
        let local_process = !agents::is_api(agents::effective_kind(agent));
        let initial_slot = local_process.then(|| try_agent_slot(&self.agent_slot_dir, self.agent_slot_limit));
        let agent_slot_dir = self.agent_slot_dir.clone();
        let agent_slot_limit = self.agent_slot_limit;
        let started_at = store::now_ms();
        let waiting_for_capacity = matches!(initial_slot, Some(Ok(None)));
        ui.activity(&AgentActivity {
            conversation: conversation.clone(),
            agent: id.clone(),
            kind: "preparing".into(),
            label: if waiting_for_capacity { format!("Queuing {}", agent.name) } else { format!("Starting {}", agent.name) },
            detail: if waiting_for_capacity { "Waiting for PC capacity".into() } else { agent_tool_label(agent).into() },
            started_at,
            updated_at: started_at,
        });
        let activity: agents::ActivitySink = Arc::new(move |signal| {
            ui.activity(&AgentActivity {
                conversation: conversation.clone(),
                agent: id.clone(),
                kind: signal.kind,
                label: signal.label,
                detail: signal.detail,
                started_at,
                updated_at: store::now_ms(),
            });
        });
        let title = self.store.meta(conv).ok().map(|m| m.title).unwrap_or_default();
        let label = if title.trim().is_empty() { agent.name.to_string() } else { format!("{} in {}", agent.name, title.trim()) };
        Box::pin(async move {
            // Linked computers see this answer in progress until it ends, however it ends.
            let _working = crate::link::working(&label);
            // Waiting is cancellation-aware, so Stop never hangs behind the capacity gate.
            let permit = match initial_slot {
                Some(Ok(Some(slot))) => Some(slot),
                Some(Ok(None)) => wait_for_agent_slot(agent_slot_dir, agent_slot_limit, &cancel).await?,
                // A gate error fails forward rather than disabling the council.
                Some(Err(_)) | None => None,
            };
            // Git can briefly touch many files in a large repository, so keep it off the async
            // executor and bound the wait. A slow/non-git folder must never delay the AI turn.
            // Share one status read between the prompt and the linked-computer summary.
            let summary_path = ws_path.clone();
            if let Some(path) = state_path {
                if let Ok(Ok(snapshot)) = tokio::time::timeout(
                    Duration::from_secs(3),
                    tokio::task::spawn_blocking(move || prompt::workspace_snapshot(&path, !local)),
                )
                .await
                {
                    let (peer_summary, state) = snapshot.map_or((None, None), |snapshot| (Some(snapshot.peer_summary), snapshot.prompt));
                    crate::link::set_git_summary(peer_summary);
                    if let Some(state) = state {
                        system.push_str("\n\n");
                        system.push_str(&state);
                    }
                }
            } else if let Some(path) = ws_path.clone() {
                refresh_git_summary(path).await;
            }
            let local_window = match agents::provider_of(agent) {
                Some(provider) => tokio::select! {
                    window = crate::apiagent::local_window(provider) => window,
                    _ = cancel.cancelled() => return Err("stopped".into()),
                },
                None => None,
            };
            let transcript = if local {
                prompt::local_transcript(&events, agent.id, Some(&data), handoff.as_deref(), local_window, &system)
            } else {
                prompt::transcript(&events, agent.id)
            };
            let reply = agents::run_with_files_observed(
                agent,
                access,
                ws_path.as_deref(),
                &system,
                &transcript,
                &files,
                &cancel,
                Some(activity),
                elevated_commands,
                chosen_model.as_deref(),
                local_window,
            )
            .await;
            drop(permit);
            if let Some(path) = summary_path {
                refresh_git_summary(path).await;
            }
            if let Some(model) = reply.as_ref().ok().and_then(|r| r.model.as_deref()) {
                record_model(&data, agent.id, model);
            }
            reply
        })
    }

    fn event(&self, e: &Event) {
        if e.kind == "message" && e.sender != "human" {
            self.journal.progressed(&self.conversation, &self.turn);
        }
        self.ui.event(e);
    }

    fn thinking(&self, agents: Vec<String>, round: u32) {
        self.ui.turn(TurnState {
            conversation: self.conversation.clone(),
            state: "running".into(),
            agent: agents.first().cloned(),
            agents,
            round,
            rounds: self.rounds,
        });
    }

    fn installed(&self, agent: &'static Agent) -> bool {
        agents::installed(agent)
    }

    fn remember(&self, agent: &'static Agent, reply: &str) -> Option<String> {
        let facts = memory::remember_lines(reply);
        if facts.is_empty() {
            return None;
        }
        let added = self.memory.remember(agent.name, &today(), &facts).ok()?;
        (!added.is_empty()).then(|| format!("{} saved to council memory: {}", agent.name, added.join("; ")))
    }

    fn tasks(&self, agent: &'static Agent, reply: &str) -> Option<String> {
        let ops = crate::tasks::task_lines(reply);
        // In a copy whose board the owning computer keeps, the answer goes there and counts there.
        let mut notes: Vec<String> = if crate::peer_sync::board_owner(&self.conversation).is_some() {
            vec![]
        } else {
            crate::tasks::apply(self.store.dir(), &self.conversation, agent.id, &ops).ok().flatten().into_iter().collect()
        };
        if !notes.is_empty() {
            crate::peer_sync::share_board(&self.conversation);
        }
        if let Some(title) = crate::tasks::title_line(reply) {
            let before = self.store.meta(&self.conversation).map(|m| m.title).unwrap_or_default();
            if let Ok(meta) = self.store.rename(&self.conversation, &title) {
                if meta.title != before {
                    notes.push(format!("{} renamed the conversation to \u{201c}{}\u{201d}.", agent.name, meta.title));
                }
            }
        }
        (!notes.is_empty()).then(|| notes.join("\n"))
    }
}

fn agent_tool_label(agent: &Agent) -> &'static str {
    match agents::effective_kind(agent) {
        agents::Kind::Codex => "Codex CLI",
        agents::Kind::ClaudeCode => "Claude Code CLI",
        agents::Kind::GeminiCli => "Gemini CLI",
        agents::Kind::Antigravity => "Antigravity CLI",
        agents::Kind::XaiApi => "xAI API",
        agents::Kind::DeepSeekApi => "DeepSeek API",
        agents::Kind::CustomApi => "API",
    }
}

fn read_models(data: &std::path::Path) -> HashMap<String, String> {
    std::fs::read_to_string(data.join("models.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

/// Remembers the model an AI just answered with (in `models.json`, shared with the other process),
/// so its usage meter can name the model when its CLI's limits don't.
fn record_model(data: &std::path::Path, agent: &str, model: &str) {
    let mut models = read_models(data);
    if models.get(agent).map(String::as_str) == Some(model) {
        return;
    }
    models.insert(agent.to_string(), model.to_string());
    let tmp = data.join("models.json.tmp");
    if let Ok(bytes) = serde_json::to_vec_pretty(&models) {
        if std::fs::write(&tmp, bytes).is_ok() {
            let _ = store::replace_file(&tmp, &data.join("models.json"));
        }
    }
}

/// Today's date as YYYY-MM-DD (UTC), for memory notes.
pub fn today() -> String {
    let days = store::now_ms() / 86_400_000;
    // Civil-from-days (Howard Hinnant), valid for all dates after 1970.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Self-check result for one AI.
#[derive(Clone, Serialize)]
pub struct CheckResult {
    pub id: String,
    pub name: String,
    pub ok: bool,
    /// How long the answer took, in milliseconds.
    pub ms: u64,
    /// The reply (trimmed) or the error.
    pub detail: String,
}

/// Asks every council member, at the same time, for a one-word reply, with chat-only access in an
/// empty folder. Each check costs about one short message of that AI's allowance.
pub async fn self_check(engine: &Engine) -> Vec<CheckResult> {
    let mut tasks = tokio::task::JoinSet::new();
    for id in engine.config.get().members {
        let Some(agent) = agents::by_id(&id) else { continue };
        let bench = engine.bench.clone();
        tasks.spawn(async move {
            let system = "This is an automatic health check from Consilium, not a conversation. Don't use tools \
or run commands. Reply with exactly the word OK and nothing else.";
            let started = std::time::Instant::now();
            let token = CancellationToken::new();
            let r = tokio::time::timeout(
                Duration::from_secs(180),
                agents::run(agent, Access::Chat, None, system, "Health check: reply with OK.", &token),
            )
            .await;
            let ms = started.elapsed().as_millis() as u64;
            let (ok, detail) = match r {
                Ok(Ok(reply)) => {
                    bench.clear(agent.id);
                    (true, reply.chars().take(80).collect())
                }
                Ok(Err(e)) => {
                    let now = store::now_ms();
                    if let Some(until) = bench::usage_limit(&e, now) {
                        bench.bench(agent.id, until, bench::reset_reported(&e, now));
                    }
                    (false, e)
                }
                Err(_) => {
                    token.cancel();
                    (false, "no answer within 3 minutes".into())
                }
            };
            CheckResult { id: agent.id.into(), name: agent.name.into(), ok, ms, detail }
        });
    }
    let mut out = Vec::new();
    while let Some(r) = tasks.join_next().await {
        if let Ok(r) = r {
            out.push(r);
        }
    }
    // In council order, whatever order the checks finished in.
    let members = engine.config.get().members;
    out.sort_by_key(|r| members.iter().position(|m| *m == r.id).unwrap_or(usize::MAX));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentReply;

    #[test]
    fn local_ai_slots_are_shared_and_released_on_drop() {
        let dir = std::env::temp_dir().join(format!("consilium-agent-slots-{}", std::process::id()));
        let first = try_agent_slot(&dir, 1).unwrap().expect("first process gets the slot");
        assert!(try_agent_slot(&dir, 1).unwrap().is_none(), "a second process must wait");
        drop(first);
        assert!(try_agent_slot(&dir, 1).unwrap().is_some(), "the OS releases a dropped slot");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn today_is_a_plausible_iso_date() {
        let t = today();
        assert_eq!(t.len(), 10);
        assert!(t.starts_with("20") && t.as_bytes()[4] == b'-' && t.as_bytes()[7] == b'-');
    }

    struct Quiet;
    impl Ui for Quiet {
        fn event(&self, _e: &Event) {}
        fn turn(&self, _t: TurnState) {}
        fn activity(&self, _activity: &AgentActivity) {}
    }

    /// AIs that answer "<name> heard: <the human's newest message>" after `delay` ms.
    struct Echo {
        delay: u64,
    }
    impl council::Host for Echo {
        fn ask(&self, agent: &'static Agent, _answer: u32, _rounds: u32, events: Vec<Event>, cancel: CancellationToken) -> council::Reply {
            let heard = events.iter().rev().find(|e| e.sender == "human" && e.kind == "message").map(|e| e.text.clone());
            let text = format!("{} heard: {}", agent.name, heard.unwrap_or_default());
            let delay = self.delay;
            Box::pin(async move {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(delay)) => Ok(AgentReply { text, usage: None, model: None }),
                    _ = cancel.cancelled() => Err("stopped".into()),
                }
            })
        }
        fn event(&self, _e: &Event) {}
        fn thinking(&self, _agents: Vec<String>, _round: u32) {}
        fn installed(&self, _agent: &'static Agent) -> bool {
            true
        }
        fn remember(&self, _agent: &'static Agent, _reply: &str) -> Option<String> {
            None
        }
    }

    /// Two engines on one data folder, standing for the app and the terminal command.
    fn pair(delay: u64) -> (Arc<Engine>, Arc<Engine>, PathBuf) {
        let dir = std::env::temp_dir().join(format!("consilium-engine-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("council.json"), r#"{"members":["gpt","claude"]}"#).unwrap();
        let make = || {
            let e = Engine::open(&dir, Arc::new(Quiet), Box::new(|t| {
                tokio::spawn(t);
            }))
            .unwrap()
            .with_host(Box::new(move |_, _| Arc::new(Echo { delay })));
            let e = Arc::new(e);
            tokio::spawn(e.clone().watch(true));
            e
        };
        (make(), make(), dir)
    }

    async fn until_idle(engines: &[&Arc<Engine>], conv: &str) {
        let started = std::time::Instant::now();
        while engines.iter().any(|e| e.busy(conv)) {
            assert!(started.elapsed() < Duration::from_secs(10), "the turn never ended");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn said(engine: &Engine, conv: &str) -> Vec<String> {
        engine.store.load(conv).unwrap().into_iter().filter(|e| e.kind == "message").map(|e| format!("{}: {}", e.sender, e.text)).collect()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_message_from_the_terminal_joins_the_turn_the_app_is_running() {
        let (app, term, dir) = pair(900);
        let conv = app.store.create("x").unwrap().id;
        app.send(&conv, "first", vec!["gpt".into()], 1).unwrap();
        assert!(app.running_here(&conv));
        tokio::time::sleep(Duration::from_millis(100)).await;
        // The terminal can't run this conversation's AIs while the app does; its message is
        // recorded and the app's turn takes it in, with Claude, whom it addresses.
        term.send(&conv, "second", vec!["claude".into()], 1).unwrap();
        assert!(!term.running_here(&conv));
        until_idle(&[&app, &term], &conv).await;
        let all = said(&app, &conv);
        assert!(all.contains(&"claude: Claude heard: second".to_string()), "{all:?}");
        assert!(all.contains(&"gpt: ChatGPT heard: second".to_string()), "{all:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_terminal_runs_a_turn_when_the_app_is_idle_and_either_can_stop_it() {
        let (app, term, dir) = pair(5_000);
        let conv = term.store.create("x").unwrap().id;
        term.send(&conv, "hello", vec!["gpt".into(), "claude".into()], 1).unwrap();
        assert!(term.running_here(&conv));
        assert!(app.busy(&conv) && !app.running_here(&conv));
        assert!(app.running_ids().contains(&conv));
        // Stop in the app reaches the terminal's turn.
        assert!(app.stop(&conv));
        until_idle(&[&app, &term], &conv).await;
        assert!(said(&term, &conv).iter().all(|m| m.starts_with("human")), "nobody answered after Stop");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_turn_interrupted_in_one_process_is_not_resumed_while_the_other_runs_it() {
        let (app, term, dir) = pair(1_500);
        let conv = term.store.create("x").unwrap().id;
        term.send(&conv, "hello", vec!["gpt".into()], 1).unwrap();
        app.resume_interrupted(None);
        assert!(!app.running_here(&conv), "the terminal's turn is left to the terminal");
        until_idle(&[&app, &term], &conv).await;
        assert_eq!(said(&app, &conv).len(), 2, "one answer, not two");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_resumed_turn_skips_an_ai_switched_off_after_it_was_recorded() {
        let (app, _term, dir) = pair(0);
        let conv = app.store.create("x").unwrap().id;
        app.store.append(&conv, "human", "message", vec!["gpt".into(), "claude".into()], "hello").unwrap();
        // The turn was recorded with both, then Claude was switched off before the restart.
        app.journal.begin(Pending::new(&conv, vec!["gpt".into(), "claude".into()], 1));
        crate::switched::set(app.store.dir(), &conv, "claude", false).unwrap();
        app.resume_interrupted(None);
        until_idle(&[&app], &conv).await;
        let said = said(&app, &conv);
        assert!(said.iter().any(|m| m.starts_with("gpt:")), "{said:?}");
        assert!(!said.iter().any(|m| m.starts_with("claude:")), "a switched-off AI answered: {said:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A window that counts the events it was shown while the turn table was locked (#14).
    #[derive(Default)]
    struct LockCheck {
        engine: std::sync::OnceLock<std::sync::Weak<Engine>>,
        locked: std::sync::atomic::AtomicUsize,
    }
    impl Ui for Arc<LockCheck> {
        fn event(&self, _e: &Event) {
            if let Some(e) = self.engine.get().and_then(|w| w.upgrade()) {
                if !e.turns_lock_free() {
                    self.locked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
            }
        }
        fn turn(&self, _t: TurnState) {}
        fn activity(&self, _activity: &AgentActivity) {}
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_turn_table_is_free_while_the_window_hears_of_a_message_and_concurrent_sends_both_get_answered() {
        let dir = std::env::temp_dir().join(format!("consilium-engine-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("council.json"), r#"{"members":["gpt","claude"]}"#).unwrap();
        let check = Arc::new(LockCheck::default());
        let ui: Arc<dyn Ui> = Arc::new(check.clone());
        let e = Arc::new(
            Engine::open(&dir, ui, Box::new(|t| {
                tokio::spawn(t);
            }))
            .unwrap()
            .with_host(Box::new(|_, _| Arc::new(Echo { delay: 400 }))),
        );
        check.engine.set(Arc::downgrade(&e)).unwrap();
        let conv = e.store.create("x").unwrap().id;
        // Claude is switched off here, so the "wasn't asked" note is shown too.
        crate::switched::set(e.store.dir(), &conv, "claude", false).unwrap();
        e.send(&conv, "first", vec!["gpt".into(), "claude".into()], 1).unwrap();
        assert!(e.running_here(&conv));
        // A linked computer's human message joins the running turn.
        let linked = e.store.append(&conv, "human", "message", vec![], "second").unwrap();
        e.from_linked(&conv, &linked, &|| true);
        assert_eq!(check.locked.load(std::sync::atomic::Ordering::SeqCst), 0, "the window was told while the turn table was locked");
        until_idle(&[&e], &conv).await;
        assert!(said(&e, &conv).contains(&"gpt: ChatGPT heard: second".to_string()), "{:?}", said(&e, &conv));
        // Two messages at the same moment: one starts a turn, the other joins it (or the next one).
        let (a, b) = (e.clone(), e.clone());
        let (c1, c2) = (conv.clone(), conv.clone());
        let t1 = tokio::task::spawn_blocking(move || a.send(&c1, "third", vec!["gpt".into()], 1).unwrap());
        let t2 = tokio::task::spawn_blocking(move || b.send(&c2, "fourth", vec!["gpt".into()], 1).unwrap());
        let newest = t1.await.unwrap().seq.max(t2.await.unwrap().seq);
        until_idle(&[&e], &conv).await;
        let events = e.store.load(&conv).unwrap();
        let last_human = events.iter().find(|m| m.seq == newest).unwrap().text.clone();
        let answered = events.iter().any(|m| m.seq > newest && m.text == format!("ChatGPT heard: {last_human}"));
        assert!(answered, "{:?}", said(&e, &conv));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn tier_targets_prefers_flagship_and_falls_back_to_local_when_exhausted() {
        let candidates = ["gpt", "claude", "qwen"].map(str::to_string);
        let is_local = |id: &str| id == "qwen";
        assert_eq!(select_tier_targets(&candidates, is_local, |_| true), ["gpt", "claude"]);
        assert_eq!(select_tier_targets(&candidates, is_local, |id| id != "gpt"), ["claude"]);
        assert_eq!(select_tier_targets(&candidates, is_local, |id| id == "qwen"), ["qwen"]);
        assert_eq!(select_tier_targets(&candidates, is_local, |_| false), candidates);
    }
}

fn select_tier_targets(
    candidates: &[String],
    is_local: impl Fn(&str) -> bool,
    available: impl Fn(&str) -> bool,
) -> Vec<String> {
    let (local, flagship): (Vec<_>, Vec<_>) = candidates.iter().partition(|id| is_local(id));
    for tier in [flagship, local] {
        let ready: Vec<_> = tier.into_iter().filter(|id| available(id)).cloned().collect();
        if !ready.is_empty() {
            return ready;
        }
    }
    candidates.to_vec()
}

/// Who a message goes to: the council members it @mentions if it names any, else the ticked ones.
/// Without this, "@claude ..." also started every other ticked AI.
fn route(ticked: Vec<String>, mentioned: Vec<String>, is_member: impl Fn(&str) -> bool) -> Vec<String> {
    let named: Vec<String> = mentioned.into_iter().filter(|m| is_member(m)).collect();
    if !named.is_empty() {
        return named;
    }
    ticked.into_iter().filter(|t| is_member(t)).collect()
}

#[cfg(test)]
mod routing_tests {
    use super::route;

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }
    fn member(a: &str) -> bool {
        matches!(a, "claude" | "gemini" | "gpt")
    }

    #[test]
    fn a_mention_starts_only_the_mentioned_ai() {
        assert_eq!(route(ids(&["claude", "gemini"]), ids(&["claude"]), member), ids(&["claude"]));
    }

    #[test]
    fn no_mention_goes_to_the_ticked_ais() {
        assert_eq!(route(ids(&["claude", "gemini"]), vec![], member), ids(&["claude", "gemini"]));
    }

    #[test]
    fn mentions_of_non_members_are_ignored() {
        assert_eq!(route(ids(&["claude", "frank"]), ids(&["grok"]), member), ids(&["claude"]));
    }

    #[test]
    fn all_reaches_every_member() {
        assert_eq!(route(ids(&["claude"]), ids(&["claude", "gemini", "gpt"]), member), ids(&["claude", "gemini", "gpt"]));
    }
}
