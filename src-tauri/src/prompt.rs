//! What each agent is told: who is in the room, how to address the human, and the transcript.

use crate::agents::AGENTS;
use crate::config::Access;
use crate::store::Event;
use std::hash::{DefaultHasher, Hasher};
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Once a transcript passes this budget, older turns become a compact digest and the newest
/// portion remains verbatim. Keeping both inside one budget prevents long sessions from crowding
/// out the current task while preserving decisions and requests from earlier turns.
pub const MAX_TRANSCRIPT_CHARS: usize = 120_000;
const MAX_TRANSCRIPT_SUMMARY_CHARS: usize = 16_000;
const RECENT_TRANSCRIPT_CHARS: usize = MAX_TRANSCRIPT_CHARS - MAX_TRANSCRIPT_SUMMARY_CHARS;
const MAX_SUMMARY_ENTRY_CHARS: usize = 420;
/// Enough patch context to identify concurrent edits without spending the whole prompt on a diff.
const MAX_WORKSPACE_DIFF_BYTES: usize = 12_000;
const MAX_WORKSPACE_STATUS_BYTES: usize = 8_000;
const GIT_COMMAND_TIMEOUT: Duration = Duration::from_millis(1_500);

type TranscriptPart<'a> = (&'a Event, String);

struct GitOutput {
    shown: String,
    digest: u64,
    truncated: bool,
}

/// Runs a read-only git command without a shell, hashes its complete output, and retains only the
/// bounded prefix needed for the prompt. A reader thread keeps the pipe drained; the parent polls
/// the child and kills it at the deadline so a stuck Git process cannot survive an async timeout.
fn git_output(workspace: &Path, args: &[&str], keep: usize) -> Option<GitOutput> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut hasher = DefaultHasher::new();
        let mut shown = Vec::with_capacity(keep.min(16 * 1024));
        let mut total = 0usize;
        let mut buf = [0u8; 8192];
        loop {
            let n = stdout.read(&mut buf).ok()?;
            if n == 0 {
                break;
            }
            hasher.write(&buf[..n]);
            total = total.saturating_add(n);
            if shown.len() < keep {
                let take = n.min(keep - shown.len());
                shown.extend_from_slice(&buf[..take]);
            }
        }
        Some(GitOutput {
            shown: String::from_utf8_lossy(&shown).into_owned(),
            digest: hasher.finish(),
            truncated: total > shown.len(),
        })
    });
    let started = Instant::now();
    let success = loop {
        if let Some(status) = child.try_wait().ok()? {
            break status.success();
        }
        if started.elapsed() >= GIT_COMMAND_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            return None;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let output = reader.join().ok().flatten()?;
    success.then_some(output)
}

/// A fresh, redacted snapshot of the repository immediately before an agent starts. It gives each
/// council member the same change id and a bounded diff excerpt while still requiring the agent to
/// re-read files before editing. Non-git folders simply have no snapshot.
pub fn workspace_state(workspace: &Path) -> Option<String> {
    let status = git_output(
        workspace,
        &["status", "--short", "--branch", "--untracked-files=normal"],
        MAX_WORKSPACE_STATUS_BYTES,
    )?;
    let diff = git_output(
        workspace,
        &[
            "diff", "HEAD", "--no-ext-diff", "--no-textconv", "--no-color", "--unified=1", "--", ".",
        ],
        MAX_WORKSPACE_DIFF_BYTES,
    )
    .or_else(|| {
        // An unborn repository has no HEAD yet, but its index can still be described.
        git_output(
            workspace,
            &["diff", "--cached", "--no-ext-diff", "--no-textconv", "--no-color", "--unified=1", "--", "."],
            MAX_WORKSPACE_DIFF_BYTES,
        )
    })?;
    let mut state = DefaultHasher::new();
    state.write_u64(status.digest);
    state.write_u64(diff.digest);
    let id = state.finish();
    let status_text = crate::redact::scrub(status.shown.trim());
    let diff_text = crate::redact::scrub(diff.shown.trim());
    let mut out = format!("PROJECT STATE (read-only snapshot refreshed immediately before this answer; id {id:016x}):\n");
    if status_text.is_empty() {
        out.push_str("Git status: clean");
    } else {
        out.push_str("Git status:\n");
        out.push_str(&status_text);
        if status.truncated {
            out.push_str("\n… status truncated");
        }
    }
    if !diff_text.is_empty() {
        out.push_str("\n\nCurrent diff excerpt:\n");
        out.push_str(&diff_text);
        if diff.truncated {
            out.push_str("\n… diff truncated; inspect the workspace for the complete change");
        }
    }
    out.push_str("\nRe-read any file immediately before changing it; another participant may edit after this snapshot.");
    Some(out)
}

