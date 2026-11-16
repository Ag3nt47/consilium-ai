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
    /// AIs working on an answer, with its number.
    thinking: Vec<(String, u32)>,
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
}

struct Shared {
    turn: Turn,
    store: Arc<Store>,
    bench: Arc<Bench>,
    host: Arc<dyn Host>,
    cancel: CancellationToken,
    state: Mutex<State>,
    wake: Notify,
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
    let start = events.iter().rposition(starts_turn).map_or(0, |i| i + 1);
    let mut p = Progress::default();
    for e in &events[start..] {
        if e.sender == "human" || e.sender == CONSILIUM {
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
        let events = store.load(&turn.conversation).unwrap_or_default();
        let done = progress(&events);
        let mut state = State::default();
        for e in events.iter().filter(|e| e.kind == "message" || starts_turn(e)) {
            state.latest.insert(e.sender.clone(), e.seq);
        }
        state.answered = done.answered.clone();
        state.passed = done.passed.clone();
        // Everyone addressed, plus anyone who joined in before a restart cut the turn short.
        let mut who: Vec<(String, Option<u64>)> = turn.targets.iter().map(|t| (t.clone(), done.last.get(t).copied())).collect();
        for (a, seq) in &done.last {
            if !turn.targets.contains(a) && turn.members.contains(a) {
                who.push((a.clone(), Some(*seq)));
            }
        }
        who.retain(|(a, _)| !done.passed.contains(a) && agents::by_id(a).is_some());
        for (a, _) in &who {
            state.live.insert(a.clone());
            state.joined.insert(a.clone());
        }
        let shared = Arc::new(Shared { turn, store, bench, host, cancel, state: Mutex::new(state), wake: Notify::new() });
        Council { shared, first: Mutex::new(who) }
    }

    /// The human said `message` while the turn is running. Nobody is interrupted: every AI gets a
    /// fresh set of answers, those waiting read it at once, one in the middle of an answer reads it
    /// as soon as it has finished, and anyone in `targets` who isn't taking part joins in.
    /// Returns false if the turn has already ended, so the message needs a turn of its own.
    pub fn join(&self, message: &Event, targets: &[String]) -> bool {
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
            st.paused = false;
            st.generation += 1;
            st.latest.insert(message.sender.clone(), message.seq);
            for t in targets {
                if agents::by_id(t).is_some() && s.bench.until(t, now_ms()).is_none() && !st.live.contains(t) {
                    st.live.insert(t.clone());
                    st.joined.insert(t.clone());
                    start.push(t.clone());
                }
            }
        }
        for t in start {
            tokio::spawn(worker(s.clone(), t, Some(message.seq.saturating_sub(1))));
        }
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
    let agents: Vec<String> = st.thinking.iter().map(|(a, _)| a.clone()).collect();
    let round = st.thinking.iter().map(|(_, n)| *n).max().unwrap_or(0);
    s.host.thinking(agents, round);
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
    let agent = agents::by_id(&id).expect("known agent");
    let rounds = s.turn.rounds;
    let mut failed = false;
    loop {
        if s.cancel.is_cancelled() || s.state.lock().unwrap().paused {
            break;
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
                    if st.paused || st.answered.get(&id).copied().unwrap_or(0) >= budget {
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
        let (n, asked_in) = {
            let mut st = s.state.lock().unwrap();
            let n = st.answered.get(&id).copied().unwrap_or(0) + 1;
            if n > rounds + st.extra.get(&id).copied().unwrap_or(0) {
                break;
            }
            st.thinking.push((id.clone(), n));
            emit_thinking(&s, &st);
            (n, st.generation)
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
        // A receipt means this bounded transcript was handed to the AI. Persist it before waiting
        // for the answer, so a slow, stopped, or failed turn still reports what it received.
        if !newly_seen.is_empty() {
            if let Ok(receipt) = s.store.append_seen(&s.turn.conversation, &id, newly_seen) {
                s.host.event(&receipt);
            }
        }
        let result = s.host.ask(agent, n, rounds, events, s.cancel.clone()).await;
        {
            let mut st = s.state.lock().unwrap();
            st.thinking.retain(|(a, _)| a != &id);
            emit_thinking(&s, &st);
        }
        match result {
            Ok(reply) if rounds > 1 && prompt::is_pass(&reply.text) => {
                s.bench.clear(&id);
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
                drop(st);
                break;
            }
            Ok(reply) => {
                s.bench.clear(&id);
                let to = prompt::mentioned_agents(&reply.text);
                let Ok(ev) = s.store.append_with_usage(&s.turn.conversation, &id, "message", to.clone(), &reply.text, reply.usage)
                else {
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
                    st.latest.insert(id.clone(), ev.seq);
                    if !ev.requests.is_empty() {
                        st.paused = true;
                    }
                    // @mentioned members get another answer, and join in if they aren't taking part.
                    let limit = MAX_PULLED_IN * rounds as usize;
                    let now = now_ms();
                    for m in to.iter().filter(|m| {
                        **m != id && s.turn.members.contains(m) && agents::by_id(m).is_some() && s.bench.until(m, now).is_none()
                    }) {
                        if st.pulled_in >= limit {
                            break;
                        }
                        let used = st.answered.get(m).copied().unwrap_or(0);
                        let budget = rounds + st.extra.get(m).copied().unwrap_or(0);
                        if st.passed.contains(m) || used >= budget || !st.live.contains(m) {
                            if used >= budget {
                                *st.extra.entry(m.clone()).or_default() += 1;
                            }
                            st.passed.remove(m);
                            st.pulled_in += 1;
                            if !st.live.contains(m) {
                                st.live.insert(m.clone());
                                st.joined.insert(m.clone());
                                start.push(m.clone());
                            }
                        }
                    }
                }
                for m in start {
                    tokio::spawn(worker(s.clone(), m, Some(ev.seq - 1)));
                }
                s.wake.notify_waiters();
            }
            Err(why) if why == "stopped" => {
                note_as(&s, &id, &format!("Stopped {}.", agent.name));
                break;
            }
            Err(why) => {
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
                } else if let Ok(e) = s.store.append(&s.turn.conversation, &id, "error", vec![], &format!("{} couldn't answer: {why}", agent.name)) {
                    s.host.event(&e);
                }
                failed = true;
                break;
            }
        }
    }
    (failed, last_seen)
}

async fn work(s: Arc<Shared>, id: String, mut last_seen: Option<u64>) {
    let agent = agents::by_id(&id).expect("known agent");
    let mut generation = s.state.lock().unwrap().generation;
    loop {
        let (failed, seen) = answer(s.clone(), id.clone(), last_seen).await;
        last_seen = seen;
        let stand_in = {
            let mut st = s.state.lock().unwrap();
            // The human said something new while this AI was finishing: stay and read it.
            if !failed && !s.cancel.is_cancelled() && st.generation != generation {
                generation = st.generation;
                continue;
            }
            st.live.remove(&id);
            st.waiting.remove(&id);
            if failed && st.live.is_empty() && !st.paused && !s.cancel.is_cancelled() {
                let now = now_ms();
                let pick = s
                    .turn
                    .members
                    .iter()
                    .filter(|m| !st.joined.contains(*m))
                    .filter_map(|m| agents::by_id(m))
                    .find(|a| s.bench.until(a.id, now).is_none() && s.host.installed(a));
                if let Some(a) = pick {
                    st.live.insert(a.id.to_string());
                    st.joined.insert(a.id.to_string());
                    Some(Some(a))
                } else if s.turn.members.iter().filter(|m| **m != id).filter_map(|m| agents::by_id(m)).any(|a| s.bench.until(a.id, now).is_none() && s.host.installed(a)) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    type Script = dyn Fn(&str, u32, &[Event]) -> Result<String, String> + Send + Sync;

    struct Fake {
        script: Box<Script>,
        /// How long each AI takes to answer, in ms.
        delay: HashMap<&'static str, u64>,
        asked: Mutex<Vec<(String, u32)>>,
        installed: HashSet<&'static str>,
    }

    impl Host for Fake {
        fn ask(&self, agent: &'static Agent, answer: u32, _rounds: u32, events: Vec<Event>, cancel: CancellationToken) -> Reply {
            self.asked.lock().unwrap().push((agent.id.to_string(), answer));
            let r = (self.script)(agent.id, answer, &events).map(|text| AgentReply { text, usage: None, model: None });
            let d = self.delay.get(agent.id).copied().unwrap_or(10);
            Box::pin(async move {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(d)) => r,
                    _ = cancel.cancelled() => Err("stopped".into()),
                }
            })
        }
        fn event(&self, _e: &Event) {}
        fn thinking(&self, _agents: Vec<String>, _round: u32) {}
        fn installed(&self, agent: &'static Agent) -> bool {
            self.installed.contains(agent.id)
        }
        fn remember(&self, _agent: &'static Agent, _reply: &str) -> Option<String> {
            None
        }
    }

    fn fake(script: impl Fn(&str, u32, &[Event]) -> Result<String, String> + Send + Sync + 'static) -> Fake {
        Fake {
            script: Box::new(script),
            delay: HashMap::new(),
            asked: Mutex::new(vec![]),
            installed: ["gpt", "claude", "gemini"].into_iter().collect(),
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
        let turn = Turn { conversation: c.into(), targets: ids(targets), rounds, members: ids(&["gpt", "claude", "gemini"]) };
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
        // Claude is asked first but is slower; ChatGPT answers without waiting for it. One after
        // the other would take 5 s; side by side, about 3 s.
        f.delay.insert("claude", 3000);
        f.delay.insert("gpt", 2000);
        let started = std::time::Instant::now();
        let ev = go(&store, &c, Arc::new(f), &["claude", "gpt"], 1, Arc::new(Bench::new())).await;
        assert!(started.elapsed() < Duration::from_millis(4500), "answers ran in parallel: {:?}", started.elapsed());
        assert_eq!(said(&ev), vec![("gpt".into(), "hi from gpt".into()), ("claude".into(), "hi from claude".into())]);
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
        assert!(council.join(&msg, &ids(&["gpt", "claude", "gemini"])));
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
            !council.join(&late, &ids(&["gpt"])),
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
}
