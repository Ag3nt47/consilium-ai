//! Auto-continue: when a turn ends while the conversation's task board still has work on it,
//! Consilium starts another round on its own, so the AIs carry on without the human typing
//! "continue". It stops on its own too:
//!
//! - when no task is left to do (every task is done, or the rest are blocked);
//! - when an AI has asked the human something (a `REQUEST:` line) that the human hasn't answered,
//!   except in an auto-run chat, where the request is approved on the human's behalf after
//!   `DELAY_SECS` (time to click Deny first) and work goes on, every time, with no limit;
//! - after `MAX_ROUNDS` automatic rounds since the human's last message;
//! - when `IDLE_ROUNDS` automatic rounds in a row changed nothing on the board, which is what
//!   waiting on a build, or every AI being out of usage, looks like. Not in a chat where the human
//!   turned on "Auto-run this chat" (`auto_run`): there commands report back by themselves, so
//!   quiet rounds keep going and only the other stops apply.
//!
//! The decision is worked out from the conversation and its board alone, so it survives restarts.
//! Any message from the human starts the count again; the human's Stop button never continues.

use crate::council::CONSILIUM;
use crate::store::Event;
use crate::tasks::{Board, Status};

/// How Consilium's own auto-continue notes begin; also how they are counted.
pub const PREFIX: &str = "Auto-continue";
/// Automatic rounds allowed after each message from the human.
pub const MAX_ROUNDS: usize = 8;
/// Automatic rounds in a row that changed nothing on the task board before it pauses.
pub const IDLE_ROUNDS: usize = 2;
/// How long after a turn ends before the next round starts, so the human can step in first.
pub const DELAY_SECS: u64 = 20;

#[derive(Debug, PartialEq)]
pub enum Decision {
    /// Start another round, posting this note.
    Continue(String),
    /// Stop, posting this note (once, when it pauses with work left).
    Pause(String),
    /// Stop quietly.
    Stop,
}

fn is_auto_note(e: &Event) -> bool {
    e.sender == CONSILIUM && e.text.starts_with(PREFIX)
}

/// How an approval given on the human's behalf is written in an auto-continue note, one per line:
/// the same "Approved: <request>" the Approve button sends, so the AIs and the screen read it alike.
const APPROVED: &str = "Approved: ";
const AUTO_APPROVED: &str = "auto-approved because Auto-run this chat is on";

/// Requests from AIs since the human's last message that nobody has answered yet, oldest first.
fn unanswered(since_human: &[Event]) -> Vec<String> {
    let mut asked: Vec<String> = Vec::new();
    for e in since_human {
        if e.sender == CONSILIUM {
            if is_auto_note(e) {
                for line in e.text.lines() {
                    if let Some(r) = line.trim().strip_prefix(APPROVED) {
                        if let Some(i) = asked.iter().position(|a| a.trim() == r.trim()) {
                            asked.remove(i);
                        }
                    }
                }
            }
        } else if e.sender != "human" {
            asked.extend(e.requests.iter().cloned());
        }
    }
    asked
}