pub fn label(sender: &str) -> String {
    match sender {
        "human" => "Human".into(),
        crate::council::CONSILIUM => "Consilium".into(),
        s => AGENTS.iter().find(|a| a.id == s).map(|a| a.name.to_string()).unwrap_or_else(|| s.to_string()),
    }
}

/// What the AI may do this turn, for its instructions.
pub struct Context<'a> {
    /// Agent ids in the council (including `me`).
    pub members: &'a [String],
    /// The project folder the AI is working in, when its access allows one.
    pub workspace: Option<&'a str>,
    pub access: Access,
    /// Council memory shared by every participant across conversations.
    pub memory: Option<&'a str>,
    /// The project's CONSILIUM.md, shared by every participant.
    pub project_notes: Option<&'a str>,
}

/// Native terminal guidance shared by every CLI-backed AI with shell access. Keeping this in the
/// system prompt makes the capability portable across providers while preserving the operating
/// system's own UAC / administrator authentication boundary.
fn terminal_capability(os: &str, access: Access) -> Option<String> {
    if !matches!(access, Access::Own | Access::Unrestricted) {
        return None;
    }
    let (platform, shells, elevation) = match os {
        "windows" => (
            "Windows",
            "Use PowerShell (`pwsh`, falling back to `powershell.exe`) for native automation. Use `cmd.exe /d /s /c` only when a `.cmd`/`.bat` file or a legacy command requires Command Prompt.",
            "After approval, use the normal Windows UAC path (for example PowerShell `Start-Process ... -Verb RunAs`) so Windows shows the person what is being elevated.",
        ),
        "macos" => (
            "macOS",
            "Use the native shell and ordinary command-line tools; use `zsh`, `bash`, or `sh` explicitly when a script requires one.",
            "After approval, use the normal macOS administrator authorization path. If `sudo` needs a TTY, open a visible native terminal for that one command or ask the person to run it there.",
        ),
        "linux" => (
            "Linux",
            "Use the native shell and ordinary command-line tools; use `bash` or `sh` explicitly when a script requires one.",
            "After approval, use the host's normal `sudo`, `doas`, or `pkexec` path. If authentication needs a TTY, open a visible native terminal for that one command or ask the person to run it there.",
        ),
        other => (
            other,
            "Use only native shell tools that are actually installed on this host.",
            "After approval, use the operating system's normal visible administrator-authorization path.",
        ),
    };
    Some(format!(
        "Native terminal capability ({platform}): {shells}\n\
Administrator elevation is a separate boundary even in Full capacity. Before any command that uses \
administrator/root elevation, stop and ask one exact request in this form: `REQUEST: Approve one elevated \
command on {platform} for <reason>? ELEVATED_COMMAND_JSON: <JSON string containing the exact command>`. \
Include the affected paths or services and the rollback in the reason. An approval applies only to that exact \
command, once, when the newest human message quotes that request with `Approved:`. A denial, edited command, \
custom response, later message, or old approval is not authorization. {elevation} Never ask for, read, store, \
echo, or pipe an administrator password, recovery key, UAC credential, token, or one-time code. Never use \
`sudo -S`, password environment variables, credential files, or an approval for one command to run another."
    ))
}

const ELEVATED_COMMAND_MARKER: &str = "ELEVATED_COMMAND_JSON:";

fn elevated_command_in_request(request: &str) -> Option<String> {
    let (before, encoded) = request.rsplit_once(ELEVATED_COMMAND_MARKER)?;
    if before.contains(ELEVATED_COMMAND_MARKER) {
        return None;
    }
    let command: String = serde_json::from_str(encoded.trim()).ok()?;
    let command = command.trim().to_string();
    (!command.is_empty()
        && command.len() <= 4_096
        && !command.chars().any(|ch| matches!(ch, '\n' | '\r' | '\0')))
    .then_some(command)
}

