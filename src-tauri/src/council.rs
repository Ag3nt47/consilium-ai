//! One turn of the council: every addressed AI answers as soon as it's ready, with no turn order.
//!
//! Each AI runs in its own task. It answers the human first, then, whenever anyone else posts
//! something new, it may answer again, up to the turn's `rounds` answers each, so nobody waits for
//! anybody else to finish talking. A turn ends when no AI has anything new to react to, when an AI
//! asks the human something (REQUEST:), or on Stop.
//!
//! An AI that fails, or runs out of usage, doesn't end the conversation: one that is out of usage is
//! benched until its limit resets (see `bench`), the others carry on, and if nobody is left answering,
//! another council member takes its place.

use crate::agents::{self, Agent, AgentReply};
use crate::bench::{self, Bench};
use crate::prompt;
use crate::store::{now_ms, Event, Store};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

/// An answer that @mentions another council member gives it one more answer in this turn, at most
/// this many times per round of the turn, so AIs can ask each other things without looping.
pub const MAX_PULLED_IN: usize = 2;

/// Sender of notes from Consilium itself that the AIs should see (shown to them as [Consilium]).
pub const CONSILIUM: &str = "consilium";

/// An AI's answer, with the token counts its provider reported for it, or why it couldn't answer.
pub type Reply = Pin<Box<dyn Future<Output = Result<AgentReply, String>> + Send>>;