pub fn decide(events: &[Event], board: &Board, auto_run: bool) -> Decision {
    let open: Vec<_> = board.tasks.iter().filter(|t| matches!(t.status, Status::Todo | Status::Doing)).collect();
    let since_human = events
        .iter()
        .rposition(|e| e.sender == "human" && e.kind == "message")
        .map_or(events, |i| &events[i + 1..]);
    let waiting = unanswered(since_human);
    if open.is_empty() && (waiting.is_empty() || !auto_run) {
        return Decision::Stop;
    }
    if !waiting.is_empty() && !auto_run {
        return Decision::Stop;
    }
    // In an auto-run chat every waiting request is approved, every time, however many rounds have
    // run; approvals don't count as rounds.
    if !waiting.is_empty() {
        let approved = waiting.iter().map(|r| format!("{APPROVED}{}", r.trim())).collect::<Vec<_>>().join("\n");
        return Decision::Continue(format!("{PREFIX}: {AUTO_APPROVED}:\n{approved}\nGo ahead with what you asked."));
    }
    let notes: Vec<&Event> = since_human.iter().filter(|e| is_auto_note(e) && !e.text.contains(AUTO_APPROVED)).collect();
    // A pause note is the last word until the human writes again.
    if notes.last().is_some_and(|n| n.text.starts_with(&format!("{PREFIX} paused"))) {
        return Decision::Stop;
    }
    let rounds: Vec<&&Event> = notes.iter().filter(|n| !n.text.starts_with(&format!("{PREFIX} paused"))).collect();
    if rounds.len() >= MAX_ROUNDS {
        return Decision::Pause(format!(
            "{PREFIX} paused after {MAX_ROUNDS} rounds on its own. {} task(s) are still open; send any message to carry on.",
            open.len()
        ));
    }
    // Rounds that changed nothing: count back from the newest note while no task changed after it.
    let last_change = board.tasks.iter().map(|t| t.updated).max().unwrap_or(0);
    let idle = rounds.iter().rev().take_while(|n| last_change < n.ts).count();
    if !auto_run && idle >= IDLE_ROUNDS {
        return Decision::Pause(format!(
            "{PREFIX} paused: the last {IDLE_ROUNDS} rounds changed nothing on the task board, so the AIs are \
             probably waiting on a build, a reset or you. {} task(s) are still open; send any message to carry on.",
            open.len()
        ));
    }
    let list = open.iter().map(|t| format!("#{} {}", t.id, t.title)).collect::<Vec<_>>().join("; ");
    Decision::Continue(format!(
        "{PREFIX} (round {} of {MAX_ROUNDS}): {} task(s) still open: {list}. Carry on with the task you own, or take an \
         open one nobody owns. Don't repeat work another AI reports done. If you are only waiting on something \
         (a build, GitHub's checks, a reset), say so in one line and change nothing.",
        rounds.len() + 1,
        open.len()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::Task;

    fn ev(seq: u64, ts: u64, sender: &str, kind: &str, text: &str, requests: Vec<String>) -> Event {
        let mut e: Event = serde_json::from_value(serde_json::json!({
            "id": format!("e{seq}"), "seq": seq, "ts": ts, "conversation": "c",
            "sender": sender, "kind": kind, "to": [], "text": text,
        }))
        .unwrap();
        e.requests = requests;
        e
    }

    fn board(status: Status, updated: u64) -> Board {
        Board {
            tasks: vec![Task {
                id: 1,
                title: "Sync".into(),
                owner: Some("claude".into()),
                status,
                note: None,
                by: "claude".into(),
                updated,
            }],
            next: 2,
        }
    }

    fn auto(seq: u64, ts: u64) -> Event {
        ev(seq, ts, CONSILIUM, "system", &format!("{PREFIX} (round x)"), vec![])
    }

    #[test]
    fn continues_while_work_is_open_and_stops_when_it_is_done() {
        let events = vec![ev(1, 10, "human", "message", "go", vec![]), ev(2, 20, "claude", "message", "did it", vec![])];
        assert!(matches!(decide(&events, &board(Status::Doing, 15), false), Decision::Continue(t) if t.contains("#1 Sync")));
        assert_eq!(decide(&events, &board(Status::Done, 15), false), Decision::Stop);
        assert_eq!(decide(&events, &board(Status::Blocked, 15), false), Decision::Stop);
    }

    #[test]
    fn waits_for_an_unanswered_request() {
        let events = vec![
            ev(1, 10, "human", "message", "go", vec![]),
            ev(2, 20, "claude", "message", "REQUEST: ok?", vec!["ok?".into()]),
        ];
        assert_eq!(decide(&events, &board(Status::Doing, 15), false), Decision::Stop);
    }

    #[test]
    fn pauses_once_after_idle_rounds_and_restarts_after_the_human_writes() {
        let mut events = vec![ev(1, 10, "human", "message", "go", vec![]), auto(2, 30), auto(3, 40)];
        // The task last changed before both automatic rounds began.
        let b = board(Status::Doing, 20);
        let Decision::Pause(note) = decide(&events, &b, false) else { panic!("should pause") };
        events.push(ev(4, 50, CONSILIUM, "system", &note, vec![]));
        assert_eq!(decide(&events, &b, false), Decision::Stop);
        events.push(ev(5, 60, "human", "message", "continue", vec![]));
        assert!(matches!(decide(&events, &b, false), Decision::Continue(_)));
        // A task changing during a round counts as progress.
        let busy = vec![ev(1, 10, "human", "message", "go", vec![]), auto(2, 30), auto(3, 40)];
        assert!(matches!(decide(&busy, &board(Status::Doing, 45), false), Decision::Continue(_)));
    }

    #[test]
    fn caps_the_rounds_after_each_human_message() {
        let mut events = vec![ev(1, 10, "human", "message", "go", vec![])];
        for i in 0..MAX_ROUNDS as u64 {
            events.push(auto(2 + i, 20 + i));
        }
        assert!(matches!(decide(&events, &board(Status::Doing, 1000), false), Decision::Pause(t) if t.contains("rounds on its own")));
    }

    #[test]
    fn auto_run_chats_keep_going_through_quiet_rounds_but_keep_the_other_stops() {
        let events = vec![ev(1, 10, "human", "message", "go", vec![]), auto(2, 30), auto(3, 40)];
        let b = board(Status::Doing, 20);
        assert!(matches!(decide(&events, &b, false), Decision::Pause(_)), "a normal chat pauses");
        assert!(matches!(decide(&events, &b, true), Decision::Continue(_)), "an auto-run chat keeps going");
        let mut capped = vec![ev(1, 10, "human", "message", "go", vec![])];
        for i in 0..MAX_ROUNDS as u64 {
            capped.push(auto(2 + i, 20 + i));
        }
        assert!(matches!(decide(&capped, &board(Status::Doing, 1000), true), Decision::Pause(t) if t.contains("rounds on its own")));
        let asked = vec![
            ev(1, 10, "human", "message", "go", vec![]),
            ev(2, 20, "claude", "message", "REQUEST: ok?", vec!["ok?".into()]),
        ];
        let Decision::Continue(note) = decide(&asked, &b, true) else { panic!("an auto-run chat approves it") };
        assert!(note.contains("auto-approved") && note.lines().any(|l| l == "Approved: ok?"), "{note}");
    }

    #[test]
    fn auto_run_chats_approve_each_request_once_and_the_human_can_deny_first() {
        let empty = Board::default();
        let mut events = vec![
            ev(1, 10, "human", "message", "go", vec![]),
            ev(2, 20, "claude", "message", "REQUEST: push to main?", vec!["push to main?".into()]),
        ];
        assert_eq!(decide(&events, &empty, false), Decision::Stop, "a normal chat waits for the human");
        let Decision::Continue(note) = decide(&events, &empty, true) else { panic!("approved even with no tasks") };
        events.push(ev(3, 30, CONSILIUM, "system", &note, vec![]));
        assert_eq!(decide(&events, &empty, true), Decision::Stop, "approved once; nothing else to do");
        // The human answers first (Deny): nothing is approved for them.
        let denied = vec![
            ev(1, 10, "human", "message", "go", vec![]),
            ev(2, 20, "claude", "message", "REQUEST: push to main?", vec!["push to main?".into()]),
            ev(3, 25, "human", "message", "Denied: push to main?", vec![]),
        ];
        assert_eq!(decide(&denied, &empty, true), Decision::Stop);
        // Every request is approved, however many rounds have run.
        let mut many = vec![ev(1, 10, "human", "message", "go", vec![])];
        for i in 0..MAX_ROUNDS as u64 {
            many.push(auto(2 + i, 20 + i));
        }
        many.push(ev(99, 99, "claude", "message", "REQUEST: again?", vec!["again?".into()]));
        assert!(matches!(decide(&many, &empty, true), Decision::Continue(t) if t.contains("Approved: again?")));
    }
}