/// Exact, single-turn elevation grant for `me`. The approval must be the newest human message,
/// target this AI (when addressed), quote one of this AI's earlier REQUEST lines exactly, and end
/// in a machine-readable JSON command. Old approvals therefore cannot be replayed by a later turn.
pub fn elevated_command_grants(events: &[Event], me: &str) -> Vec<String> {
    let Some(approval) = events.iter().rev().find(|event| event.kind == "message" && event.sender == "human") else {
        return Vec::new();
    };
    if !approval.to.is_empty() && !approval.to.iter().any(|target| target == me) {
        return Vec::new();
    }
    let Some(request) = approval.text.trim().strip_prefix("Approved:").map(str::trim) else {
        return Vec::new();
    };
    let requested_by_me = events.iter().rev().any(|event| {
        event.seq < approval.seq
            && matches!(event.kind.as_str(), "message" | "error")
            && event.sender == me
            && event.requests.iter().any(|candidate| candidate == request)
    });
    if !requested_by_me {
        return Vec::new();
    }
    elevated_command_in_request(request).into_iter().collect()
}

pub fn system(me: &str, round: u32, rounds: u32, ctx: &Context) -> String {
    let others: Vec<String> = ctx
        .members
        .iter()
        .filter(|m| m.as_str() != me)
        .filter_map(|m| AGENTS.iter().find(|a| a.id == m))
        .map(|a| format!("{} (@{})", a.name, a.id))
        .collect();
    let others = match others.len() {
        0 => "the Human".to_string(),
        _ => format!("the Human, and {}", others.join(", ")),
    };
    let mut s = format!(
        "You are {name}, taking part in Consilium, a shared conversation between a human and several AI \
assistants. The other participants are: {others}. Every message in the transcript starts with a tag \
naming its author, such as [Human].

How to take part:
- Write only your own next message, as plain text. Never write lines for other participants or \
put a [Name] tag on your own message.
- Address someone with @name when it matters who should answer. The human is @human.
- When you need a decision, an approval, or an action from the human, put it on its own line \
starting with REQUEST: so it is shown to them in red. Ask only what you actually need. The human \
answers each REQUEST with Approve or Deny buttons, so when you can, phrase it as one question \
that a yes or no answers; their answer arrives as a message starting \"Approved:\" or \"Denied:\" followed \
by your request.
- To ask another participant something, @mention them: they see it and answer in the same turn.
- Participants answer independently and at the same time: nobody waits for a turn, so others may be writing while you are, and messages may arrive while you think. React to whatever is newest.
- Lines tagged [Consilium] are notes from the app itself (for example an AI that is out of usage and sitting out, or an update). Take them into account; don't reply to Consilium.{add_ai}
- To save something every participant should know in future conversations (a decision, a preference \
of the human's, a fact about this setup), put it on its own line starting REMEMBER:. Keep it to one \
short sentence, and never save passwords, keys or other secrets.
- Be direct and specific. Disagree plainly when you think another participant is wrong, and \
say why; concede in one line when they are right. No pleasantries and no recaps.",
        name = label(me),
        add_ai = add_ai_hint(ctx.members),
    );
    s.push_str("\n\n");
    s.push_str(&match (ctx.workspace, ctx.access) {
        (Some(ws), Access::Read) => format!(
            "Workspace: you can read the project folder {ws} (your current directory). You cannot change files. \
Cite paths and lines from what you actually read."
        ),
        (Some(ws), Access::Edit) => format!(
            "Workspace: you are working in the project folder {ws} (your current directory) and may read and \
edit its files. Other AI participants may edit the same folder in their turns, so re-read a file before \
changing it. Make focused changes, and end your message with a short list of the files you changed and why."
        ),
        (Some(ws), Access::Unrestricted) => format!(
            "Workspace: you are working in the project folder {ws} (your current directory) with no approval \
checks or sandbox: you can run commands, install software and change files. The human has allowed this; use \
it carefully, avoid destructive commands, and don't touch anything outside what the task needs. Other AI \
participants may change the same folder in their turns, so re-read files before changing them. End your \
message with a short list of what you changed or ran, and why."
        ),
        (Some(ws), Access::Own) => format!(
            "Workspace: you are working in the project folder {ws} (your current directory) with the permissions \
the human has already approved for you. Other AI participants may change the same folder in their turns, so \
re-read files before changing them. End your message with a short list of what you changed or ran, and why."
        ),
        _ => "You have no tools, files, or internet access here; you see only this transcript. Don't try to run \
commands or use tools: answer in text, and say what you would check rather than guessing."
            .to_string(),
    });
    if let Some(capability) = terminal_capability(std::env::consts::OS, ctx.access) {
        s.push_str("\n\n");
        s.push_str(&capability);
    }
    s.push_str(
        "\n\nCommand boxes: when you want the human to run a command, put it alone in a fenced block tagged with its \
shell (```bash, ```zsh, ```powershell or ```cmd). Consilium shows it with Run, Deny and Copy buttons; Run executes it \
once in the project folder with no terminal to type into, and the output comes back to you as the human's message. \
Use `sudo` lines (macOS/Linux) or `Start-Process -Verb RunAs` (Windows) when admin rights are needed: the computer \
shows its own password prompt. Use other tags (```text, ```rust …) for anything that shouldn't be run.",
    );
    if matches!(ctx.access, Access::Own | Access::Unrestricted) {
        s.push_str(
            " You can run commands yourself here, on every operating system including Windows: run ordinary \
commands (PowerShell, cmd, shell, git, npm, builds, tests) with your own tools and don't hand them to the human. Use a \
command box only for a command that needs administrator rights, or one your own tools failed to run (say why).",
        );
    }
    if ctx.workspace.is_some() && matches!(ctx.access, Access::Edit | Access::Own | Access::Unrestricted) {
        s.push_str(
            "\n\nWhen the human asks the council to build or change something, work at full capacity as a team: \
agree on a short plan, split the work between participants with @mentions so nobody duplicates effort, \
build your part in the project folder, and review what the others changed. You have creative latitude: \
make sensible decisions yourself and ask the human (REQUEST:) only for choices that are genuinely theirs.",
        );
    }
    if let Some(notes) = ctx.project_notes {
        s.push_str(&format!(
            "\n\nPROJECT NOTES (CONSILIUM.md in the project folder, shared by every participant):\n{notes}"
        ));
    }
    if let Some(memory) = ctx.memory {
        s.push_str(&format!(
            "\n\nCOUNCIL MEMORY (notes every participant keeps across conversations; newest last):\n{memory}"
        ));
    }
    if rounds > 1 {
        s.push_str(&format!(
            "\n\nThis is round {round} of {rounds} of a discussion among the AI participants. Build on, \
challenge, or correct the latest messages instead of repeating them. If you have nothing useful \
to add, reply with exactly PASS."
        ));
        if rounds >= 3 && round >= 3 && !ctx.members.is_empty() {
            // Rotate the critical-review role deterministically. Random assignment would make a
            // resumed turn behave differently after a restart; rotation still prevents every
            // model from falling into polite agreement while keeping the conversation replayable.
            let reviewer = &ctx.members[(round as usize - 3) % ctx.members.len()];
            if reviewer == me {
                s.push_str(
                    "\n\nADVERSARIAL REVIEW ROLE: You are the designated devil's advocate for this round. \
Identify the strongest concrete failure mode, unsafe assumption, or missing test in the current \
proposal. Cite transcript or workspace evidence, propose a way to falsify the claim, and do not \
agree merely to reach consensus.",
                );
            } else if let Some(agent) = AGENTS.iter().find(|a| a.id == reviewer) {
                s.push_str(&format!(
                    "\n\n{} is the designated adversarial reviewer this round. Address its evidence directly; \
do not smooth over a substantive objection just to reach consensus.",
                    agent.name
                ));
            }
        }
    }
    s
}

/// How to bring in an AI that isn't a participant yet; empty when every known AI already is.
fn add_ai_hint(members: &[String]) -> String {
    let outside: Vec<String> = AGENTS
        .iter()
        .filter(|a| !members.iter().any(|m| m == a.id))
        .map(|a| format!("{} (@{})", a.name, a.id))
        .collect();
    if outside.is_empty() {
        return String::new();
    }
    format!(
        "\n- To bring an AI that isn't a participant yet into the council, put a line ADD_AI: <name> in your \
message. Consilium presents the proposal to the human, who must approve the addition and sign in to \
that AI's account before any transcript is shared with it. AIs not yet \
in the council: {}. Never use ADD_AI for a current participant; @mention them instead.",
        outside.join(", ")
    )
}

/// Transcript entries, newest last. Once the safety threshold is crossed, returns the older
/// entries separately for condensation and keeps a generous recent window verbatim.
fn transcript_parts<'a>(events: &'a [Event], me: &str) -> (Vec<TranscriptPart<'a>>, Vec<TranscriptPart<'a>>) {
    let mut parts: Vec<TranscriptPart<'a>> = events
        .iter()
        .filter(|e| e.kind == "message" || (e.kind == "system" && e.sender == crate::council::CONSILIUM))
        .map(|e| {
            let who = if e.sender == me { format!("{} (you)", label(me)) } else { label(&e.sender) };
            (e, format!("[{who}] {}{}", e.text, crate::attach::for_transcript(&e.attachments, me)))
        })
        .collect();
    let mut total: usize = parts.iter().map(|(_, p)| p.len() + 2).sum();
    if total <= MAX_TRANSCRIPT_CHARS {
        return (Vec::new(), parts);
    }
    let mut older = Vec::new();
    while parts.len() > 1 && total > RECENT_TRANSCRIPT_CHARS {
        total -= parts[0].1.len() + 2;
        older.push(parts.remove(0));
    }
    (older, parts)
}

