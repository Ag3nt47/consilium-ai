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
/// The same in a chat where the human turned on Auto-run: they chose not to step in each time.
pub const AUTO_RUN_DELAY_SECS: u64 = 5;
/// Automatic rounds allowed after each message from the human in an Auto-run chat.
pub const AUTO_RUN_MAX_ROUNDS: usize = 25;
/// While a command box holds rounds back, how often auto-continue looks again.
pub const RECHECK_SECS: u64 = 30;

/// The wait before the next automatic round.
pub fn delay_secs(auto_run: bool) -> u64 {
    if auto_run { AUTO_RUN_DELAY_SECS } else { DELAY_SECS }
}

#[derive(Debug, PartialEq)]
pub enum Decision {
    /// Start another round, posting this note.
    Continue(String),
    /// Stop, posting this note (once, when it pauses with work left).
    Pause(String),
    /// Stop quietly.
    Stop,
    /// Nothing to do yet (a command box is due to report back); look again in `RECHECK_SECS`.
    Recheck,
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

/// How Consilium words a run, or a Deny, of a command box when it reports back in the human's
/// name (`runcmd::report` and the Deny button). That isn't the human writing, so it doesn't
/// restart the round count.
pub const RAN: &str = "Ran command from ";
pub const DENIED: &str = "Denied command from ";
/// A box with no result after this long has timed out (`runcmd::TIMEOUT` is 10 minutes), been
/// skipped or been left for the human, so it stops holding rounds back.
pub const PENDING_MS: u64 = 12 * 60 * 1000;
/// Boxes older than this aren't run when Auto-run is turned on later; they wait for a click.
pub const CATCH_UP_MS: u64 = 30 * 60 * 1000;

fn is_human_message(e: &Event) -> bool {
    e.sender == "human" && e.kind == "message" && !e.text.starts_with(RAN) && !e.text.starts_with(DENIED)
}

/// Where the events since the human last wrote begin.
pub fn since_human_start(events: &[Event]) -> usize {
    events.iter().rposition(is_human_message).map_or(0, |i| i + 1)
}

/// Command boxes from AIs on this computer in `events[from..]`, under `max_age` ms old, that can
/// run here and have neither run nor been denied (a running one counts too): `(event id, index)`.
/// Boxes whose author has posted a later message are skipped (the author moved on or superseded them).
pub fn waiting_boxes(events: &[Event], from: usize, now: u64, max_age: u64) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    for (idx, e) in events.iter().enumerate().skip(from) {
        if e.kind != "message" || e.sender == "human" || e.sender == CONSILIUM || e.via.is_some() {
            continue;
        }
        if now.saturating_sub(e.ts) >= max_age {
            continue;
        }
        if events[idx + 1..].iter().any(|later| later.sender == e.sender && later.kind == "message") {
            continue;
        }
        for (i, b) in crate::runcmd::fenced_blocks(&e.text).iter().enumerate() {
            if b.closed && crate::runcmd::check(b).is_ok() && !crate::runcmd::handled(events, &e.id, i) {
                out.push((e.id.clone(), i));
            }
        }
    }
    out
}

/// Whether an AI's command box is running, or about to, and will report back by itself.
pub fn pending_boxes(events: &[Event], now: u64) -> bool {
    !waiting_boxes(events, 0, now, PENDING_MS).is_empty()
}

/// The old signature, kept for the tests; the engine calls `decide_with`.
#[cfg(test)]
pub fn decide(events: &[Event], board: &Board, auto_run: bool) -> Decision {
    decide_with(events, board, auto_run, false)
}