/// What a turn needs from the app; tests supply fake AIs.
pub trait Host: Send + Sync + 'static {
    fn resolve_agent(&self, id: &str) -> Option<&'static Agent> {
        agents::by_id(id)
    }

    /// Asks `agent` for its `answer`-th message of `rounds` in this turn, given the conversation.
    fn ask(&self, agent: &'static Agent, answer: u32, rounds: u32, events: Vec<Event>, cancel: CancellationToken) -> Reply;
    /// A new event was recorded.
    fn event(&self, e: &Event);
    /// Which AIs are working on an answer now, and the highest answer number among them.
    fn thinking(&self, agents: Vec<String>, round: u32);
    /// Whether this AI is installed, so it can stand in for one that can't answer.
    fn installed(&self, agent: &'static Agent) -> bool;
    /// Saves an answer's REMEMBER: lines to council memory; returns the note to show, if any.
    fn remember(&self, agent: &'static Agent, reply: &str) -> Option<String>;
    /// Applies an answer's TASK: lines to the conversation's task board; returns the note to show.
    fn tasks(&self, _agent: &'static Agent, _reply: &str) -> Option<String> {
        None
    }
    /// Persists granted answer slots before a worker can consume them.
    fn extra_answers(&self, _extra: &HashMap<String, u32>, _pulled_in: usize) {}
}

pub struct Turn {
    pub conversation: String,
    /// The AIs the human addressed.
    pub targets: Vec<String>,
    pub rounds: u32,
    /// Council members, in council order (stand-ins are chosen in this order).
    pub members: Vec<String>,
}

#[derive(Default)]
struct State {
    /// AIs taking part in this turn right now (answering or waiting for something new).
    live: HashSet<String>,
    /// Of those, the ones waiting for something new to react to.
    waiting: HashSet<String>,
    /// AIs working on an answer, with its number and the human-message generation it answers.
    thinking: Vec<(String, u32, u64)>,
    /// Every AI that has taken part in this turn, so a stand-in is someone new.
    joined: HashSet<String>,
    answered: HashMap<String, u32>,
    /// Extra answers granted by @mentions.
    extra: HashMap<String, u32>,
    pulled_in: usize,
    passed: HashSet<String>,
    /// Sequence number of the newest message from each sender.
    latest: HashMap<String, u64>,
    /// An AI asked the human something: nobody starts a new answer until they reply.
    paused: bool,
    /// Raised each time the human sends a message into the running turn, so an AI that was about
    /// to leave stays to read it.
    generation: u64,
    /// The turn has ended; a new message needs a new turn.
    closed: bool,
    /// An AI's failure has already alerted the council in this turn, so a failure while the
    /// others work on it doesn't raise another alarm.
    alarmed: bool,
    /// Each AI's own stop switch for this turn, under the turn's: its council-box Stop ends its
    /// answer in progress (and the program or request behind it) without stopping the others.
    own_cancel: HashMap<String, CancellationToken>,
    /// AIs stopped in their council box during this turn: they don't answer again in it.
    stopped: HashSet<String>,
}

/// Each conversation's AIs whose failure has alerted the council, with the error: the same AI
/// failing the same way again (a dead endpoint, turn after turn) raises no new alarm until it has
/// answered once.
static ALARMED: std::sync::LazyLock<Mutex<HashMap<(String, String), String>>> = std::sync::LazyLock::new(Default::default);

fn answered_again(conversation: &str, id: &str) {
    ALARMED.lock().unwrap().remove(&(conversation.to_string(), id.to_string()));
}

/// An AI couldn't answer because of a real error (not usage): tells every other AI that can
/// answer, and brings them into the turn to work out why. At most one alarm a turn, and one per AI
/// and error until that AI answers again, so a failing alarm round can't start another.
fn alarm(s: &Arc<Shared>, failed: &'static Agent, why: &str) {
    let key = (s.turn.conversation.clone(), failed.id.to_string());
    let now = now_ms();
    let mut st = s.state.lock().unwrap();
    if st.alarmed || st.paused || s.cancel.is_cancelled() || ALARMED.lock().unwrap().get(&key).is_some_and(|w| w == why) {
        return;
    }
    if crate::switched::is_off(s.store.dir(), &s.turn.conversation, failed.id) {
        return;
    }
    let helpers: Vec<&'static Agent> = s
        .turn
        .members
        .iter()
        .filter(|m| **m != failed.id)
        .filter_map(|m| agents::by_id(m))
        .filter(|a| s.bench.until(a.id, now).is_none() && s.host.installed(a))
        .filter(|a| !crate::switched::is_off(s.store.dir(), &s.turn.conversation, a.id))
        .collect();
    if helpers.is_empty() {
        return;
    }
    st.alarmed = true;
    ALARMED.lock().unwrap().insert(key, why.to_string());
    let to: Vec<String> = helpers.iter().map(|a| a.id.to_string()).collect();
    let names = to.iter().map(|id| format!("@{id}")).collect::<Vec<_>>().join(" ");
    let text = format!(
        "{names}: {} couldn't answer: {why}\nPlease work out why together and fix it: one of you takes it with a TASK: line, the others review.",
        failed.name
    );
    let Ok(e) = s.store.append(&s.turn.conversation, CONSILIUM, "system", to.clone(), &text) else {
        return;
    };
    s.host.event(&e);
    // Waiting AIs wake for it; the rest get one more answer and join in.
    st.latest.insert(CONSILIUM.to_string(), e.seq);
    let mut start = Vec::new();
    for id in to {
        *st.extra.entry(id.clone()).or_default() += 1;
        st.passed.remove(&id);
        if st.live.insert(id.clone()) {
            st.joined.insert(id.clone());
            start.push(id);
        }
    }
    s.host.extra_answers(&st.extra, st.pulled_in);
    drop(st);
    for id in start {
        tokio::spawn(worker(s.clone(), id, Some(e.seq - 1)));
    }
    s.wake.notify_waiters();
}

struct Shared {
    turn: Turn,
    store: Arc<Store>,
    bench: Arc<Bench>,
    host: Arc<dyn Host>,
    cancel: CancellationToken,
    state: Mutex<State>,
    wake: Notify,
    #[cfg(test)]
    before_worker_exit: Mutex<Option<WorkerExitGate>>,
}

#[cfg(test)]
#[derive(Clone)]
struct WorkerExitGate {
    agent: String,
    reached: Arc<Notify>,
    release: Arc<Notify>,
}

/// Where a turn has got to, read from the conversation itself: what each AI has said since the
/// human's last message. A turn resumed after a restart picks up from here.
#[derive(Debug, Default, PartialEq)]
pub struct Progress {
    pub answered: HashMap<String, u32>,
    pub passed: HashSet<String>,
    /// Each AI's newest message since the human's, by sequence number.
    pub last: HashMap<String, u64>,
}

/// Whether `e` asks the council for a fresh round: the human's message, or a Consilium note
/// addressed to the council (auto-continue, failed GitHub checks). Answers before it don't count.
fn starts_turn(e: &Event) -> bool {
    (e.sender == "human" && e.kind == "message") || (e.sender == CONSILIUM && e.kind == "system" && !e.to.is_empty())
}

pub fn progress(events: &[Event]) -> Progress {
    progress_after(events, None)
}

fn progress_after(events: &[Event], after: Option<u64>) -> Progress {
    let start = events.iter().rposition(starts_turn).map_or(0, |i| i + 1);
    let mut p = Progress::default();
    for e in events[start..].iter().filter(|e| after.is_none_or(|seq| e.seq > seq)) {
        // A namesake on another computer never uses this computer's answers or passes.
        if e.sender == "human" || e.sender == CONSILIUM || e.via.is_some() {
            continue;
        }
        if e.kind == "message" {
            *p.answered.entry(e.sender.clone()).or_default() += 1;
            p.last.insert(e.sender.clone(), e.seq);
        } else if e.kind == "system" && e.text.ends_with("had nothing to add.") {
            *p.answered.entry(e.sender.clone()).or_default() += 1;
            p.passed.insert(e.sender.clone());
        }
    }
    p
}

/// Runs the turn to its end (the app uses `Council` directly, to take messages while it runs).
#[cfg(test)]
pub async fn run(turn: Turn, store: Arc<Store>, bench: Arc<Bench>, host: Arc<dyn Host>, cancel: CancellationToken) {
    Council::new(turn, store, bench, host, cancel).run().await
}

/// A turn in progress. The human can keep talking while it runs (see `join`).
pub struct Council {
    shared: Arc<Shared>,
    /// The AIs to start when the turn runs, with the newest message each has already answered.
    first: Mutex<Vec<(String, Option<u64>)>>,
}

impl Council {
    pub fn new(turn: Turn, store: Arc<Store>, bench: Arc<Bench>, host: Arc<dyn Host>, cancel: CancellationToken) -> Self {
        Self::new_after(turn, store, bench, host, cancel, None)
    }

    /// Starts or resumes a turn whose answer accounting begins after `after`. The complete
    /// conversation remains visible; an earlier turn's answers don't consume this turn's budget.
    pub fn new_after(
        turn: Turn,
        store: Arc<Store>,
        bench: Arc<Bench>,
        host: Arc<dyn Host>,
        cancel: CancellationToken,
        after: Option<u64>,
    ) -> Self {
        Self::new_with_allowance(turn, store, bench, host, cancel, after, (HashMap::new(), 0))
    }

    /// Restores the durable answer grants and the turn's use of its mention cap.
    pub fn new_with_allowance(
        turn: Turn,
        store: Arc<Store>,
        bench: Arc<Bench>,
        host: Arc<dyn Host>,
        cancel: CancellationToken,
        after: Option<u64>,
        allowance: (HashMap<String, u32>, usize),
    ) -> Self {
        let events = store.load(&turn.conversation).unwrap_or_default();
        let done = match after {
            Some(_) => progress_after(&events, after),
            None => progress(&events),
        };
        let mut state = State::default();
        for e in events.iter().filter(|e| e.kind == "message" || starts_turn(e)) {
            let who = match &e.via {
                Some(via) => format!("{}@{via}", e.sender),
                None => e.sender.clone(),
            };
            state.latest.insert(who, e.seq);
        }
        state.answered = done.answered.clone();
        state.passed = done.passed.clone();
        (state.extra, state.pulled_in) = allowance;
        // Everyone addressed, plus anyone who joined in before a restart cut the turn short.
        let mut who: Vec<(String, Option<u64>)> = turn.targets.iter().map(|t| (t.clone(), done.last.get(t).copied())).collect();
        for (a, seq) in &done.last {
            if !turn.targets.contains(a) && turn.members.contains(a) {
                who.push((a.clone(), Some(*seq)));
            }
        }
        for a in state.extra.keys() {
            if turn.members.contains(a) && !who.iter().any(|(id, _)| id == a) {
                who.push((a.clone(), done.last.get(a).copied()));
            }
        }
        for (a, seen) in &mut who {
            if state.extra.get(a).is_some_and(|extra| done.answered.get(a).copied().unwrap_or(0) < turn.rounds + *extra) {
                // A peer can ask during an answer that is persisted after the request.
                // The durable unused grant still needs an answer despite that newer reply.
                *seen = None;
            }
        }
        // An AI switched off in this chat isn't asked, even when a turn recorded before it was
        // switched off resumes after a restart.
        who.retain(|(a, _)| {
            !done.passed.contains(a)
                && agents::by_id(a).is_some()
                && !crate::switched::is_off(store.dir(), &turn.conversation, a)
        });
        for (a, _) in &who {
            state.live.insert(a.clone());
            state.joined.insert(a.clone());
        }
        let shared = Arc::new(Shared {
            turn, store, bench, host, cancel, state: Mutex::new(state), wake: Notify::new(),
            #[cfg(test)]
            before_worker_exit: Mutex::new(None),
        });
        Council { shared, first: Mutex::new(who) }
    }

    /// The human said `message` while the turn is running. Nobody is interrupted: every AI gets a
    /// fresh set of answers, those waiting read it at once, one in the middle of an answer reads it
    /// as soon as it has finished, and anyone in `targets` who isn't taking part joins in.
    /// Returns false if the turn has already ended, so the message needs a turn of its own.
    /// New workers use the process's injected executor, including from synchronous callers.
    pub fn join(&self, message: &Event, targets: &[String], spawn: &crate::engine::Spawn) -> bool {
        let s = &self.shared;
        let mut start = Vec::new();
        {
            let mut st = s.state.lock().unwrap();
            if st.closed || s.cancel.is_cancelled() {
                return false;
            }
            st.answered.clear();
            st.extra.clear();
            st.passed.clear();
            st.pulled_in = 0;
            s.host.extra_answers(&st.extra, st.pulled_in);
            st.paused = false;
            st.generation += 1;
            st.latest.insert(message.sender.clone(), message.seq);
            for t in targets {
                if agents::by_id(t).is_some()
                    && s.bench.until(t, now_ms()).is_none()
                    && !st.live.contains(t)
                    && !crate::switched::is_off(s.store.dir(), &s.turn.conversation, t)
                {
                    st.live.insert(t.clone());
                    st.joined.insert(t.clone());
                    start.push(t.clone());
                }
            }
        }
        for t in start {
            // Signal and other synchronous callers need not be inside an async runtime.
            spawn(Box::pin(worker(s.clone(), t, Some(message.seq.saturating_sub(1)))));
        }
        s.wake.notify_waiters();
        true
    }

    /// Another computer's AI said `message` in this conversation (see `peer_sync`). AIs waiting
    /// for something new read it and may answer, within the answers they have left; unlike `join`
    /// it gives no new answers and brings nobody in, so two computers can't keep each other going.
    /// Returns false if the turn has already ended.
    pub fn heard(&self, message: &Event) -> bool {
        let s = &self.shared;
        {
            let mut st = s.state.lock().unwrap();
            if st.closed || s.cancel.is_cancelled() {
                return false;
            }
            // Keyed apart from this computer's own AI of the same name, so "Claude (MacBook)"
            // counts as someone else to this computer's Claude.
            let who = format!("{}@{}", message.sender, message.via.as_deref().unwrap_or("linked"));
            st.latest.insert(who, message.seq);
        }
        s.wake.notify_waiters();
        true
    }

    /// Another computer's AI addressed `ids` (this computer's AIs) by handle in `message`. Like an
    /// @mention between AIs here: each gets one more answer and joins in if it isn't taking part,
    /// within the turn's limit on AIs brought in this way. Returns false if the turn has ended.
    pub fn mentioned(&self, message: &Event, ids: &[String]) -> bool {
        let s = &self.shared;
        let rounds = s.turn.rounds;
        let mut start = Vec::new();
        {
            let mut st = s.state.lock().unwrap();
            if st.closed || s.cancel.is_cancelled() {
                return false;
            }
            let who = format!("{}@{}", message.sender, message.via.as_deref().unwrap_or("linked"));
            st.latest.insert(who, message.seq);
            let limit = MAX_PULLED_IN * rounds as usize;
            let now = now_ms();
            let mut allowance_changed = false;
            for m in ids {
                if st.pulled_in >= limit {
                    break;
                }
                if agents::by_id(m).is_none()
                    || s.bench.until(m, now).is_some()
                    || crate::switched::is_off(s.store.dir(), &s.turn.conversation, m)
                {
                    continue;
                }
                let used = committed_answers(&st, m);
                let budget = rounds + st.extra.get(m).copied().unwrap_or(0);
                if st.passed.contains(m) || used >= budget || !st.live.contains(m) {
                    let extra = st.extra.entry(m.clone()).or_default();
                    if used >= budget {
                        *extra += 1;
                    }
                    st.passed.remove(m);
                    st.pulled_in += 1;
                    allowance_changed = true;
                    if !st.live.contains(m) {
                        st.live.insert(m.clone());
                        st.joined.insert(m.clone());
                        start.push(m.clone());
                    }
                }
            }
            if allowance_changed {
                s.host.extra_answers(&st.extra, st.pulled_in);
            }
        }
        for m in start {
            tokio::spawn(worker(s.clone(), m, Some(message.seq.saturating_sub(1))));
        }
        s.wake.notify_waiters();
        true
    }

    /// The human stopped `id` in its council box: ends its answer in progress, and the program or
    /// request behind it, and it takes no further part in this turn. The others carry on. Returns
    /// whether it was taking part.
    pub fn stop_agent(&self, id: &str) -> bool {
        let s = &self.shared;
        let mut st = s.state.lock().unwrap();
        if !st.live.contains(id) || st.closed {
            return false;
        }
        st.stopped.insert(id.to_string());
        st.own_cancel.entry(id.to_string()).or_insert_with(|| s.cancel.child_token()).cancel();
        drop(st);
        s.wake.notify_waiters();
        true
    }

    /// Runs the turn to its end.
    pub async fn run(&self) {
        let shared = self.shared.clone();
        let who = std::mem::take(&mut *self.first.lock().unwrap());
        for (a, last_seen) in who {
            tokio::spawn(worker(shared.clone(), a, last_seen));
        }
        // Workers can start others (stand-ins, @mentioned AIs, AIs the human adds); wait until
        // nobody is taking part.
        loop {
            let n = shared.wake.notified();
            tokio::pin!(n);
            n.as_mut().enable();
            {
                let mut st = shared.state.lock().unwrap();
                if st.live.is_empty() {
                    st.closed = true;
                    break;
                }
            }
            n.await;
        }
        let st = shared.state.lock().unwrap();
        if shared.turn.rounds > 1 && !shared.cancel.is_cancelled() {
            let note = if st.paused {
                Some("Discussion paused: a question for you is waiting.")
            } else if !st.passed.is_empty() {
                Some("Discussion ended: nobody had more to add.")
            } else {
                None
            };
            drop(st);
            if let Some(note) = note {
                note_as(&shared, "human", note);
            }
        }
    }
}

fn note_as(s: &Shared, sender: &str, text: &str) -> Option<Event> {
    let e = s.store.append(&s.turn.conversation, sender, "system", vec![], text).ok()?;
    s.host.event(&e);
    Some(e)
}

fn emit_thinking(s: &Shared, st: &State) {
    let agents: Vec<String> = st.thinking.iter().map(|(a, _, _)| a.clone()).collect();
    let round = st.thinking.iter().map(|(_, n, _)| *n).max().unwrap_or(0);
    s.host.thinking(agents, round);
}

/// An answer already in flight needs its slot too: a later mention must reserve a follow-up,
/// while several mentions before that follow-up starts can share it.
fn committed_answers(st: &State, id: &str) -> u32 {
    let answered = st.answered.get(id).copied().unwrap_or(0);
    st.thinking
        .iter()
        .filter(|(a, _, generation)| a == id && *generation == st.generation)
        .map(|(_, n, _)| *n)
        .fold(answered, u32::max)
}

fn finish_thinking(s: &Shared, st: &mut State, id: &str) {
    st.thinking.retain(|(a, _, _)| a != id);
    emit_thinking(s, st);
}

fn about(ms: u64) -> String {
    let mins = ms.div_ceil(60_000);
    match (mins / 60, mins % 60) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
    }
}

/// One AI's part in the turn. `last_seen` is set when it has already answered (after a restart),
/// so it waits for something new before answering again. (Boxed, because a worker can start others.)
fn worker(s: Arc<Shared>, id: String, last_seen: Option<u64>) -> Pin<Box<dyn Future<Output = ()> + Send>> {
    Box::pin(work(s, id, last_seen))
}

/// Answers until there is nothing new to react to; returns whether it couldn't answer (an error or
/// out of usage), and the newest message it has read.
async fn answer(s: Arc<Shared>, id: String, mut last_seen: Option<u64>) -> (bool, Option<u64>) {
    let agent = s.host.resolve_agent(&id).expect("known agent");
    if crate::switched::is_off(s.store.dir(), &s.turn.conversation, &id) {
        return (false, last_seen);
    }
    let rounds = s.turn.rounds;
    let mut failed = false;
    loop {
        {
            let st = s.state.lock().unwrap();
            if s.cancel.is_cancelled() || st.paused || st.stopped.contains(&id) {
                break;
            }
        }
        let now = now_ms();
        if let Some(until) = s.bench.until(&id, now) {
            note_as(
                &s,
                CONSILIUM,
                &format!(
                    "{} is out of usage (its limit resets in about {}), so it is sitting out; the council carries on without it.",
                    agent.name,
                    about(until - now)
                ),
            );
            failed = true;
            break;
        }
        // Wait for something new from someone else, unless this is the first answer.
        if let Some(seen) = last_seen {
            let go_on = loop {
                let n = s.wake.notified();
                tokio::pin!(n);
                n.as_mut().enable();
                {
                    let mut st = s.state.lock().unwrap();
                    let budget = rounds + st.extra.get(&id).copied().unwrap_or(0);
                    if st.paused || st.stopped.contains(&id) || st.answered.get(&id).copied().unwrap_or(0) >= budget {
                        break false;
                    }
                    if st.latest.iter().any(|(who, seq)| who != &id && *seq > seen) {
                        st.waiting.remove(&id);
                        break true;
                    }
                    st.waiting.insert(id.clone());
                    // Everyone is waiting, so nothing new will come.
                    if st.live.iter().all(|a| st.waiting.contains(a)) {
                        s.wake.notify_waiters();
                        break false;
                    }
                }
                tokio::select! {
                    _ = &mut n => {}
                    _ = s.cancel.cancelled() => break false,
                }
            };
            if !go_on {
                break;
            }
        }
        let (n, asked_in, mine) = {
            let mut st = s.state.lock().unwrap();
            let n = st.answered.get(&id).copied().unwrap_or(0) + 1;
            if n > rounds + st.extra.get(&id).copied().unwrap_or(0) {
                break;
            }
            let generation = st.generation;
            st.thinking.push((id.clone(), n, generation));
            emit_thinking(&s, &st);
            let mine = st.own_cancel.entry(id.clone()).or_insert_with(|| s.cancel.child_token()).clone();
            (n, st.generation, mine)
        };
        let events = s.store.load(&s.turn.conversation).unwrap_or_default();
        last_seen = Some(events.last().map_or(0, |e| e.seq));
        let already_seen: HashSet<&str> = events
            .iter()
            .filter(|event| event.kind == "seen" && event.sender == id)
            .flat_map(|event| event.seen.iter().map(String::as_str))
            .collect();
        let newly_seen: Vec<String> = prompt::visible_message_ids(&events, &id)
            .into_iter()
            .filter(|message| !already_seen.contains(message.as_str()))
            .collect();
        // Other computers' AIs whose messages this AI is reading now: once it answers, that is
        // its half of the handshake with each (see `link::Link::acknowledge`).
        let mut heard_from: Vec<(String, String)> = Vec::new();
        for e in events.iter().filter(|e| e.kind == "message" && e.sender != "human" && newly_seen.contains(&e.id)) {
            if let Some(via) = &e.via {
                let pair = (via.clone(), e.sender.clone());
                if !heard_from.contains(&pair) {
                    heard_from.push(pair);
                }
            }
        }
        // A receipt means this bounded transcript was handed to the AI. Persist it before waiting
        // for the answer, so a slow, stopped, or failed turn still reports what it received.
        if !newly_seen.is_empty() {
            if let Ok(receipt) = s.store.append_seen(&s.turn.conversation, &id, newly_seen) {
                s.host.event(&receipt);
            }
        }
        let result = s.host.ask(agent, n, rounds, events, mine).await;
        if result.is_ok() {
            if let Some(link) = crate::link_handle() {
                for (computer, their_ai) in &heard_from {
                    link.acknowledge(computer, their_ai, &id);
                }
            }
        }
        match result {
            Ok(reply) if rounds > 1 && prompt::is_pass(&reply.text) => {
                s.bench.clear(&id);
                answered_again(&s.turn.conversation, &id);
                // A pass still used tokens, so they are recorded on the note.
                let text = format!("{} had nothing to add.", agent.name);
                if let Ok(e) = s.store.append_with_usage(&s.turn.conversation, &id, "system", vec![], &text, reply.usage) {
                    s.host.event(&e);
                }
                let mut st = s.state.lock().unwrap();
                // A pass on what came before the human's newest message doesn't count.
                if st.generation == asked_in {
                    *st.answered.entry(id.clone()).or_default() += 1;
                    st.passed.insert(id.clone());
                }
                finish_thinking(&s, &mut st, &id);
                drop(st);
                break;
            }
            Ok(reply) => {
                s.bench.clear(&id);
                answered_again(&s.turn.conversation, &id);
                let to = prompt::mentioned_agents(&reply.text);
                let Ok(ev) = s.store.append_with_usage(&s.turn.conversation, &id, "message", to.clone(), &reply.text, reply.usage)
                else {
                    finish_thinking(&s, &mut s.state.lock().unwrap(), &id);
                    break;
                };
                s.host.event(&ev);
                if let Some(note) = s.host.remember(agent, &reply.text) {
                    note_as(&s, &id, &note);
                }
                if let Some(note) = s.host.tasks(agent, &reply.text) {
                    note_as(&s, CONSILIUM, &note);
                }
                let mut start = Vec::new();
                {
                    let mut st = s.state.lock().unwrap();
                    // An answer begun before the human's newest message doesn't use up the
                    // answers that message gave.
                    if st.generation == asked_in {
                        *st.answered.entry(id.clone()).or_default() += 1;
                    }
                    finish_thinking(&s, &mut st, &id);
                    st.latest.insert(id.clone(), ev.seq);
                    if !ev.requests.is_empty() {
                        st.paused = true;
                    }
                    // @mentioned members get another answer, and join in if they aren't taking part.
                    let limit = MAX_PULLED_IN * rounds as usize;
                    let now = now_ms();
                    let mut allowance_changed = false;
                    for m in to.iter().filter(|m| {
                        **m != id
                            && s.turn.members.contains(m)
                            && agents::by_id(m).is_some()
                            && s.bench.until(m, now).is_none()
                            && !crate::switched::is_off(s.store.dir(), &s.turn.conversation, m)
                    }) {
                        if st.pulled_in >= limit {
                            break;
                        }
                        let used = committed_answers(&st, m);
                        let budget = rounds + st.extra.get(m).copied().unwrap_or(0);
                        if st.passed.contains(m) || used >= budget || !st.live.contains(m) {
                            let extra = st.extra.entry(m.clone()).or_default();
                            if used >= budget {
                                *extra += 1;
                            }
                            st.passed.remove(m);
                            st.pulled_in += 1;
                            allowance_changed = true;
                            if !st.live.contains(m) {
                                st.live.insert(m.clone());
                                st.joined.insert(m.clone());
                                start.push(m.clone());
                            }
                        }
                    }
                    if allowance_changed {
                        s.host.extra_answers(&st.extra, st.pulled_in);
                    }
                }
                for m in start {
                    tokio::spawn(worker(s.clone(), m, Some(ev.seq - 1)));
                }
                s.wake.notify_waiters();
            }
            Err(why) if why == "stopped" => {
                finish_thinking(&s, &mut s.state.lock().unwrap(), &id);
                note_as(&s, &id, &format!("Stopped {}.", agent.name));
                break;
            }
            Err(why) => {
                finish_thinking(&s, &mut s.state.lock().unwrap(), &id);
                let now = now_ms();
                if let Some(until) = bench::usage_limit(&why, now) {
                    let when = match s.bench.bench(&id, until, bench::reset_reported(&why, now)) {
                        Some(t) => format!("its limit resets in about {}", about(t.saturating_sub(now))),
                        None => "it hasn't said when its limit resets".to_string(),
                    };
                    note_as(
                        &s,
                        CONSILIUM,
                        &format!("{} ran out of usage ({when}); the council carries on without it.", agent.name),
                    );
                } else {
                    let off_here = crate::switched::is_off(s.store.dir(), &s.turn.conversation, &id);
                    if !off_here {
                        if let Ok(e) = s.store.append(&s.turn.conversation, &id, "error", vec![], &format!("{} couldn't answer: {why}", agent.name)) {
                            s.host.event(&e);
                        }
                        alarm(&s, agent, &why);
                    }
                }
                failed = true;
                break;
            }
        }
    }
    (failed, last_seen)
}

async fn work(s: Arc<Shared>, id: String, mut last_seen: Option<u64>) {
    let agent = s.host.resolve_agent(&id).expect("known agent");
    let mut generation = s.state.lock().unwrap().generation;
    loop {
        let (failed, seen) = answer(s.clone(), id.clone(), last_seen).await;
        last_seen = seen;
        #[cfg(test)]
        {
            let gate = {
                let mut gate = s.before_worker_exit.lock().unwrap();
                if gate.as_ref().is_some_and(|gate| gate.agent == id) { gate.take() } else { None }
            };
            if let Some(gate) = gate {
                gate.reached.notify_one();
                gate.release.notified().await;
            }
        }
        let stand_in = {
            let mut st = s.state.lock().unwrap();
            // The human said something new while this AI was finishing: stay and read it.
            if !failed && !s.cancel.is_cancelled() && st.generation != generation {
                generation = st.generation;
                continue;
            }
            // A mention can grant an answer after answer() decides to leave but before this
            // worker leaves `live`. Check under the same lock used to grant or start workers.
            let budget = s.turn.rounds + st.extra.get(&id).copied().unwrap_or(0);
            let unread = last_seen.is_some_and(|seen| st.latest.iter().any(|(who, seq)| who != &id && *seq > seen));
            if !failed
                && !s.cancel.is_cancelled()
                && !st.paused
                && !st.stopped.contains(&id)
                && st.answered.get(&id).copied().unwrap_or(0) < budget
                && unread
                && !crate::switched::is_off(s.store.dir(), &s.turn.conversation, &id)
            {
                continue;
            }
            st.live.remove(&id);
            st.waiting.remove(&id);
            if failed && st.live.is_empty() && !st.paused && !s.cancel.is_cancelled() {
                let now = now_ms();
                // An AI switched off in this chat never stands in for one that couldn't answer.
                let available = |a: &'static agents::Agent| {
                    s.bench.until(a.id, now).is_none()
                        && s.host.installed(a)
                        && !crate::switched::is_off(s.store.dir(), &s.turn.conversation, a.id)
                };
                let unjoined: Vec<&'static agents::Agent> = s
                    .turn
                    .members
                    .iter()
                    .filter(|m| !st.joined.contains(*m))
                    .filter_map(|m| s.host.resolve_agent(m))
                    .collect();
                let pick = pick_stand_in(&unjoined, available);
                if let Some(a) = pick {
                    st.live.insert(a.id.to_string());
                    st.joined.insert(a.id.to_string());
                    Some(Some(a))
                } else if s
                    .turn
                    .members
                    .iter()
                    .filter(|m| **m != id)
                    .filter_map(|m| s.host.resolve_agent(m))
                    .any(available)
                {
                    // Others could answer; they have already taken part in this turn.
                    None
                } else {
                    Some(None)
                }
            } else {
                None
            }
        };
        match stand_in {
            Some(Some(a)) => {
                note_as(&s, CONSILIUM, &format!("{} is taking {}'s turn.", a.name, agent.name));
                tokio::spawn(worker(s.clone(), a.id.to_string(), None));
            }
            Some(None) => {
                note_as(&s, CONSILIUM, "No other AI in the council can answer right now, so this turn ends here. Send your message again once one is back.");
            }
            None => {}
        }
        s.wake.notify_waiters();
        break;
    }
}

