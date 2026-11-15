//! What each agent is told: who is in the room, how to address the human, and the transcript.

use crate::agents::AGENTS;
use crate::config::Access;
use crate::store::Event;

/// Older turns are dropped first once the transcript passes this many characters.
pub const MAX_TRANSCRIPT_CHARS: usize = 120_000;

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
starting with REQUEST: so it is shown to them in red. Ask only what you actually need.
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
message. Consilium then installs and connects it; the human signs in to that AI's account. AIs not yet \
in the council: {}. Never use ADD_AI for a current participant; @mention them instead.",
        outside.join(", ")
    )
}

/// The transcript as text, newest last, trimmed from the oldest end to fit.
pub fn transcript(events: &[Event], me: &str) -> String {
    let mut parts: Vec<String> = events
        .iter()
        .filter(|e| e.kind == "message" || (e.kind == "system" && e.sender == crate::council::CONSILIUM))
        .map(|e| {
            let who = if e.sender == me { format!("{} (you)", label(me)) } else { label(&e.sender) };
            format!("[{who}] {}", e.text)
        })
        .collect();
    let mut total: usize = parts.iter().map(|p| p.len() + 2).sum();
    let mut dropped = 0;
    while parts.len() > 1 && total > MAX_TRANSCRIPT_CHARS {
        total -= parts[0].len() + 2;
        parts.remove(0);
        dropped += 1;
    }
    let mut out = String::from("CONVERSATION SO FAR\n\n");
    if dropped > 0 {
        out.push_str(&format!("({dropped} earlier messages omitted for length)\n\n"));
    }
    out.push_str(&parts.join("\n\n"));
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
    fn transcript_drops_oldest_messages_first() {
        let big = "x".repeat(MAX_TRANSCRIPT_CHARS / 2);
        let t = transcript(&[ev("human", &big), ev("gpt", &big), ev("human", "latest")], "gpt");
        assert!(t.contains("latest"));
        assert!(t.contains("earlier messages omitted"));
        assert!(t.len() <= MAX_TRANSCRIPT_CHARS + 500);
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
        assert_eq!(mentioned_agents("@all"), vec!["gpt", "claude", "gemini"]);
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
        let all = members(&["gpt", "claude", "gemini"]);
        let full = system("gpt", 1, 1, &Context { members: &all, workspace: None, access: Access::Chat, memory: None, project_notes: None });
        assert!(!full.contains("ADD_AI"), "no ADD_AI hint once every AI is a participant");
        assert!(full.contains("@mention them: they see it and answer"));
        assert!(full.contains("at the same time") && full.contains("[Consilium]"));
        let with = system("gpt", 1, 1, &Context { members: &all, workspace: None, access: Access::Chat, memory: Some("- Use Tauri 2"), project_notes: Some("Run cargo test") });
        assert!(with.contains("COUNCIL MEMORY") && with.contains("Use Tauri 2"));
        assert!(with.contains("PROJECT NOTES") && with.contains("Run cargo test"));
        assert!(with.contains("REMEMBER:"));
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
}
