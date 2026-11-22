//! The conversation's shared task checklist.
//!
//! AIs say what they take, finish, hand off or are blocked on with `TASK:` lines in their
//! messages; the human edits the same list in the app. Consilium keeps the list in
//! `<conversation>.tasks.json` beside the conversation's log, posts a short note in the chat for
//! each change (who did what, and overall progress), and gives the list to every AI each answer,
//! so nobody has to scroll back to learn who is doing what.
//!
//! Linked computers share one board per conversation. The computer that owns the conversation
//! keeps it: TASK lines from another computer's AIs are applied there (as "<agent>-<computer>",
//! the same id as the AI's handle), and every change is sent back as a snapshot that replaces the
//! copy's board (see `peer_sync`). So a task number means the same task on every computer.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Longest task title kept, in characters.
const MAX_TITLE: usize = 140;
/// Most tasks a board holds; the oldest finished ones go first.
const MAX_TASKS: usize = 200;
/// Finished tasks shown to the AIs (the newest ones).
const DONE_IN_PROMPT: usize = 5;
const LOCAL_OPEN_IN_PROMPT: usize = 24;

static LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Todo,
    Doing,
    Blocked,
    Done,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Task {
    pub id: u32,
    pub title: String,
    /// Agent id, or "human"; none while nobody has taken it.
    #[serde(default)]
    pub owner: Option<String>,
    pub status: Status,
    /// Why it's blocked, if it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Who changed it last, and when (ms since the epoch).
    pub by: String,
    pub updated: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Board {
    pub tasks: Vec<Task>,
    #[serde(default)]
    pub next: u32,
}

/// A change to the board, from a `TASK:` line or the app.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum Op {
    /// A new task nobody has taken yet (or `owner`'s, from the app).
    Add {
        title: String,
        #[serde(default)]
        owner: Option<String>,
    },
    /// Take a task and start it; a title that matches nothing becomes a new task.
    Take {
        task: String,
    },
    Done {
        task: String,
    },
    Block {
        task: String,
        #[serde(default)]
        note: Option<String>,
    },
    /// Give a task to someone else, who then has it in progress.
    Hand {
        task: String,
        to: String,
    },
    /// Back to not started, or not finished.
    Reopen {
        task: String,
    },
    Drop {
        task: String,
    },
}

pub fn path(conversations: &Path, conversation: &str) -> PathBuf {
    conversations.join(format!("{conversation}.tasks.json"))
}