fn clipped(text: &str, max: usize) -> String {
    let mut chars = text.chars();
    let shown: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        format!("{}…", shown.trim_end())
    } else {
        shown
    }
}

/// A local, deterministic digest of older turns. It intentionally avoids another provider call:
/// context compression must still work offline, under rate limits, and on weaker hardware. The
/// scoring keeps human intent and explicit decisions ahead of routine progress narration.
fn transcript_digest(older: &[(&Event, String)]) -> String {
    if older.is_empty() {
        return String::new();
    }
    let mut candidates: Vec<(usize, u8, String)> = older
        .iter()
        .enumerate()
        .map(|(index, (event, _))| {
            let compact = event.text.split_whitespace().collect::<Vec<_>>().join(" ");
            let lower = compact.to_ascii_lowercase();
            let decisive = lower.contains("request:")
                || lower.starts_with("approved:")
                || lower.starts_with("denied:")
                || lower.contains("remember:");
            let outcome = [" decided ", " complete", " committed", " pushed", " blocked", " failed", " fixed", " next step"]
                .iter()
                .any(|needle| lower.contains(needle));
            let score = u8::from(decisive) * 100 + u8::from(event.sender == "human") * 30 + u8::from(outcome) * 10;
            let attachments = if event.attachments.is_empty() {
                String::new()
            } else {
                format!(
                    " [attached: {}]",
                    event.attachments.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", ")
                )
            };
            (
                index,
                score,
                format!("- [{}] {}{}", label(&event.sender), clipped(&compact, MAX_SUMMARY_ENTRY_CHARS), attachments),
            )
        })
        .collect();
    // Prefer decisions, requests, human intent and concrete outcomes; within a tier, prefer the
    // most recent entry. Restore chronological order before rendering.
    candidates.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| b.0.cmp(&a.0)));
    let header_reserve = 180;
    let mut used = header_reserve;
    let mut selected = Vec::new();
    for (index, _, line) in candidates {
        let cost = line.len() + 1;
        if used + cost <= MAX_TRANSCRIPT_SUMMARY_CHARS {
            used += cost;
            selected.push((index, line));
        }
    }
    selected.sort_by_key(|(index, _)| *index);
    format!(
        "EARLIER CONVERSATION DIGEST ({} messages condensed locally; {} key entries retained)\n{}",
        older.len(),
        selected.len(),
        selected.into_iter().map(|(_, line)| line).collect::<Vec<_>>().join("\n")
    )
}