/// `decide`, knowing whether a command box is still due to report back (`pending_boxes`).
pub fn decide_with(events: &[Event], board: &Board, auto_run: bool, boxes_pending: bool) -> Decision {
    let open: Vec<_> = board.tasks.iter().filter(|t| matches!(t.status, Status::Todo | Status::Doing)).collect();
    let since_human = &events[since_human_start(events)..];
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
    // In an auto-run chat a box that is running, or about to, reports back by itself and starts
    // its AI's next turn; a round now would only have every AI say it's waiting.
    // If the box never reports (skipped, lost in a restart), the recheck carries on once it ages
    // out after PENDING_MS instead of leaving the chat stopped for good.
    if auto_run && boxes_pending {
        return Decision::Recheck;
    }
    let notes: Vec<&Event> = since_human.iter().filter(|e| is_auto_note(e) && !e.text.contains(AUTO_APPROVED)).collect();
    // A pause note is the last word until the human writes again.
    if notes.last().is_some_and(|n| n.text.starts_with(&format!("{PREFIX} paused"))) {
        return Decision::Stop;
    }
    let rounds: Vec<&&Event> = notes.iter().filter(|n| !n.text.starts_with(&format!("{PREFIX} paused"))).collect();
    let max_rounds = if auto_run { AUTO_RUN_MAX_ROUNDS } else { MAX_ROUNDS };
    if rounds.len() >= max_rounds {
        return Decision::Pause(format!(
            "{PREFIX} paused after {max_rounds} rounds on its own. {} task(s) are still open; send any message to carry on.",
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
        "{PREFIX} (round {} of {max_rounds}): {} task(s) still open: {list}. Carry on with the task you own, or take an \
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
    fn command_boxes_hold_rounds_back_only_while_they_are_due_to_report() {
        let events = vec![ev(1, 10, "human", "message", "go", vec![]), boxed(2, 20)];
        let b = board(Status::Doing, 15);
        let now = 20 + 1000;
        assert!(pending_boxes(&events, now));
        assert_eq!(decide_with(&events, &b, true, true), Decision::Recheck, "an auto-run chat waits for the box, and looks again");
        assert!(matches!(decide_with(&events, &b, false, true), Decision::Continue(_)), "a normal chat isn't held back");
        let mut done = events.clone();
        done.push(ran(3, 30, "e2"));
        assert!(!pending_boxes(&done, now), "a box with a result is finished");
        assert!(!pending_boxes(&events, 20 + PENDING_MS), "a box that never reported stops holding rounds back");
        let notes = vec![ev(1, 10, "claude", "message", "plan\n```text\nnot a command\n```", vec![])];
        assert!(!pending_boxes(&notes, 20), "a text block isn't a command");
    }

    #[test]
    fn command_results_and_denials_dont_restart_the_round_count() {
        let mut events = vec![ev(1, 10, "human", "message", "go", vec![])];
        for i in 0..MAX_ROUNDS as u64 {
            events.push(auto(2 + 2 * i, 20 + 2 * i));
            events.push(ran(3 + 2 * i, 21 + 2 * i, "x"));
        }
        events.push(ev(100, 100, "human", "message", &format!("{DENIED}Claude's message.\n(command ref x#1)"), vec![]));
        assert!(matches!(decide(&events, &board(Status::Doing, 1000), false), Decision::Pause(t) if t.contains("rounds on its own")));
    }

    #[test]
    fn catch_up_picks_only_waiting_boxes_since_the_human_wrote() {
        let events = vec![boxed(1, 5), ev(2, 10, "human", "message", "go", vec![]), boxed(3, 20)];
        let from = since_human_start(&events);
        assert_eq!(waiting_boxes(&events, from, 50, CATCH_UP_MS), vec![("e3".to_string(), 0)]);
        assert!(waiting_boxes(&events, from, 20 + CATCH_UP_MS, CATCH_UP_MS).is_empty(), "too old to run unasked");
    }

    #[test]
    fn waiting_boxes_skips_boxes_older_than_a_later_message_from_the_same_author() {
        let events = vec![
            boxed(1, 5),
            ev(2, 10, "claude", "message", "actually never mind", vec![]),
            boxed(3, 20),
        ];
        // Event 1 is skipped because claude posted a later message (event 2).
        // Event 3 is picked because claude posted no later message.
        assert_eq!(waiting_boxes(&events, 0, 50, CATCH_UP_MS), vec![("e3".to_string(), 0)]);
    }

    #[test]
    fn waiting_boxes_from_zero_picks_unhandled_boxes_across_human_messages() {
        let events = vec![
            boxed(1, 5),
            ev(2, 10, "human", "message", "Approved: run it", vec![]),
        ];
        // With from = 0, box 1 (e1#0) is picked because it was never handled and author didn't move on,
        // even though human wrote afterwards (e.g. approving a request).
        assert_eq!(waiting_boxes(&events, 0, 50, CATCH_UP_MS), vec![("e1".to_string(), 0)]);
    }

    const SHELL: &str = if cfg!(windows) { "powershell" } else { "bash" };

    fn boxed(seq: u64, ts: u64) -> Event {
        ev(seq, ts, "claude", "message", &format!("checking\n```{SHELL}\nls\n```"), vec![])
    }

    fn ran(seq: u64, ts: u64, of: &str) -> Event {
        let text = format!("{RAN}Claude's message (exit 0, succeeded, 1.0 s):\nok\n{}", crate::runcmd::reference(of, 0));
        ev(seq, ts, "human", "message", &text, vec![])
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
        assert!(matches!(decide(&capped, &board(Status::Doing, 1000), true), Decision::Continue(_)), "auto-run allows more rounds");
        let mut capped = vec![ev(1, 10, "human", "message", "go", vec![])];
        for i in 0..AUTO_RUN_MAX_ROUNDS as u64 {
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