pub fn load(conversations: &Path, conversation: &str) -> Board {
    std::fs::read_to_string(path(conversations, conversation))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Replaces the conversation's board with `board` (a snapshot from the computer that owns it).
/// Returns whether anything changed.
pub fn replace(conversations: &Path, conversation: &str, board: &Board) -> Result<bool, String> {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if load(conversations, conversation) == *board {
        return Ok(false);
    }
    save(conversations, conversation, board)?;
    Ok(true)
}

/// Owner and author ids as seen from this computer, for a board kept on the computer `from`
/// (its handle slug, e.g. "msi-agent47"): that computer's own AIs ("claude") become
/// "claude-msi-agent47", and this computer's AIs ("claude-lukes-mbp") become "claude".
/// With `from` None only the second step runs, for ids written on this computer.
pub fn localize(board: &mut Board, here: &str, from: Option<&str>) {
    let fix = |id: &str| -> String {
        if let Some(from) = from {
            if crate::agents::by_id(id).is_some() {
                return format!("{id}-{from}");
            }
        }
        match split_remote(id) {
            Some((agent, computer)) if !here.is_empty() && computer == here => agent.to_string(),
            _ => id.to_string(),
        }
    };
    for t in &mut board.tasks {
        t.owner = t.owner.as_deref().map(fix);
        t.by = fix(&t.by);
    }
}

/// "claude-lukes-mbp" -> ("claude", "lukes-mbp"), for a known AI on another computer.
fn split_remote(id: &str) -> Option<(&str, &str)> {
    id.match_indices('-').find_map(|(i, _)| {
        let (agent, rest) = (&id[..i], &id[i + 1..]);
        (crate::agents::by_id(agent).is_some() && !rest.is_empty()).then_some((agent, rest))
    })
}

fn save(conversations: &Path, conversation: &str, board: &Board) -> Result<(), String> {
    let p = path(conversations, conversation);
    let tmp = p.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(board).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
    crate::store::replace_file(&tmp, &p).map_err(|e| e.to_string())
}

/// Applies `ops` by `who` to the conversation's board and saves it. Returns the note to post in
/// the chat, or none if nothing changed.
pub fn apply(
    conversations: &Path,
    conversation: &str,
    who: &str,
    ops: &[Op],
) -> Result<Option<String>, String> {
    if ops.is_empty() {
        return Ok(None);
    }
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut board = load(conversations, conversation);
    let now = crate::store::now_ms();
    let said: Vec<String> = ops
        .iter()
        .filter_map(|op| board.apply(who, op, now))
        .collect();
    if said.is_empty() {
        return Ok(None);
    }
    board.trim();
    // A hand-off to "@claude-<this computer>" means this computer's own Claude.
    localize(&mut board, &crate::prompt::here_slug(), None);
    save(conversations, conversation, &board)?;
    Ok(Some(format!(
        "Tasks: {} {}. {}",
        name(who),
        said.join("; "),
        board.progress()
    )))
}

impl Board {
    fn find(&self, reference: &str) -> Option<usize> {
        let r = reference.trim().trim_end_matches('.');
        let digits = r.strip_prefix('#').unwrap_or(r);
        if let Ok(n) = digits.parse::<u32>() {
            return self.tasks.iter().position(|t| t.id == n);
        }
        let lower = r.to_lowercase();
        if let Some(i) = self
            .tasks
            .iter()
            .position(|t| t.title.to_lowercase() == lower)
        {
            return Some(i);
        }
        // A shortened title works when it fits exactly one task.
        if lower.chars().count() >= 6 {
            let hits: Vec<usize> = self
                .tasks
                .iter()
                .enumerate()
                .filter(|(_, t)| t.title.to_lowercase().contains(&lower))
                .map(|(i, _)| i)
                .collect();
            if hits.len() == 1 {
                return Some(hits[0]);
            }
        }
        None
    }

    fn add(
        &mut self,
        title: &str,
        owner: Option<String>,
        status: Status,
        who: &str,
        now: u64,
    ) -> Option<usize> {
        let title = clean(title)?;
        self.next = self
            .next
            .max(self.tasks.iter().map(|t| t.id).max().unwrap_or(0))
            + 1;
        self.tasks.push(Task {
            id: self.next,
            title,
            owner,
            status,
            note: None,
            by: who.to_string(),
            updated: now,
        });
        Some(self.tasks.len() - 1)
    }

    /// Finds the task, or adds it with `status` when the reference is a title rather than a number.
    fn find_or_add(
        &mut self,
        reference: &str,
        status: Status,
        who: &str,
        now: u64,
    ) -> Option<(usize, bool)> {
        if let Some(i) = self.find(reference) {
            return Some((i, false));
        }
        if reference
            .trim()
            .trim_start_matches('#')
            .parse::<u32>()
            .is_ok()
        {
            return None;
        }
        self.add(reference, Some(who.to_string()), status, who, now)
            .map(|i| (i, true))
    }

    /// Applies one change; returns what happened, worded for the chat note.
    fn apply(&mut self, who: &str, op: &Op, now: u64) -> Option<String> {
        let (i, verb) = match op {
            Op::Add { title, owner } => {
                let owner = owner.clone().filter(|o| !o.is_empty());
                let status = if owner.is_some() {
                    Status::Doing
                } else {
                    Status::Todo
                };
                (self.add(title, owner, status, who, now)?, "added")
            }
            Op::Take { task } => {
                let (i, new) = self.find_or_add(task, Status::Doing, who, now)?;
                let t = &mut self.tasks[i];
                if !new {
                    let owner = t.owner.as_deref();
                    let already_doing = t.status == Status::Doing && owner == Some(who);
                    let assigned_elsewhere = who != "human"
                        && matches!(t.status, Status::Doing | Status::Blocked)
                        && owner.is_some_and(|owner| owner != who);
                    if already_doing || assigned_elsewhere {
                        return None;
                    }
                }
                t.owner = Some(who.to_string());
                t.status = Status::Doing;
                t.note = None;
                (i, "took")
            }
            Op::Done { task } => {
                let (i, new) = self.find_or_add(task, Status::Done, who, now)?;
                let t = &mut self.tasks[i];
                if !new && t.status == Status::Done {
                    return None;
                }
                t.status = Status::Done;
                t.note = None;
                t.owner.get_or_insert_with(|| who.to_string());
                (i, "finished")
            }
            Op::Block { task, note } => {
                let (i, _) = self.find_or_add(task, Status::Blocked, who, now)?;
                let t = &mut self.tasks[i];
                t.status = Status::Blocked;
                t.note = note.as_deref().and_then(clean);
                t.owner.get_or_insert_with(|| who.to_string());
                (i, "is blocked on")
            }
            Op::Hand { task, to } => {
                let i = self.find(task)?;
                let to = to.trim().trim_start_matches('@').to_lowercase();
                if to.is_empty() {
                    return None;
                }
                let t = &mut self.tasks[i];
                if who != "human" && t.owner.as_deref().is_some_and(|owner| owner != who) {
                    return None;
                }
                t.owner = Some(to.clone());
                t.status = Status::Doing;
                t.note = None;
                t.by = who.to_string();
                t.updated = now;
                return Some(format!("handed #{} {} to {}", t.id, t.title, name(&to)));
            }
            Op::Reopen { task } => {
                let i = self.find(task)?;
                let t = &mut self.tasks[i];
                t.status = if t.owner.is_some() {
                    Status::Doing
                } else {
                    Status::Todo
                };
                t.note = None;
                (i, "reopened")
            }
            Op::Drop { task } => {
                let i = self.find(task)?;
                let t = self.tasks.remove(i);
                return Some(format!("removed #{} {}", t.id, t.title));
            }
        };
        let t = &mut self.tasks[i];
        t.by = who.to_string();
        t.updated = now;
        let reason = t
            .note
            .as_deref()
            .map(|n| format!(" ({n})"))
            .unwrap_or_default();
        Some(format!("{verb} #{} {}{reason}", t.id, t.title))
    }

    /// Keeps the board under MAX_TASKS by dropping the oldest finished tasks.
    fn trim(&mut self) {
        while self.tasks.len() > MAX_TASKS {
            let oldest = self
                .tasks
                .iter()
                .enumerate()
                .filter(|(_, t)| t.status == Status::Done)
                .min_by_key(|(_, t)| t.updated)
                .map(|(i, _)| i)
                .unwrap_or(0);
            self.tasks.remove(oldest);
        }
    }

    fn count(&self, s: Status) -> usize {
        self.tasks.iter().filter(|t| t.status == s).count()
    }

    /// "Progress: 3 of 7 done (43%), 2 in progress, 1 blocked."
    pub fn progress(&self) -> String {
        let total = self.tasks.len();
        if total == 0 {
            return "The task list is empty.".into();
        }
        let done = self.count(Status::Done);
        let mut s = format!("Progress: {done} of {total} done ({}%)", done * 100 / total);
        let doing = self.count(Status::Doing);
        if doing > 0 {
            s.push_str(&format!(", {doing} in progress"));
        }
        let blocked = self.count(Status::Blocked);
        if blocked > 0 {
            s.push_str(&format!(", {blocked} blocked"));
        }
        s.push('.');
        s
    }
}

fn clean(text: &str) -> Option<String> {
    let t = crate::redact::scrub(text.trim().trim_matches('`'));
    let t: String = t.split_whitespace().collect::<Vec<_>>().join(" ");
    (!t.is_empty()).then(|| t.chars().take(MAX_TITLE).collect())
}

fn name(id: &str) -> String {
    match id {
        "human" => "The human".into(),
        "consilium" => "Consilium".into(),
        _ => match (crate::agents::by_id(id), split_remote(id)) {
            (Some(a), _) => a.name.to_string(),
            (None, Some((agent, computer))) => format!("{} ({computer})", name(agent)),
            _ => id.to_string(),
        },
    }
}

/// The changes asked for on an answer's `TASK:` lines.
///
/// `TASK: take <title or #n>` · `TASK: done <title or #n>` · `TASK: blocked <#n> - <why>` ·
/// `TASK: hand <#n> to @name` · `TASK: add <title>` · `TASK: reopen <#n>` · `TASK: drop <#n>`.
/// Lines inside fenced code blocks are ignored, so examples don't change the board.
pub fn task_lines(text: &str) -> Vec<Op> {
    let mut ops = Vec::new();
    let mut fenced = false;
    for line in text.lines() {
        let t = line.trim_start();
        if t.starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced || !t.get(..5).is_some_and(|prefix| prefix.eq_ignore_ascii_case("TASK:")) {
            continue;
        }
        let rest = t[5..].trim();
        let (verb, arg) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
        let arg = arg.trim();
        if arg.is_empty() {
            continue;
        }
        let task = arg.to_string();
        let op = match verb.to_lowercase().trim_end_matches(':') {
            "take" | "taking" | "took" | "claim" | "start" | "starting" | "doing" => {
                Op::Take { task }
            }
            "done" | "did" | "finished" | "finish" | "complete" | "completed" => Op::Done { task },
            "block" | "blocked" => {
                let (task, note) = split_reason(arg);
                Op::Block { task, note }
            }
            "hand" | "handoff" | "give" => match arg.rsplit_once(" to ") {
                Some((task, to)) => Op::Hand {
                    task: task.trim().into(),
                    to: to.trim().into(),
                },
                None => continue,
            },
            "add" | "todo" | "new" => Op::Add {
                title: task,
                owner: None,
            },
            "reopen" | "undo" => Op::Reopen { task },
            "drop" | "remove" | "cancel" => Op::Drop { task },
            _ => continue,
        };
        ops.push(op);
    }
    ops
}

/// The last `TITLE: <new title>` line outside code blocks: an AI renaming the conversation to fit
/// the project.
pub fn title_line(text: &str) -> Option<String> {
    let mut fenced = false;
    let mut found = None;
    for line in text.lines() {
        let t = line.trim_start();
        if t.starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if !fenced && t.len() > 6 && t[..6].eq_ignore_ascii_case("TITLE:") {
            let title = t[6..].trim().trim_matches(|c| c == '"' || c == '`' || c == '*').trim();
            if !title.is_empty() {
                found = Some(title.to_string());
            }
        }
    }
    found
}

fn split_reason(arg: &str) -> (String, Option<String>) {
    for sep in [" — ", " -- ", " - ", ": ", " because "] {
        if let Some((task, why)) = arg.split_once(sep) {
            return (task.trim().into(), Some(why.trim().into()));
        }
    }
    (arg.into(), None)
}

/// The board and how to change it, for an AI's instructions.
pub fn for_prompt(board: &Board, local_agent: Option<&str>) -> String {
    let mut s = String::from(
        "\n\nTASK BOARD (shared across computers; editable by the human). \
Before claiming a large task, divide it into independent tasks with affected files and a verification step. Check ownership, claim once, coordinate shared edits through a handoff, and mark done after commit and checks. \
Put each action on its own line: `TASK: take <title or #n>` (adds a new title), `TASK: done <title or #n>`, \
`TASK: blocked <#n> - <why>`, `TASK: hand <#n> to @name`, `TASK: add <title>`, `TASK: drop <#n>`. \
Task numbers are shared. An owner shown as \"<AI> (<computer>)\" is remote. \
Rename the conversation with `TITLE: <short new title>` (max 60 characters).",
    );
    if board.tasks.is_empty() {
        s.push_str("\nThe board is empty.");
        return s;
    }
    let mut open: Vec<&Task> = board
        .tasks
        .iter()
        .filter(|t| t.status != Status::Done)
        .collect();
    let omitted_open = if let Some(agent) = local_agent {
        open.sort_by_key(|task| {
            let ownership = match task.owner.as_deref() {
                Some(owner) if owner == agent => 0,
                None => 1,
                Some(_) => 2,
            };
            (ownership, task.status == Status::Blocked)
        });
        let omitted = open.len().saturating_sub(LOCAL_OPEN_IN_PROMPT);
        open.truncate(LOCAL_OPEN_IN_PROMPT);
        omitted
    } else {
        0
    };
    let mut done: Vec<&Task> = board
        .tasks
        .iter()
        .filter(|t| t.status == Status::Done)
        .collect();
    done.sort_by_key(|t| std::cmp::Reverse(t.updated));
    for t in open.iter().chain(done.iter().take(DONE_IN_PROMPT)) {
        let state = match t.status {
            Status::Todo => "open".to_string(),
            Status::Doing => format!(
                "doing · {}",
                t.owner.as_deref().map(name).unwrap_or_default()
            ),
            Status::Blocked => format!(
                "blocked · {}",
                t.owner.as_deref().map(name).unwrap_or_default()
            ),
            Status::Done => format!(
                "done · {}",
                t.owner.as_deref().map(name).unwrap_or_default()
            ),
        };
        let note = t
            .note
            .as_deref()
            .map(|note| {
                if local_agent.is_none() {
                    return format!(" ({note})");
                }
                const LOCAL_NOTE_CHARS: usize = 160;
                let mut chars = note.chars();
                let excerpt: String = chars.by_ref().take(LOCAL_NOTE_CHARS).collect();
                let suffix = if chars.next().is_some() { "..." } else { "" };
                format!(" ({excerpt}{suffix})")
            })
            .unwrap_or_default();
        s.push_str(&format!("\n#{} [{state}] {}{note}", t.id, t.title));
    }
    if omitted_open > 0 {
        s.push_str(&format!("\n({omitted_open} open tasks omitted. The task board in the app shows every task.)"));
    }
    if done.len() > DONE_IN_PROMPT {
        s.push_str(&format!(
            "\n({} older finished tasks not shown)",
            done.len() - DONE_IN_PROMPT
        ));
    }
    s.push('\n');
    s.push_str(&board.progress());
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "consilium-tasks-{}-{}",
            std::process::id(),
            crate::store::now_ms()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn local_task_prompt_keeps_owned_work_and_limits_open_rows() {
        let mut board = Board::default();
        for id in 1..=30 {
            board.tasks.push(Task {
                id,
                title: format!("Assigned task {id}"),
                owner: Some(if id == 30 { "local" } else { "other" }.to_string()),
                status: Status::Doing,
                note: None,
                by: "human".into(),
                updated: 0,
            });
        }
        board.tasks[0].status = Status::Blocked;
        let prefix = "\u{00e9}".repeat(160);
        board.tasks[29].note = Some(format!("{prefix}remaining details"));
        let local = for_prompt(&board, Some("local"));
        assert_eq!(local.lines().filter(|line| line.starts_with('#')).count(), LOCAL_OPEN_IN_PROMPT);
        assert!(local.contains("#30 [doing"));
        assert!(!local.contains("#1 [blocked"));
        assert!(!local.contains("#29 [doing"));
        assert!(local.contains("6 open tasks omitted"));
        assert!(local.contains(&format!("{prefix}...")));
        assert!(!local.contains("remaining details"));
        let hosted = for_prompt(&board, None);
        assert!(hosted.contains("remaining details"));
        assert_eq!(hosted.lines().filter(|line| line.starts_with('#')).count(), 30);
    }

    #[test]
    fn owners_are_named_as_seen_from_each_computer() {
        let mut b = Board::default();
        let now = 1;
        b.apply("claude", &Op::Take { task: "Fix helper".into() }, now);
        b.apply("gemini-lukes-mbp", &Op::Take { task: "Shared board".into() }, now);
        b.apply("claude", &Op::Hand { task: "#1".into(), to: "@gpt-lukes-mbp".into() }, now);
        let mut copy = b.clone();
        localize(&mut copy, "lukes-mbp", Some("msi-agent47"));
        let owners: Vec<_> = copy.tasks.iter().map(|t| t.owner.clone().unwrap()).collect();
        assert_eq!(owners, vec!["gpt", "gemini"]);
        assert_eq!(copy.tasks[0].by, "claude-msi-agent47");
        assert_eq!(name("gemini-lukes-mbp"), "Gemini (lukes-mbp)");
    }

    #[test]
    fn reads_task_lines_and_skips_code_blocks() {
        let ops = task_lines(
            "正在处理。\nTASK: take Signal relay\ntask: done #2\nTASK: blocked #3 - needs disk space\nTASK: hand #4 to @gemini\n```text\nTASK: take example\n```\nTASK: nonsense here",
        );
        assert_eq!(
            ops,
            vec![
                Op::Take {
                    task: "Signal relay".into()
                },
                Op::Done { task: "#2".into() },
                Op::Block {
                    task: "#3".into(),
                    note: Some("needs disk space".into())
                },
                Op::Hand {
                    task: "#4".into(),
                    to: "@gemini".into()
                },
            ]
        );
    }

    #[test]
    fn a_second_agent_cannot_take_an_owned_task_without_a_handoff() {
        let mut board = Board::default();
        let take = Op::Take { task: "Review map".into() };
        assert!(board.apply("claude", &take, 1).is_some());
        assert!(board.apply("gemini", &take, 2).is_none());
        assert!(board.apply("claude", &Op::Block { task: "#1".into(), note: Some("Waiting for data".into()) }, 3).is_some());
        assert!(board.apply("gemini", &take, 4).is_none());
        assert_eq!(board.tasks[0].owner.as_deref(), Some("claude"));
        let handoff = Op::Hand { task: "#1".into(), to: "gemini".into() };
        assert!(board.apply("gemini", &handoff, 5).is_none());
        assert!(board.apply("claude", &handoff, 6).is_some());
        assert_eq!(board.tasks[0].owner.as_deref(), Some("gemini"));
        assert_eq!(board.tasks[0].status, Status::Doing);
    }

    #[test]
    fn records_who_does_what_and_progress() {
        let d = dir();
        let note = apply(
            &d,
            "c1",
            "claude",
            &task_lines("TASK: take Task board\nTASK: add Review link fixes"),
        )
        .unwrap()
        .unwrap();
        assert!(note.contains("Claude took #1 Task board"), "{note}");
        assert!(note.contains("added #2 Review link fixes"), "{note}");
        assert!(note.contains("0 of 2 done (0%), 1 in progress"), "{note}");

        // Taking it again changes nothing, so no note.
        assert_eq!(
            apply(&d, "c1", "claude", &[Op::Take { task: "#1".into() }]).unwrap(),
            None
        );

        let note = apply(&d, "c1", "gemini", &task_lines("TASK: take review link"))
            .unwrap()
            .unwrap();
        assert!(note.contains("Gemini took #2"), "{note}");
        let note = apply(&d, "c1", "claude", &task_lines("TASK: done task board"))
            .unwrap()
            .unwrap();
        assert!(note.contains("1 of 2 done (50%)"), "{note}");

        let note = apply(
            &d,
            "c1",
            "human",
            &[Op::Hand {
                task: "#2".into(),
                to: "claude".into(),
            }],
        )
        .unwrap()
        .unwrap();
        assert!(
            note.contains("The human handed #2 Review link fixes to Claude"),
            "{note}"
        );

        let board = load(&d, "c1");
        assert_eq!(board.tasks[1].owner.as_deref(), Some("claude"));
        let p = for_prompt(&board, None);
        assert!(p.contains("#2 [doing · Claude] Review link fixes"), "{p}");
        assert!(p.contains("#1 [done · Claude] Task board"), "{p}");

        // An unknown number isn't invented.
        assert_eq!(
            apply(&d, "c1", "claude", &[Op::Done { task: "#9".into() }]).unwrap(),
            None
        );
        let note = apply(&d, "c1", "human", &[Op::Drop { task: "#1".into() }])
            .unwrap()
            .unwrap();
        assert!(note.contains("removed #1"), "{note}");
        assert_eq!(title_line("ok\nTITLE: \"Consilium 0.3.21\"\n"), Some("Consilium 0.3.21".into()));
        assert_eq!(title_line("```\nTITLE: example\n```"), None);
        let _ = std::fs::remove_dir_all(d);
    }
}