/// Message ids actually included in this AI's bounded transcript.
pub fn visible_message_ids(events: &[Event], me: &str) -> Vec<String> {
    transcript_parts(events, me)
        .1
        .into_iter()
        .filter(|(event, _)| event.kind == "message" && event.sender != me)
        .map(|(event, _)| event.id.clone())
        .collect()
}

/// The transcript as text, newest last, with older turns condensed once the safety threshold is
/// crossed. The latest window remains verbatim so current code, requests and corrections retain
/// their exact wording.
pub fn transcript(events: &[Event], me: &str) -> String {
    let (older, parts) = transcript_parts(events, me);
    let mut out = String::from("CONVERSATION SO FAR\n\n");
    if !older.is_empty() {
        out.push_str(&transcript_digest(&older));
        out.push_str("\n\nRECENT MESSAGES (verbatim)\n\n");
    }
    out.push_str(&parts.into_iter().map(|(_, text)| text).collect::<Vec<_>>().join("\n\n"));
    out.push_str(&format!("\n\nEND OF CONVERSATION. Write your next message as {} now.", label(me)));
    out
}

/// True when a reply is just "PASS" (nothing to add in a discussion round).
pub fn is_pass(text: &str) -> bool {
    text.trim().trim_end_matches('.').eq_ignore_ascii_case("pass")
}