/// Cloud council members get one chance before an available local model takes over.
fn pick_stand_in(
    unjoined: &[&'static agents::Agent],
    available: impl Fn(&'static agents::Agent) -> bool,
) -> Option<&'static agents::Agent> {
    unjoined
        .iter()
        .copied()
        .filter(|a| !agents::is_local(a))
        .find(|a| available(a))
        .or_else(|| unjoined.iter().copied().filter(|a| agents::is_local(a)).find(|a| available(a)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    type Script = dyn Fn(&str, u32, &[Event]) -> Result<String, String> + Send + Sync;

    #[derive(Clone)]
    struct AnswerGate {
        agent: &'static str,
        answer: u32,
        started: Arc<Notify>,
        release: Arc<Notify>,
    }

    impl AnswerGate {
        fn new(agent: &'static str, answer: u32) -> Self {
            Self { agent, answer, started: Arc::new(Notify::new()), release: Arc::new(Notify::new()) }
        }

        async fn wait_started(&self) {
            tokio::time::timeout(Duration::from_secs(5), self.started.notified()).await.expect("the answer starts");
        }
    }

    fn hold_worker_exit(council: &Council, agent: &str) -> WorkerExitGate {
        let gate = WorkerExitGate { agent: agent.into(), reached: Arc::new(Notify::new()), release: Arc::new(Notify::new()) };
        *council.shared.before_worker_exit.lock().unwrap() = Some(gate.clone());
        gate
    }

    struct Fake {
        script: Box<Script>,
        /// How long each AI takes to answer, in ms.
        delay: HashMap<&'static str, u64>,
        wait_for_gpt: Option<Arc<tokio::sync::Notify>>,
        answer_gates: Vec<AnswerGate>,
        allowance_updates: Mutex<Vec<(HashMap<String, u32>, usize)>>,
        asked: Mutex<Vec<(String, u32)>>,
        installed: HashSet<&'static str>,
        local: Option<&'static Agent>,
    }

    impl Host for Fake {
        fn resolve_agent(&self, id: &str) -> Option<&'static Agent> {
            self.local.filter(|agent| agent.id == id).or_else(|| agents::by_id(id))
        }

        fn ask(&self, agent: &'static Agent, answer: u32, _rounds: u32, events: Vec<Event>, cancel: CancellationToken) -> Reply {
            self.asked.lock().unwrap().push((agent.id.to_string(), answer));
            let r = (self.script)(agent.id, answer, &events).map(|text| AgentReply { text, usage: None, model: None });
            let d = self.delay.get(agent.id).copied().unwrap_or(10);
            let gate = self.wait_for_gpt.clone().filter(|_| agent.id == "claude");
            let answer_gate = self.answer_gates.iter().find(|gate| gate.agent == agent.id && gate.answer == answer).cloned();
            Box::pin(async move {
                let ready = async {
                    if let Some(gate) = answer_gate {
                        gate.started.notify_one();
                        gate.release.notified().await;
                    }
                    if let Some(gate) = gate {
                        gate.notified().await;
                    }
                    tokio::time::sleep(Duration::from_millis(d)).await;
                };
                tokio::select! {
                    _ = ready => r,
                    _ = cancel.cancelled() => Err("stopped".into()),
                }
            })
        }
        fn event(&self, e: &Event) {
            if e.kind == "message" && e.sender == "gpt" {
                if let Some(gate) = &self.wait_for_gpt {
                    gate.notify_one();
                }
            }
        }
        fn thinking(&self, _agents: Vec<String>, _round: u32) {}
        fn installed(&self, agent: &'static Agent) -> bool {
            self.installed.contains(agent.id)
        }
        fn remember(&self, _agent: &'static Agent, _reply: &str) -> Option<String> {
            None
        }
        fn extra_answers(&self, extra: &HashMap<String, u32>, pulled_in: usize) {
            self.allowance_updates.lock().unwrap().push((extra.clone(), pulled_in));
        }
    }

    fn fake(script: impl Fn(&str, u32, &[Event]) -> Result<String, String> + Send + Sync + 'static) -> Fake {
        Fake {
            script: Box::new(script),
            delay: HashMap::new(),
            wait_for_gpt: None,
            answer_gates: vec![],
            allowance_updates: Mutex::new(vec![]),
            asked: Mutex::new(vec![]),
            installed: ["gpt", "claude", "gemini"].into_iter().collect(),
            local: None,
        }
    }

    fn setup() -> (Arc<Store>, String) {
        let dir = std::env::temp_dir().join(format!("consilium-council-{}", uuid::Uuid::new_v4()));
        let store = Arc::new(Store::new(dir).unwrap());
        let c = store.create("t").unwrap().id;
        store.append(&c, "human", "message", vec![], "hello").unwrap();
        (store, c)
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    async fn go(store: &Arc<Store>, c: &str, host: Arc<Fake>, targets: &[&str], rounds: u32, bench: Arc<Bench>) -> Vec<Event> {
        go_with_members(store, c, host, targets, &["gpt", "claude", "gemini"], rounds, bench).await
    }

    async fn go_with_members(
        store: &Arc<Store>,
        c: &str,
        host: Arc<Fake>,
        targets: &[&str],
        members: &[&str],
        rounds: u32,
        bench: Arc<Bench>,
    ) -> Vec<Event> {
        let turn = Turn { conversation: c.into(), targets: ids(targets), rounds, members: ids(members) };
        tokio::time::timeout(Duration::from_secs(10), run(turn, store.clone(), bench, host, CancellationToken::new()))
            .await
            .expect("the turn ends");
        store.load(c).unwrap()
    }

    fn said(events: &[Event]) -> Vec<(String, String)> {
        events.iter().filter(|e| e.kind == "message" && e.sender != "human").map(|e| (e.sender.clone(), e.text.clone())).collect()
    }

    #[tokio::test]
    async fn everyone_answers_at_once_without_waiting_for_each_other() {
        let (store, c) = setup();
        let mut f = fake(|id, _, _| Ok(format!("hi from {id}")));
        // Claude is asked first and can finish only after the GPT reply is published.
        // A sequential council would time out in go() while waiting for Claude.
        f.wait_for_gpt = Some(Arc::new(tokio::sync::Notify::new()));
        let ev = go(&store, &c, Arc::new(f), &["claude", "gpt"], 1, Arc::new(Bench::new())).await;
        assert_eq!(said(&ev), vec![("gpt".into(), "hi from gpt".into()), ("claude".into(), "hi from claude".into())]);
    }

    #[tokio::test]
    async fn stopping_one_ai_in_its_council_box_ends_its_answer_and_the_others_carry_on() {
        let (store, c) = setup();
        let mut f = fake(|id, _, _| Ok(format!("hi from {id}")));
        // Claude would take a minute (a slow local model, say); ChatGPT is quick.
        f.delay.insert("claude", 60_000);
        f.delay.insert("gpt", 300);
        let turn = Turn { conversation: c.clone(), targets: ids(&["claude", "gpt"]), rounds: 1, members: ids(&["gpt", "claude", "gemini"]) };
        let council = Arc::new(Council::new(turn, store.clone(), Arc::new(Bench::new()), Arc::new(f), CancellationToken::new()));
        let running = tokio::spawn({
            let council = council.clone();
            async move { council.run().await }
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(council.stop_agent("claude"));
        tokio::time::timeout(Duration::from_secs(5), running).await.expect("the turn ends without waiting for Claude").unwrap();
        let ev = store.load(&c).unwrap();
        assert_eq!(said(&ev), vec![("gpt".into(), "hi from gpt".into())]);
        assert!(ev.iter().any(|e| e.sender == "claude" && e.text == "Stopped Claude."), "its stop is noted");
        assert!(!council.stop_agent("claude"), "the turn is over");
    }

    #[tokio::test]
    async fn seen_receipts_are_compact_and_not_repeated_on_the_next_turn() {
        let (store, c) = setup();
        let first_message = store.load(&c).unwrap()[0].id.clone();
        let first = go(
            &store,
            &c,
            Arc::new(fake(|_, _, _| Ok("first answer".into()))),
            &["gpt"],
            1,
            Arc::new(Bench::new()),
        )
        .await;
        let receipts: Vec<&Event> = first.iter().filter(|event| event.kind == "seen").collect();
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].sender, "gpt");
        assert_eq!(receipts[0].seen, vec![first_message.clone()]);

        let second_message = store.append(&c, "human", "message", vec![], "second").unwrap().id;
        let second = go(
            &store,
            &c,
            Arc::new(fake(|_, _, _| Ok("second answer".into()))),
            &["gpt"],
            1,
            Arc::new(Bench::new()),
        )
        .await;
        let seen: Vec<String> = second
            .iter()
            .filter(|event| event.kind == "seen" && event.sender == "gpt")
            .flat_map(|event| event.seen.clone())
            .collect();
        assert_eq!(seen, vec![first_message, second_message]);
    }

    #[tokio::test]
    async fn in_a_discussion_each_reacts_to_new_messages_until_they_pass() {
        let (store, c) = setup();
        let f = Arc::new(fake(|id, n, _| if n >= 3 { Ok("PASS".into()) } else { Ok(format!("{id} #{n}")) }));
        let ev = go(&store, &c, f.clone(), &["gpt", "claude"], 5, Arc::new(Bench::new())).await;
        let s = said(&ev);
        assert_eq!(s.iter().filter(|(a, _)| a == "gpt").count(), 2);
        assert_eq!(s.iter().filter(|(a, _)| a == "claude").count(), 2);
        assert!(ev.iter().any(|e| e.text == "Discussion ended: nobody had more to add."));
    }

    #[tokio::test]
    async fn a_real_error_alerts_the_others_once_and_not_again_until_it_answers() {
        let (store, c) = setup();
        let f = Arc::new(fake(|id, _, _| match id {
            "claude" => Err("error sending request".into()),
            _ => Ok(format!("{id} looking into it")),
        }));
        let ev = go(&store, &c, f.clone(), &["claude", "gpt"], 1, Arc::new(Bench::new())).await;
        let alarms: Vec<&Event> = ev.iter().filter(|e| e.sender == CONSILIUM && e.text.contains("couldn't answer: error sending request")).collect();
        assert_eq!(alarms.len(), 1, "one alarm: {alarms:?}");
        assert_eq!(alarms[0].to, ids(&["gpt", "gemini"]));
        assert!(said(&ev).iter().any(|(a, _)| a == "gemini"), "gemini was brought in");
        assert_eq!(f.asked.lock().unwrap().iter().filter(|(a, _)| a == "claude").count(), 1, "the failing AI isn't asked again");

        // The same failure next turn raises no new alarm.
        store.append(&c, "human", "message", vec![], "again").unwrap();
        let ev = go(&store, &c, f.clone(), &["claude"], 1, Arc::new(Bench::new())).await;
        assert_eq!(ev.iter().filter(|e| e.sender == CONSILIUM && e.text.contains("couldn't answer")).count(), 1);
    }

    #[tokio::test]
    async fn an_alerted_ai_failing_too_raises_no_second_alarm() {
        let (store, c) = setup();
        let f = Arc::new(fake(|id, _, _| match id {
            "gpt" => Ok("gpt here".into()),
            _ => Err(format!("{id} crashed")),
        }));
        let ev = go(&store, &c, f, &["claude"], 1, Arc::new(Bench::new())).await;
        assert_eq!(ev.iter().filter(|e| e.sender == CONSILIUM && e.text.contains("couldn't answer")).count(), 1);
        assert_eq!(ev.iter().filter(|e| e.kind == "error").count(), 2, "both failures are still shown");
    }

    #[tokio::test]
    async fn running_out_of_usage_raises_no_alarm() {
        let (store, c) = setup();
        let f = Arc::new(fake(|id, _, _| match id {
            "claude" => Err("Claude AI usage limit reached|9999999999".into()),
            _ => Ok(format!("{id} here")),
        }));
        let ev = go(&store, &c, f, &["claude", "gpt"], 1, Arc::new(Bench::new())).await;
        assert!(!ev.iter().any(|e| e.sender == CONSILIUM && e.text.contains("couldn't answer")));
    }

    #[tokio::test]
    async fn switched_off_ai_is_not_asked_and_raises_no_alarm_or_error() {
        let (store, c) = setup();
        crate::switched::set(store.dir(), &c, "claude", false).unwrap();
        let f = Arc::new(fake(|id, _, _| match id {
            "claude" => Err("Claude crashed".into()),
            _ => Ok(format!("{id} here")),
        }));
        let ev = go(&store, &c, f.clone(), &["claude", "gpt"], 1, Arc::new(Bench::new())).await;
        assert!(!ev.iter().any(|e| e.sender == CONSILIUM && e.text.contains("couldn't answer")));
        assert!(!ev.iter().any(|e| e.sender == "claude" && e.kind == "error"));
        assert_eq!(f.asked.lock().unwrap().iter().filter(|(a, _)| a == "claude").count(), 0);
    }

    #[tokio::test]
    async fn an_ai_switched_off_in_the_chat_never_stands_in() {
        let (store, c) = setup();
        crate::switched::set(store.dir(), &c, "gpt", false).unwrap();
        let f = Arc::new(fake(|id, _, _| match id {
            "claude" => Err("rate limit exceeded".into()),
            _ => Ok(format!("{id} here")),
        }));
        let ev = go(&store, &c, f.clone(), &["claude"], 1, Arc::new(Bench::new())).await;
        assert!(f.asked.lock().unwrap().iter().all(|(a, _)| a != "gpt"), "a switched-off AI was asked");
        assert!(!ev.iter().any(|e| e.text.contains("ChatGPT is taking")));
    }

    #[tokio::test]
    async fn switched_off_ai_is_not_started_by_mention() {
        let (store, c) = setup();
        crate::switched::set(store.dir(), &c, "claude", false).unwrap();
        let f = Arc::new(fake(|id, _, _| match id {
            "gpt" => Ok("Hey @claude check this out".into()),
            _ => Ok(format!("{id} here")),
        }));
        let ev = go(&store, &c, f.clone(), &["gpt"], 2, Arc::new(Bench::new())).await;
        assert_eq!(f.asked.lock().unwrap().iter().filter(|(a, _)| a == "claude").count(), 0);
        assert!(!ev.iter().any(|e| e.sender == "claude"));
    }

    #[tokio::test]
    async fn an_ai_out_of_usage_is_benched_and_the_others_carry_on() {
        let (store, c) = setup();
        let bench = Arc::new(Bench::new());
        let f = Arc::new(fake(|id, n, _| match id {
            "claude" => Err("Claude AI usage limit reached|9999999999".into()),
            _ if n >= 3 => Ok("PASS".into()),
            _ => Ok(format!("{id} #{n}")),
        }));
        let ev = go(&store, &c, f.clone(), &["gpt", "claude"], 3, bench.clone()).await;
        assert!(bench.until("claude", now_ms()).is_some(), "claude is benched");
        assert!(ev.iter().any(|e| e.sender == CONSILIUM && e.text.contains("Claude ran out of usage")));
        assert!(said(&ev).iter().any(|(a, _)| a == "gpt"));
        assert_eq!(f.asked.lock().unwrap().iter().filter(|(a, _)| a == "claude").count(), 1, "not asked again");
        assert!(!ev.iter().any(|e| e.kind == "error"));
    }

    #[tokio::test]
    async fn when_the_only_ai_asked_runs_out_another_takes_its_turn() {
        let (store, c) = setup();
        let f = Arc::new(fake(|id, _, _| match id {
            "claude" => Err("rate limit exceeded".into()),
            _ => Ok(format!("{id} here")),
        }));
        let ev = go(&store, &c, f, &["claude"], 1, Arc::new(Bench::new())).await;
        assert_eq!(said(&ev), vec![("gpt".into(), "gpt here".into())]);
        assert!(ev.iter().any(|e| e.text == "ChatGPT is taking Claude's turn."));
    }

    #[test]
    fn exhausted_clouds_are_tried_once_before_one_local_stand_in_takes_over() {
        let local = crate::customai::test_agent(crate::customai::CustomAi {
            id: "localstandin".into(),
            name: "Local Stand-in".into(),
            base_url: "http://127.0.0.1:18080/v1".into(),
            model: "test-local".into(),
            local: true,
        });
        let candidates = [agents::by_id("gpt").unwrap(), agents::by_id("claude").unwrap(), local];
        let mut exhausted = HashSet::new();

        let first = pick_stand_in(&candidates, |a| !exhausted.contains(a.id)).unwrap();
        assert_eq!(first.id, "gpt");
        exhausted.insert(first.id);
        let second = pick_stand_in(&candidates, |a| !exhausted.contains(a.id)).unwrap();
        assert_eq!(second.id, "claude");
        exhausted.insert(second.id);
        let third = pick_stand_in(&candidates, |a| !exhausted.contains(a.id)).unwrap();
        assert_eq!(third.id, "localstandin", "each cloud is tried once before the custom local provider");
        assert!(agents::is_local(third));
    }

    #[tokio::test]
    async fn local_stand_in_continues_the_same_turn_with_chat_context() {
        let (store, conversation) = setup();
        let local = crate::customai::test_agent(crate::customai::CustomAi {
            id: "localcontext".into(),
            name: "Local Context".into(),
            base_url: "http://127.0.0.1:18080/v1".into(),
            model: "test-local".into(),
            local: true,
        });
        let mut host = fake(|id, _, events| match id {
            "claude" | "gpt" => Err("quota exceeded".into()),
            "localcontext" => {
                assert!(events.iter().any(|e| e.sender == "human" && e.text == "hello"));
                Ok("Continued locally".into())
            }
            _ => panic!("unexpected stand-in: {id}"),
        });
        host.installed.insert(local.id);
        host.local = Some(local);
        let host = Arc::new(host);
        let events = go_with_members(
            &store, &conversation, host.clone(), &["claude"], &["claude", "gpt", local.id],
            1, Arc::new(Bench::new()),
        ).await;
        assert_eq!(said(&events), vec![(local.id.into(), "Continued locally".into())]);
        let asked = host.asked.lock().unwrap();
        assert_eq!(asked.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(), ["claude", "gpt", local.id]);
    }

    #[tokio::test]
    async fn switched_off_last_stand_in_reports_that_nobody_can_answer() {
        let (store, c) = setup();
        crate::switched::set(store.dir(), &c, "gpt", false).unwrap();
        crate::switched::set(store.dir(), &c, "gemini", false).unwrap();
        let host = Arc::new(fake(|id, _, _| match id {
            "claude" => Err("quota exceeded".into()),
            _ => panic!("switched-off stand-in was asked: {id}"),
        }));
        let ev = go(&store, &c, host, &["claude"], 1, Arc::new(Bench::new())).await;
        assert!(ev.iter().any(|e| e.text.starts_with("No other AI in the council can answer")));
    }

    #[tokio::test]
    async fn a_benched_ai_is_skipped_at_the_start_and_says_so() {
        let (store, c) = setup();
        let bench = Arc::new(Bench::new());
        bench.bench("claude", now_ms() + 3_600_000, true);
        let f = Arc::new(fake(|id, _, _| Ok(format!("{id} here"))));
        let ev = go(&store, &c, f.clone(), &["claude", "gpt"], 1, bench).await;
        assert_eq!(said(&ev), vec![("gpt".into(), "gpt here".into())]);
        assert!(ev.iter().any(|e| e.text.contains("Claude is out of usage")));
        assert!(f.asked.lock().unwrap().iter().all(|(a, _)| a != "claude"));
    }

    #[tokio::test]
    async fn when_every_ai_is_out_the_turn_ends_with_a_note() {
        let (store, c) = setup();
        let f = Arc::new(fake(|_, _, _| Err("quota exceeded".into())));
        let ev = go(&store, &c, f, &["gpt"], 1, Arc::new(Bench::new())).await;
        assert!(said(&ev).is_empty());
        assert!(ev.iter().any(|e| e.text.starts_with("No other AI in the council can answer")));
    }

    #[tokio::test]
    async fn a_mention_brings_another_member_in() {
        let (store, c) = setup();
        let f = Arc::new(fake(|id, _, _| Ok(if id == "gpt" { "@gemini what do you think?".into() } else { format!("{id} agrees") })));
        let ev = go(&store, &c, f, &["gpt"], 1, Arc::new(Bench::new())).await;
        assert!(said(&ev).iter().any(|(a, _)| a == "gemini"));
    }

    #[tokio::test]
    async fn a_request_for_the_human_pauses_the_discussion() {
        let (store, c) = setup();
        let mut f = fake(|id, _, _| Ok(if id == "gpt" { "REQUEST: which database?".into() } else { format!("{id} talks") }));
        f.delay.insert("claude", 200);
        let ev = go(&store, &c, Arc::new(f), &["gpt", "claude"], 5, Arc::new(Bench::new())).await;
        assert!(ev.iter().any(|e| e.text == "Discussion paused: a question for you is waiting."));
        assert!(said(&ev).iter().filter(|(a, _)| a == "claude").count() <= 1, "no new answers after the request");
    }

    #[tokio::test]
    async fn stop_ends_every_answer_at_once() {
        let (store, c) = setup();
        let mut f = fake(|id, _, _| Ok(id.to_string()));
        f.delay.insert("gpt", 30_000);
        f.delay.insert("claude", 30_000);
        let cancel = CancellationToken::new();
        let c2 = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            c2.cancel();
        });
        let turn = Turn { conversation: c.clone(), targets: ids(&["gpt", "claude"]), rounds: 3, members: ids(&["gpt", "claude"]) };
        tokio::time::timeout(Duration::from_secs(3), run(turn, store.clone(), Arc::new(Bench::new()), Arc::new(f), cancel))
            .await
            .expect("stopped promptly");
        let ev = store.load(&c).unwrap();
        assert_eq!(ev.iter().filter(|e| e.text.starts_with("Stopped ")).count(), 2);
    }

    #[tokio::test]
    async fn a_resumed_turn_carries_on_without_repeating_answers() {
        let (store, c) = setup();
        store.append(&c, "gpt", "message", vec![], "gpt #1").unwrap();
        let f = Arc::new(fake(|id, n, _| if n >= 2 { Ok("PASS".into()) } else { Ok(format!("{id} #{n}")) }));
        let ev = go(&store, &c, f.clone(), &["gpt", "claude"], 3, Arc::new(Bench::new())).await;
        let asked = f.asked.lock().unwrap().clone();
        assert!(asked.contains(&("claude".into(), 1)), "claude answers for the first time");
        assert!(!asked.contains(&("gpt".into(), 1)), "gpt's first answer isn't repeated");
        assert_eq!(said(&ev).iter().filter(|(_, t)| t == "gpt #1").count(), 1);
    }

    #[tokio::test]
    async fn a_fresh_bounded_turn_has_new_answers_and_keeps_the_full_transcript() {
        let (store, c) = setup();
        store.append(&c, "gpt", "message", vec![], "earlier local answer").unwrap();
        store.append(&c, "claude", "system", vec![], "Claude had nothing to add.").unwrap();
        let ask = store.append_imported(
            &c, "gpt", "message", vec![], "new peer request", Some("MacBook".into()), None,
        ).unwrap();
        let boundary = store.append(&c, "gpt", "message", vec![], "late answer from the previous turn").unwrap();
        assert!(boundary.seq > ask.seq);
        let host = Arc::new(fake(|id, n, events| {
            assert!(events.iter().any(|e| e.text == "earlier local answer"));
            assert!(events.iter().any(|e| e.text == "new peer request"));
            assert!(events.iter().any(|e| e.text == "late answer from the previous turn"));
            Ok(format!("{id} fresh #{n}"))
        }));
        let turn = Turn { conversation: c.clone(), targets: ids(&["gpt", "claude"]), rounds: 1, members: ids(&["gpt", "claude"]) };
        let council = Council::new_after(
            turn, store.clone(), Arc::new(Bench::new()), host.clone(), CancellationToken::new(), Some(boundary.seq),
        );
        tokio::time::timeout(Duration::from_secs(5), council.run()).await.expect("the fresh turn ends");
        let mut asked = host.asked.lock().unwrap().clone();
        asked.sort();
        assert_eq!(asked, vec![("claude".into(), 1), ("gpt".into(), 1)]);
    }

    #[tokio::test]
    async fn a_resumed_bounded_turn_keeps_only_its_own_completed_answers() {
        let (store, c) = setup();
        store.append(&c, "gpt", "message", vec![], "earlier turn").unwrap();
        let boundary = store.append_imported(
            &c, "gpt", "message", vec![], "start this turn", Some("MacBook".into()), None,
        ).unwrap();
        store.append(&c, "gpt", "message", vec![], "completed in this turn").unwrap();
        store.append(&c, "claude", "system", vec![], "Claude had nothing to add.").unwrap();
        store.append_imported(&c, "gpt", "message", vec![], "peer follow-up", Some("MacBook".into()), None).unwrap();
        let host = Arc::new(fake(|id, n, _| Ok(format!("{id} resumed #{n}"))));
        let turn = Turn { conversation: c.clone(), targets: ids(&["gpt", "claude"]), rounds: 2, members: ids(&["gpt", "claude"]) };
        let council = Council::new_after(
            turn, store.clone(), Arc::new(Bench::new()), host.clone(), CancellationToken::new(), Some(boundary.seq),
        );
        tokio::time::timeout(Duration::from_secs(5), council.run()).await.expect("the resumed turn ends");
        assert_eq!(*host.asked.lock().unwrap(), vec![("gpt".into(), 2)]);
        let events = store.load(&c).unwrap();
        assert_eq!(events.iter().filter(|e| e.text == "completed in this turn").count(), 1);
        assert!(events.iter().any(|e| e.text == "gpt resumed #2"));
    }

    #[tokio::test]
    async fn a_resumed_one_round_turn_restores_extra_answers_and_unstarted_recipients() {
        // First run alone: another AI's reply must not mask a lost pending request by waking GPT.
        for include_unstarted in [false, true] {
            let (store, c) = setup();
            let boundary = store.load(&c).unwrap().last().unwrap().seq;
            store.append_imported(&c, "claude", "message", vec![], "peer follow-up", Some("MacBook".into()), None).unwrap();
            store.append(&c, "gpt", "message", vec![], "completed answer").unwrap();
            let mut extra = HashMap::from([("gpt".to_string(), 1)]);
            if include_unstarted {
                extra.insert("gemini".to_string(), 0);
            }
            let host = Arc::new(fake(|id, n, events| {
                assert!(events.iter().any(|e| e.text == "peer follow-up"));
                Ok(format!("{id} restored #{n}"))
            }));
            let members = if include_unstarted { ids(&["gpt", "gemini"]) } else { ids(&["gpt"]) };
            let turn = Turn { conversation: c.clone(), targets: ids(&["gpt"]), rounds: 1, members };
            let pulled_in = if include_unstarted { 2 } else { 1 };
            let council = Council::new_with_allowance(
                turn, store.clone(), Arc::new(Bench::new()), host.clone(), CancellationToken::new(), Some(boundary), (extra.clone(), pulled_in),
            );
            tokio::time::timeout(Duration::from_secs(5), council.run()).await.expect("the restored turn ends");
            let mut asked = host.asked.lock().unwrap().clone();
            asked.sort();
            let expected = if include_unstarted { vec![("gemini".into(), 1), ("gpt".into(), 2)] } else { vec![("gpt".into(), 2)] };
            assert_eq!(asked, expected);
            let state = council.shared.state.lock().unwrap();
            assert_eq!(state.extra, extra);
            assert_eq!(state.pulled_in, pulled_in);
        }
    }

    #[tokio::test]
    async fn a_peer_mention_after_the_answer_loop_exits_is_persisted_and_read() {
        let (store, c) = setup();
        let host = Arc::new(fake(|_, n, events| {
            if n == 2 {
                assert!(events.iter().any(|e| e.text == "arrived at exit"));
            }
            Ok(format!("answer #{n}"))
        }));
        let turn = Turn { conversation: c.clone(), targets: ids(&["gpt"]), rounds: 1, members: ids(&["gpt"]) };
        let council = Arc::new(Council::new(turn, store.clone(), Arc::new(Bench::new()), host.clone(), CancellationToken::new()));
        let exit = hold_worker_exit(&council, "gpt");
        let runner = council.clone();
        let done = tokio::spawn(async move { runner.run().await });
        tokio::time::timeout(Duration::from_secs(5), exit.reached.notified()).await.expect("the worker reaches its exit");
        let ask = store.append_imported(&c, "claude", "message", vec![], "arrived at exit", Some("MacBook".into()), None).unwrap();
        assert!(council.mentioned(&ask, &ids(&["gpt"])));
        assert_eq!(*host.allowance_updates.lock().unwrap(), vec![(HashMap::from([("gpt".to_string(), 1)]), 1)]);
        exit.release.notify_one();
        tokio::time::timeout(Duration::from_secs(5), done).await.expect("the follow-up is answered").unwrap();
        assert_eq!(*host.asked.lock().unwrap(), vec![("gpt".into(), 1), ("gpt".into(), 2)]);
    }

    #[tokio::test]
    async fn a_peer_mention_at_worker_exit_does_not_resume_a_stop_pause_or_failure() {
        for outcome in ["stop", "pause", "failure"] {
            let (store, c) = setup();
            let host = Arc::new(fake(move |_, _, _| match outcome {
                "pause" => Ok("REQUEST: choose a database".into()),
                "failure" => Err("provider unavailable".into()),
                _ => Ok("completed answer".into()),
            }));
            let turn = Turn { conversation: c.clone(), targets: ids(&["gpt"]), rounds: 1, members: ids(&["gpt"]) };
            let council = Arc::new(Council::new(turn, store.clone(), Arc::new(Bench::new()), host.clone(), CancellationToken::new()));
            let exit = hold_worker_exit(&council, "gpt");
            let runner = council.clone();
            let done = tokio::spawn(async move { runner.run().await });
            tokio::time::timeout(Duration::from_secs(5), exit.reached.notified()).await.expect("the worker reaches its exit");
            if outcome == "stop" {
                assert!(council.stop_agent("gpt"));
            }
            let ask = store.append_imported(&c, "claude", "message", vec![], "peer follow-up", Some("MacBook".into()), None).unwrap();
            assert!(council.mentioned(&ask, &ids(&["gpt"])));
            exit.release.notify_one();
            tokio::time::timeout(Duration::from_secs(5), done).await.expect("the halted turn ends").unwrap();
            assert_eq!(*host.asked.lock().unwrap(), vec![("gpt".into(), 1)], "{outcome} must remain stopped");
        }
    }

    #[tokio::test]
    async fn peer_mentions_during_the_final_answer_reserve_one_follow_up_and_obey_the_cap() {
        let (store, c) = setup();
        let snapshots = Arc::new(Mutex::new(Vec::new()));
        let recorded = snapshots.clone();
        let mut host = fake(move |_, n, events| {
            let peer_texts: Vec<String> = events.iter().filter(|e| e.via.is_some()).map(|e| e.text.clone()).collect();
            recorded.lock().unwrap().push((n, peer_texts));
            Ok(format!("answer #{n}"))
        });
        let gates = [AnswerGate::new("gpt", 1), AnswerGate::new("gpt", 2), AnswerGate::new("gpt", 3)];
        host.answer_gates = gates.to_vec();
        let host = Arc::new(host);
        let turn = Turn { conversation: c.clone(), targets: ids(&["gpt"]), rounds: 1, members: ids(&["gpt"]) };
        let council = Arc::new(Council::new(turn, store.clone(), Arc::new(Bench::new()), host.clone(), CancellationToken::new()));
        let runner = council.clone();
        let done = tokio::spawn(async move { runner.run().await });
        gates[0].wait_started().await;
        for text in ["first request", "second request"] {
            let ask = store.append_imported(&c, "claude", "message", vec![], text, Some("MacBook".into()), None).unwrap();
            assert!(council.mentioned(&ask, &ids(&["gpt"])));
        }
        {
            let state = council.shared.state.lock().unwrap();
            assert_eq!(state.extra.get("gpt"), Some(&1), "requests share the pending follow-up");
            assert_eq!(state.pulled_in, 1);
        }
        gates[0].release.notify_one();
        gates[1].wait_started().await;
        let ask = store.append_imported(&c, "claude", "message", vec![], "third request", Some("MacBook".into()), None).unwrap();
        assert!(council.mentioned(&ask, &ids(&["gpt"])));
        gates[1].release.notify_one();
        gates[2].wait_started().await;
        let ask = store.append_imported(&c, "claude", "message", vec![], "past the cap", Some("MacBook".into()), None).unwrap();
        assert!(council.mentioned(&ask, &ids(&["gpt"])));
        gates[2].release.notify_one();
        tokio::time::timeout(Duration::from_secs(5), done).await.expect("the bounded turn ends").unwrap();
        assert_eq!(*host.asked.lock().unwrap(), vec![("gpt".into(), 1), ("gpt".into(), 2), ("gpt".into(), 3)]);
        assert_eq!(*snapshots.lock().unwrap(), vec![
            (1, vec![]),
            (2, vec!["first request".to_string(), "second request".to_string()]),
            (3, vec!["first request".to_string(), "second request".to_string(), "third request".to_string()]),
        ]);
        assert_eq!(council.shared.state.lock().unwrap().pulled_in, MAX_PULLED_IN);
    }

    #[tokio::test]
    async fn a_peer_mention_after_the_human_joins_uses_the_fresh_answer_already_available() {
        let (store, c) = setup();
        let gate = AnswerGate::new("gpt", 1);
        let mut host = fake(|_, _, _| Ok("answer".into()));
        host.answer_gates.push(gate.clone());
        let host = Arc::new(host);
        let turn = Turn { conversation: c.clone(), targets: ids(&["gpt"]), rounds: 1, members: ids(&["gpt"]) };
        let council = Arc::new(Council::new(turn, store.clone(), Arc::new(Bench::new()), host.clone(), CancellationToken::new()));
        let runner = council.clone();
        let done = tokio::spawn(async move { runner.run().await });
        gate.wait_started().await;
        let human = store.append(&c, "human", "message", vec![], "follow-up").unwrap();
        assert!(council.join(&human, &ids(&["gpt"]), &|task| { tokio::spawn(task); }));
        let ask = store.append_imported(&c, "claude", "message", vec![], "peer detail", Some("MacBook".into()), None).unwrap();
        assert!(council.mentioned(&ask, &ids(&["gpt"])));
        {
            let state = council.shared.state.lock().unwrap();
            assert!(state.extra.is_empty(), "the old in-flight answer does not consume a fresh slot");
            assert_eq!(state.pulled_in, 0);
        }
        gate.release.notify_one();
        gate.wait_started().await;
        gate.release.notify_one();
        tokio::time::timeout(Duration::from_secs(5), done).await.expect("the turn ends after the fresh answer").unwrap();
        assert_eq!(*host.asked.lock().unwrap(), vec![("gpt".into(), 1), ("gpt".into(), 1)]);
    }

    #[tokio::test]
    async fn an_ai_addressed_by_handle_from_another_computer_joins_and_answers_once() {
        let (store, c) = setup();
        let mut f = fake(|id, _, _| Ok(format!("{id} here")));
        f.delay.insert("gpt", 400);
        let turn = Turn { conversation: c.clone(), targets: ids(&["gpt"]), rounds: 1, members: ids(&["gpt", "gemini"]) };
        let council = Arc::new(Council::new(turn, store.clone(), Arc::new(Bench::new()), Arc::new(f), CancellationToken::new()));
        let runner = council.clone();
        let done = tokio::spawn(async move { runner.run().await });
        tokio::time::sleep(Duration::from_millis(100)).await;
        let ask = store.append_imported(&c, "claude", "message", vec![], "@gemini-pc can you check?", Some("MacBook".into()), None).unwrap();
        assert!(council.mentioned(&ask, &ids(&["gemini"])));
        tokio::time::timeout(Duration::from_secs(10), done).await.expect("the turn ends").unwrap();
        let s = said(&store.load(&c).unwrap());
        assert_eq!(s.iter().filter(|(a, _)| a == "gemini").count(), 1, "gemini was brought in once: {s:?}");
        assert_eq!(s.iter().filter(|(a, _)| a == "gpt").count(), 1);
    }

    #[tokio::test]
    async fn another_computers_answer_is_read_by_the_running_turn() {
        let (store, c) = setup();
        let mut f = fake(|id, _, ev| {
            let remote = ev.iter().any(|e| e.via.as_deref() == Some("MacBook"));
            Ok(format!("{id} {}", if remote { "read MacBook" } else { "alone" }))
        });
        f.delay.insert("gpt", 400);
        let turn = Turn { conversation: c.clone(), targets: ids(&["gpt"]), rounds: 2, members: ids(&["gpt"]) };
        let council = Arc::new(Council::new(turn, store.clone(), Arc::new(Bench::new()), Arc::new(f), CancellationToken::new()));
        let runner = council.clone();
        let done = tokio::spawn(async move { runner.run().await });
        // While ChatGPT is answering, the MacBook's Claude answers; ChatGPT reads it next.
        tokio::time::sleep(Duration::from_millis(100)).await;
        let remote = store.append_imported(&c, "claude", "message", vec![], "from the Mac", Some("MacBook".into()), None).unwrap();
        assert!(council.heard(&remote));
        tokio::time::timeout(Duration::from_secs(10), done).await.expect("the turn ends").unwrap();
        let s = said(&store.load(&c).unwrap());
        assert_eq!(
            s.iter().filter(|(a, _)| a == "gpt").map(|(_, t)| t.as_str()).collect::<Vec<_>>(),
            vec!["gpt alone", "gpt read MacBook"],
            "no extra answers beyond the turn's two: {s:?}"
        );
        assert!(!council.heard(&remote), "an ended turn starts nothing");
    }

    #[tokio::test]
    async fn a_plain_thread_can_join_a_live_council_and_start_an_addressed_ai() {
        let (store, c) = setup();
        let mut f = fake(|id, _, events| {
            let text = events.iter().rev().find(|event| event.sender == "human").unwrap().text.as_str();
            Ok(format!("{id} heard: {text}"))
        });
        f.delay.insert("gpt", 30_000);
        let host = Arc::new(f);
        let cancel = CancellationToken::new();
        let _cancel_on_drop = cancel.clone().drop_guard();
        let turn = Turn { conversation: c.clone(), targets: ids(&["gpt"]), rounds: 1, members: ids(&["gpt", "claude"]) };
        let council = Arc::new(Council::new(turn, store.clone(), Arc::new(Bench::new()), host.clone(), cancel.clone()));
        let runner = council.clone();
        let done = tokio::spawn(async move { runner.run().await });
        tokio::time::timeout(Duration::from_secs(5), async {
            while !host.asked.lock().unwrap().iter().any(|(id, _)| id == "gpt") {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }).await.expect("The first AI must be active before the phone message arrives");

        let message = store.append(&c, "human", "message", ids(&["claude"]), "phone follow-up").unwrap();
        let runtime = tokio::runtime::Handle::current();
        // This is the same injected executor the engine supplies, callable outside its runtime.
        let spawn: Box<crate::engine::Spawn> = Box::new(move |task| { runtime.spawn(task); });
        let intake = std::thread::spawn(move || {
            assert!(tokio::runtime::Handle::try_current().is_err(), "Signal's intake thread has no runtime context");
            council.join(&message, &ids(&["claude"]), spawn.as_ref())
        });
        assert!(intake.join().expect("Joining from Signal's plain thread must not panic"));

        let reply = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(reply) = store.load(&c).unwrap().into_iter().find(|event| event.sender == "claude" && event.kind == "message") {
                    break reply;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }).await;
        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(5), done).await.expect("The fake council must stop promptly").unwrap();
        assert_eq!(reply.expect("The newly addressed AI must record a real reply").text, "claude heard: phone follow-up");
        assert_eq!(store.load(&c).unwrap().iter().filter(|event| event.sender == "claude" && event.kind == "message").count(), 1);
    }

    #[tokio::test]
    async fn a_message_sent_mid_turn_is_read_without_interrupting_anyone() {
        let (store, c) = setup();
        // Each AI answers whatever the human said last.
        let mut f = fake(|id, _, ev| {
            let last = ev
                .iter()
                .rev()
                .find(|e| e.sender == "human")
                .map(|e| e.text.clone())
                .unwrap_or_default();
            Ok(format!("{id} read: {last}"))
        });
        f.delay.insert("claude", 600);
        let host = Arc::new(f);
        let turn = Turn {
            conversation: c.clone(),
            targets: ids(&["gpt", "claude"]),
            rounds: 1,
            members: ids(&["gpt", "claude", "gemini"]),
        };
        let council = Arc::new(Council::new(
            turn,
            store.clone(),
            Arc::new(Bench::new()),
            host.clone(),
            CancellationToken::new(),
        ));
        let runner = council.clone();
        let done = tokio::spawn(async move { runner.run().await });
        // GPT has answered and Claude is still busy when the human adds something and brings Gemini in.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let msg = store
            .append(&c, "human", "message", ids(&["gemini"]), "also this")
            .unwrap();
        assert!(council.join(&msg, &ids(&["gpt", "claude", "gemini"]), &|task| { tokio::spawn(task); }));
        tokio::time::timeout(Duration::from_secs(10), done)
            .await
            .expect("the turn ends")
            .unwrap();
        let s = said(&store.load(&c).unwrap());
        assert!(
            s.contains(&("claude".into(), "claude read: hello".into())),
            "claude wasn't interrupted: {s:?}"
        );
        for a in ["gpt", "claude", "gemini"] {
            assert!(
                s.contains(&(a.into(), format!("{a} read: also this"))),
                "{a} read the new message: {s:?}"
            );
        }
        assert!(!store
            .load(&c)
            .unwrap()
            .iter()
            .any(|e| e.text.starts_with("Stopped")));
        let late = store
            .append(&c, "human", "message", vec![], "after")
            .unwrap();
        assert!(
            !council.join(&late, &ids(&["gpt"]), &|task| { tokio::spawn(task); }),
            "an ended turn takes no more messages"
        );
    }

    #[test]
    fn progress_counts_answers_since_the_humans_last_message() {
        let (store, c) = setup();
        store.append(&c, "gpt", "message", vec![], "a").unwrap();
        store.append(&c, "claude", "system", vec![], "Claude had nothing to add.").unwrap();
        store.append(&c, CONSILIUM, "system", vec![], "update").unwrap();
        let p = progress(&store.load(&c).unwrap());
        assert_eq!(p.answered.get("gpt"), Some(&1));
        assert!(p.passed.contains("claude"));
        assert!(!p.answered.contains_key(CONSILIUM));
        store.append(&c, "human", "message", vec![], "next").unwrap();
        assert_eq!(progress(&store.load(&c).unwrap()), Progress::default());
        // A Consilium note addressed to the council (auto-continue) starts a fresh round too.
        store.append(&c, "gpt", "message", vec![], "b").unwrap();
        store.append(&c, CONSILIUM, "system", vec!["gpt".into()], "Auto-continue").unwrap();
        assert_eq!(progress(&store.load(&c).unwrap()), Progress::default());
    }

    #[test]
    fn progress_ignores_imported_answers_and_passes_after_a_signal_human_message() {
        let (store, c) = setup();
        store.append(&c, "gpt", "message", vec![], "before the human follow-up").unwrap();
        store.append_imported(&c, "human", "message", vec![], "from the phone", Some("Signal".into()), None).unwrap();
        let local = store.append(&c, "claude", "message", vec![], "local answer").unwrap();
        store.append_imported(&c, "gpt", "message", vec![], "remote answer", Some("MacBook".into()), None).unwrap();
        store.append_imported(&c, "gemini", "system", vec![], "Gemini had nothing to add.", Some("MacBook".into()), None).unwrap();
        let p = progress(&store.load(&c).unwrap());
        assert_eq!(p.answered, HashMap::from([("claude".to_string(), 1)]));
        assert_eq!(p.last, HashMap::from([("claude".to_string(), local.seq)]));
        assert!(p.passed.is_empty());
    }
}
