//! What each agent is told: who is in the room, how to address the human, and the transcript.

use crate::agents::AGENTS;
use crate::store::Event;

/// Older turns are dropped first once the transcript passes this many characters.
pub const MAX_TRANSCRIPT_CHARS: usize = 120_000;

pub fn label(sender: &str) -> String {
    match sender {
        "human" => "Human".into(),
        s => AGENTS.iter().find(|a| a.id == s).map(|a| a.name.to_string()).unwrap_or_else(|| s.to_string()),
    }
}

pub fn system(me: &str, round: u32, rounds: u32) -> String {
    let others: Vec<String> = AGENTS
        .iter()
        .filter(|a| a.id != me)
        .map(|a| format!("{} (@{})", a.name, a.id))
        .collect();
    let mut s = format!(
        "You are {name}, taking part in Consilium, a shared conversation between a human and several AI \
assistants. The other participants are: the Human, and {others}. Every message in the transcript starts \
with a tag naming its author, such as [Human] or [{example}].

How to take part:
- Write only your own next message, as plain text. Never write lines for other participants or \
put a [Name] tag on your own message.
- Address someone with @name when it matters who should answer. The human is @human.
- When you need a decision, an approval, or an action from the human, put it on its own line \
starting with REQUEST: so it is shown to them in red. Ask only what you actually need.
- You have no tools, files, or internet access here; you see only this transcript. Say what you \
would check rather than guessing.
- Be direct and specific. Disagree plainly when you think another participant is wrong, and \
say why; concede in one line when they are right. No pleasantries and no recaps.",
        name = label(me),
        others = others.join(" and "),
        example = label(AGENTS.iter().find(|a| a.id != me).map_or("gpt", |a| a.id)),
    );
    if rounds > 1 {
        s.push_str(&format!(
            "\n\nThis is round {round} of {rounds} of a discussion among the AI participants. Build on, \
challenge, or correct the latest messages instead of repeating them. If you have nothing useful \
to add, reply with exactly PASS."
        ));
    }
    s
}

/// The transcript as text, newest last, trimmed from the oldest end to fit.
pub fn transcript(events: &[Event], me: &str) -> String {
    let mut parts: Vec<String> = events
        .iter()
        .filter(|e| e.kind == "message")
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
    fn mentions_resolve_names_ids_and_all() {
        assert_eq!(mentioned_agents("@claude and @GPT"), vec!["claude", "gpt"]);
        assert_eq!(mentioned_agents("@ChatGPT please"), vec!["gpt"]);
        assert_eq!(mentioned_agents("@all"), vec!["gpt", "claude"]);
        assert!(mentioned_agents("ping me at team@claude").is_empty());
        assert!(mentioned_agents("Claude said so").is_empty());
    }

    #[test]
    fn pass_detection() {
        assert!(is_pass(" PASS. "));
        assert!(!is_pass("PASS on this, but..."));
    }

    #[test]
    fn system_prompt_names_others_and_round_rules() {
        let s = system("gpt", 2, 3);
        assert!(s.contains("You are ChatGPT"));
        assert!(s.contains("Claude (@claude)"));
        assert!(s.contains("round 2 of 3"));
        assert!(!system("gpt", 1, 1).contains("round"));
    }
}