/// Agent ids mentioned with @, in order of appearance, without duplicates.
pub fn mentioned_agents(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let lower = text.to_lowercase();
    let mut push = |id: &str| {
        if !out.iter().any(|x| x == id) {
            out.push(id.to_string());
        }
    };
    let bytes: Vec<char> = lower.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == '@' && (i == 0 || !bytes[i - 1].is_alphanumeric()) {
            let word: String = bytes[i + 1..].iter().take_while(|c| c.is_alphanumeric() || **c == '_').collect();
            match word.as_str() {
                "all" | "everyone" => AGENTS.iter().for_each(|a| push(a.id)),
                w => {
                    if let Some(a) = AGENTS.iter().find(|a| a.id == w || a.name.to_lowercase() == w) {
                        push(a.id)
                    }
                }
            }
            i += 1 + word.len();
        } else {
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(sender: &str, text: &str) -> Event {
        Event {
            id: "x".into(),
            seq: 0,
            ts: 0,
            conversation: "c".into(),
            sender: sender.into(),
            kind: "message".into(),
            to: vec![],
            text: text.into(),
            requests: vec![],
            add_ai: vec![],
            seen: vec![],
            usage: None,
            attachments: vec![],
        }
    }

    #[test]
    fn transcript_tags_authors_and_marks_own_messages() {
        let t = transcript(&[ev("human", "hi"), ev("gpt", "hello"), ev("claude", "hey")], "claude");
        assert!(t.contains("[Human] hi"));
        assert!(t.contains("[ChatGPT] hello"));
        assert!(t.contains("[Claude (you)] hey"));
        assert!(t.ends_with("Write your next message as Claude now."));
    }

    #[test]
    fn transcript_condenses_oldest_messages_and_keeps_recent_messages_verbatim() {
        let big = "x".repeat(MAX_TRANSCRIPT_CHARS / 2);
        let mut old = ev("human", &big);
        old.id = "old".into();
        old.text = format!("Approved: keep the safe design {big}");
        let mut middle = ev("gpt", &big);
        middle.id = "middle".into();
        let mut latest = ev("human", "latest");
        latest.id = "latest".into();
        let events = [old, middle, latest];
        let t = transcript(&events, "gpt");
        assert!(t.contains("latest"));
        assert!(t.contains("EARLIER CONVERSATION DIGEST"));
        assert!(t.contains("Approved: keep the safe design"));
        assert!(t.contains("RECENT MESSAGES (verbatim)"));
        assert!(t.len() <= MAX_TRANSCRIPT_CHARS + 500);
        assert_eq!(visible_message_ids(&events, "gpt"), vec!["latest"]);
    }

    #[test]
    fn transcript_digest_prioritizes_decisions_and_human_intent() {
        let noise = "routine progress ".repeat(900);
        let mut events: Vec<Event> = (0..20).map(|_| ev("gpt", &noise)).collect();
        events.insert(0, ev("human", "Approved: preserve this decision"));
        events.push(ev("human", "the current task stays exact"));
        let t = transcript(&events, "claude");
        assert!(t.contains("Approved: preserve this decision"));
        assert!(t.contains("the current task stays exact"));
        assert!(t.len() <= MAX_TRANSCRIPT_CHARS + 500);
    }

    #[test]
    fn workspace_state_is_fresh_bounded_and_secret_scrubbed() {
        let repo = std::env::temp_dir().join(format!("consilium-prompt-git-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&repo);
        std::fs::create_dir_all(&repo).unwrap();
        let git = |args: &[&str]| {
            assert!(Command::new("git").arg("-C").arg(&repo).args(args).status().unwrap().success());
        };
        git(&["init", "-q"]);
        git(&["config", "user.name", "Consilium Test"]);
        git(&["config", "user.email", "test@invalid.example"]); // consilium-allow-secret-scan (non-routable test identity)
        std::fs::write(repo.join("note.txt"), "base\n").unwrap();
        git(&["add", "note.txt"]);
        git(&["commit", "-qm", "base", "--no-gpg-sign"]);

        let secret = format!("sk-{}", "Ab3_".repeat(8));
        std::fs::write(repo.join("note.txt"), format!("base\nAPI_KEY={secret}\n")).unwrap();
        let state = workspace_state(&repo).unwrap();
        assert!(state.contains("PROJECT STATE") && state.contains("note.txt"));
        assert!(state.contains("Current diff excerpt"));
        assert!(state.contains("[REDACTED OpenAI key]"));
        assert!(!state.contains(&secret));

        std::fs::remove_dir_all(&repo).unwrap();
        let plain = std::env::temp_dir();
        assert!(workspace_state(&plain).is_none());
    }

    #[test]
    fn consilium_notes_reach_the_ais_but_other_status_notes_dont() {
        let mut note = ev("consilium", "Claude ran out of usage");
        note.kind = "system".into();
        let mut status = ev("gpt", "ChatGPT had nothing to add.");
        status.kind = "system".into();
        let t = transcript(&[ev("human", "hi"), note, status], "gpt");
        assert!(t.contains("[Consilium] Claude ran out of usage"));
        assert!(!t.contains("nothing to add"));
    }

    #[test]
    fn mentions_resolve_names_ids_and_all() {
        assert_eq!(mentioned_agents("@claude and @GPT"), vec!["claude", "gpt"]);
        assert_eq!(mentioned_agents("@ChatGPT please"), vec!["gpt"]);
        assert_eq!(mentioned_agents("@all"), vec!["gpt", "claude", "gemini", "grok", "deepseek"]);
        assert!(mentioned_agents("ping me at team@claude").is_empty());
        assert!(mentioned_agents("Claude said so").is_empty());
    }

    #[test]
    fn pass_detection() {
        assert!(is_pass(" PASS. "));
        assert!(!is_pass("PASS on this, but..."));
    }

    fn members(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn system_prompt_names_members_and_round_rules() {
        let m = members(&["gpt", "claude", "gemini"]);
        let ctx = Context { members: &m, workspace: None, access: Access::Chat, memory: None, project_notes: None };
        let s = system("gpt", 2, 3, &ctx);
        assert!(s.contains("You are ChatGPT"));
        assert!(s.contains("Claude (@claude), Gemini (@gemini)"));
        assert!(s.contains("round 2 of 3"));
        assert!(s.contains("no tools"));
        assert!(!system("gpt", 1, 1, &ctx).contains("round"));
        let m = members(&["gpt", "claude"]);
        let two = system("gpt", 1, 1, &Context { members: &m, workspace: None, access: Access::Chat, memory: None, project_notes: None });
        assert!(two.contains("The other participants are: the Human, and Claude (@claude)."));
        assert!(two.contains("ADD_AI:") && two.contains("Gemini (@gemini)"), "lists AIs that can be added");
        assert!(two.contains("human, who must approve"), "participant additions require human approval");
        let all = members(&["gpt", "claude", "gemini", "grok", "deepseek"]);
        let full = system("gpt", 1, 1, &Context { members: &all, workspace: None, access: Access::Chat, memory: None, project_notes: None });
        assert!(!full.contains("ADD_AI"), "no ADD_AI hint once every AI is a participant");
        assert!(full.contains("@mention them: they see it and answer"));
        assert!(full.contains("at the same time") && full.contains("[Consilium]"));
        assert!(full.contains("Approve or Deny buttons") && full.contains("Approved:") && full.contains("Denied:"));
        let with = system("gpt", 1, 1, &Context { members: &all, workspace: None, access: Access::Chat, memory: Some("- Use Tauri 2"), project_notes: Some("Run cargo test") });
        assert!(with.contains("COUNCIL MEMORY") && with.contains("Use Tauri 2"));
        assert!(with.contains("PROJECT NOTES") && with.contains("Run cargo test"));
        assert!(with.contains("REMEMBER:"));
    }

    #[test]
    fn round_three_rotates_an_explicit_adversarial_reviewer() {
        let m = members(&["gpt", "claude", "gemini"]);
        let ctx = Context {
            members: &m,
            workspace: None,
            access: Access::Chat,
            memory: None,
            project_notes: None,
        };
        assert!(system("gpt", 3, 4, &ctx).contains("ADVERSARIAL REVIEW ROLE"));
        assert!(
            system("claude", 3, 4, &ctx).contains("ChatGPT is the designated adversarial reviewer")
        );
        assert!(system("claude", 4, 4, &ctx).contains("ADVERSARIAL REVIEW ROLE"));
        assert!(!system("gpt", 2, 4, &ctx).contains("ADVERSARIAL REVIEW ROLE"));
    }

    #[test]
    fn system_prompt_describes_workspace_access() {
        let m = members(&["gpt"]);
        let edit = system("gpt", 1, 1, &Context { members: &m, workspace: Some("/p"), access: Access::Edit, memory: None, project_notes: None });
        assert!(edit.contains("may read and edit") && edit.contains("/p"));
        assert!(edit.contains("full capacity"));
        assert!(!system("gpt", 1, 1, &Context { members: &m, workspace: None, access: Access::Edit, memory: None, project_notes: None }).contains("full capacity"));
        let read = system("gpt", 1, 1, &Context { members: &m, workspace: Some("/p"), access: Access::Read, memory: None, project_notes: None });
        assert!(read.contains("cannot change files"));
    }

    #[test]
    fn native_terminal_skill_is_cross_platform_and_approval_gated() {
        for (os, names) in [
            ("windows", ["PowerShell", "cmd.exe"]),
            ("macos", ["macOS", "sudo"]),
            ("linux", ["Linux", "sudo"]),
        ] {
            let text = terminal_capability(os, Access::Unrestricted).unwrap();
            assert!(names.iter().all(|name| text.contains(name)), "{os}: {text}");
            assert!(text.contains("ELEVATED_COMMAND_JSON"));
            assert!(text.contains("exact command, once"));
            assert!(text.contains("Never ask for, read, store"));
        }
        assert!(terminal_capability("windows", Access::Own).is_some());
        assert!(terminal_capability("windows", Access::Edit).is_none());
        assert!(terminal_capability("linux", Access::Read).is_none());
        assert!(terminal_capability("macos", Access::Chat).is_none());
    }

    #[test]
    fn elevation_grant_is_exact_targeted_and_only_on_the_latest_human_approval() {
        let request = r##"Approve one elevated command on Linux to install the package? ELEVATED_COMMAND_JSON: "sudo apt-get install widget""##;
        let mut asked = ev("gpt", &format!("REQUEST: {request}"));
        asked.id = "asked".into();
        asked.seq = 1;
        asked.requests = vec![request.into()];
        let mut approved = ev("human", &format!("Approved: {request}"));
        approved.id = "approved".into();
        approved.seq = 2;
        approved.to = vec!["gpt".into()];
        assert_eq!(
            elevated_command_grants(&[asked.clone(), approved.clone()], "gpt"),
            vec!["sudo apt-get install widget"]
        );
        assert!(elevated_command_grants(&[asked.clone(), approved.clone()], "claude").is_empty());

        let mut later = ev("human", "also update the docs");
        later.seq = 3;
        assert!(elevated_command_grants(&[asked.clone(), approved.clone(), later], "gpt").is_empty());

        // The emergency brake records a blocked turn as an "error" event; its REQUEST must still grant.
        let mut blocked = asked.clone();
        blocked.kind = "error".into();
        assert_eq!(
            elevated_command_grants(&[blocked, approved.clone()], "gpt"),
            vec!["sudo apt-get install widget"]
        );

        approved.text = format!("Approved: {request} --changed");
        assert!(elevated_command_grants(&[asked, approved], "gpt").is_empty());
    }
}
