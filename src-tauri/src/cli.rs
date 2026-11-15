//! The `consilium` terminal command: the council in a terminal, in the same conversations as the
//! app. It is the app's own program started with a command (`consilium chat`); without one, the
//! program opens the window as usual. The app and the terminal can be used one after the other or
//! both at once (see `engine`).

use crate::agents;
use crate::engine::{self, Engine, TurnState, Ui};
use crate::store::{now_ms, ConversationMeta, Event};
use serde::Serialize;
use std::collections::HashSet;
use std::io::{BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const COMMANDS: &[&str] =
    &["chat", "ask", "say", "log", "list", "new", "stop", "status", "check", "install-cli", "help", "--help", "-h", "--version", "-V"];

/// Whether the program was started as the terminal command rather than to open the window.
pub fn wants_cli(args: &[String]) -> bool {
    args.first().is_some_and(|a| COMMANDS.contains(&a.as_str()))
}

const HELP: &str = "consilium: the Consilium council in a terminal, in the same conversations as the app.

Usage:
  consilium chat [options]           talk with the council (type /help inside)
  consilium ask MESSAGE [options]    ask, print the answers as they come, and exit
  consilium say MESSAGE [options]    add a message without asking any AI
  consilium log [-n N] [-f]          show a conversation; -f keeps following it
  consilium list                     list conversations, newest first
  consilium new [TITLE] [--folder P] start a conversation
  consilium stop [options]           stop the AIs in a conversation (here or in the app)
  consilium status                   the council: who is in it, signed in, access
  consilium check                    one tiny test message to every AI (uses a little allowance)
  consilium install-cli              install this `consilium` command for your terminal
  consilium                          (no command) open the app

Options:
  -c, --conversation C   which conversation: a number from `consilium list`, an id, or words
                         from its title (default: the most recent one)
      --new              start a new conversation
      --to NAMES         who is asked: gpt, claude, gemini, comma-separated, or all (default: all)
      --discuss N        let them discuss, up to N answers each (default: one answer each)
      --folder PATH      the conversation's project folder (\".\" for this one)
  -a, --attach FILE      attach a file to the message (ask, say; repeat for more): documents,
                         Word files, pictures, PDFs, code
  -n N                   how many recent messages to show (0 for all)
  -f, --follow           keep showing new messages (log)

@name in a message also asks that AI. The app and the terminal share conversations, council,
memory and settings, and can be used at the same time.";

const CHAT_HELP: &str = "  /to NAMES       who is asked: gpt, claude, gemini, comma-separated, or all
  /discuss N      let them discuss, up to N answers each; /ask for one answer each
  /stop           stop the AIs (Ctrl-C does too)
  /say TEXT       add a message without asking any AI
  /attach FILE…   attach files to your next message (drag them in); /attach lists them,
                  /detach clears them
  /list           conversations; /open N to switch; /new to start one
  /folder [PATH]  show or set the project folder (\"none\" to clear)
  /history [N]    show recent messages again
  /status         the council
  /quit           leave (Ctrl-D too); AIs still answering are stopped
@name in a message also asks that AI.";

#[derive(Debug, Default, PartialEq)]
struct Opts {
    conversation: Option<String>,
    new: bool,
    to: Option<Vec<String>>,
    rounds: Option<u32>,
    folder: Option<String>,
    lines: Option<usize>,
    follow: bool,
    attach: Vec<String>,
    words: Vec<String>,
}

fn parse(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts::default();
    let mut i = 0;
    let value = |i: &mut usize, name: &str| -> Result<String, String> {
        *i += 1;
        args.get(*i).cloned().ok_or_else(|| format!("{name} needs a value"))
    };
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "-c" | "--conversation" => o.conversation = Some(value(&mut i, a)?),
            "--new" => o.new = true,
            "--to" => o.to = Some(agent_ids(&value(&mut i, a)?)?),
            "--discuss" => {
                let n = value(&mut i, a)?;
                o.rounds = Some(n.parse::<u32>().map_err(|_| format!("--discuss takes a number, not {n}"))?.max(1));
            }
            "--folder" => o.folder = Some(value(&mut i, a)?),
            "-n" => {
                let n = value(&mut i, a)?;
                o.lines = Some(n.parse().map_err(|_| format!("-n takes a number, not {n}"))?);
            }
            "-f" | "--follow" => o.follow = true,
            "-a" | "--attach" => o.attach.push(value(&mut i, a)?),
            "--" => {
                o.words.extend(args[i + 1..].iter().cloned());
                break;
            }
            _ if a.starts_with('-') && a.len() > 1 => return Err(format!("unknown option {a} (see consilium help)")),
            _ => o.words.push(args[i].clone()),
        }
        i += 1;
    }
    Ok(o)
}

/// Agent ids from a list like "gpt, Claude" or "all" (all = every council member: an empty list
/// here, filled in from the council when used).
fn agent_ids(list: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for word in list.split(|c: char| c == ',' || c.is_whitespace()).filter(|w| !w.is_empty()) {
        let w = word.trim_start_matches('@').to_lowercase();
        if w == "all" || w == "everyone" {
            return Ok(vec![]);
        }
        let a = agents::AGENTS
            .iter()
            .find(|a| a.id == w || a.name.to_lowercase() == w || (a.id == "gpt" && (w == "openai" || w == "codex")))
            .ok_or_else(|| format!("no AI called {word} (try gpt, claude or gemini)"))?;
        if !out.iter().any(|x| x == a.id) {
            out.push(a.id.to_string());
        }
    }
    Ok(out)
}

/// The conversation `query` names: a number from `list` (newest first), an id or the start of
/// one, or words from its title.
fn find(list: &[ConversationMeta], query: &str) -> Result<ConversationMeta, String> {
    if let Ok(n) = query.parse::<usize>() {
        return list.get(n.wrapping_sub(1)).cloned().ok_or_else(|| format!("there is no conversation {n} (see consilium list)"));
    }
    let q = query.to_lowercase();
    let matches: Vec<&ConversationMeta> = if q.len() >= 4 && list.iter().any(|m| m.id.starts_with(&q)) {
        list.iter().filter(|m| m.id.starts_with(&q)).collect()
    } else {
        list.iter().filter(|m| m.title.to_lowercase().contains(&q)).collect()
    };
    match matches.as_slice() {
        [one] => Ok((*one).clone()),
        [] => Err(format!("no conversation matches \"{query}\" (see consilium list)")),
        many => Err(format!(
            "\"{query}\" matches {} conversations; use its number from consilium list: {}",
            many.len(),
            many.iter().map(|m| format!("\"{}\"", m.title)).collect::<Vec<_>>().join(", ")
        )),
    }
}

// ---------------------------------------------------------------- output

#[derive(Clone, Copy)]
struct Paint {
    on: bool,
}

impl Paint {
    fn new() -> Self {
        Paint { on: std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none() }
    }
    fn c(self, code: &str, s: &str) -> String {
        if self.on {
            format!("\x1b[{code}m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }
}

fn name_of(sender: &str) -> String {
    match sender {
        "human" => "You".into(),
        crate::council::CONSILIUM => "Consilium".into(),
        id => agents::by_id(id).map_or_else(|| id.to_string(), |a| a.name.to_string()),
    }
}

fn color_of(sender: &str) -> &'static str {
    match sender {
        "gpt" => "1;32",
        "claude" => "1;38;5;209",
        "gemini" => "1;34",
        "human" => "1;36",
        _ => "1",
    }
}

fn render(p: Paint, e: &Event) -> String {
    match e.kind.as_str() {
        "message" => {
            let mut head = p.c(color_of(&e.sender), &name_of(&e.sender));
            if e.sender == "human" && !e.to.is_empty() {
                let to: Vec<String> = e.to.iter().map(|t| name_of(t)).collect();
                head.push_str(&p.c("2", &format!(" → {}", to.join(", "))));
            }
            let mut out = format!("{head}\n");
            for a in &e.attachments {
                out.push_str(&format!("  {}\n", p.c("2", &format!("📎 {} ({}, {})", a.name, a.kind, human_size(a.size)))));
            }
            for line in e.text.lines() {
                let t = line.trim_start();
                if t.len() >= 8 && t[..8].eq_ignore_ascii_case("REQUEST:") {
                    out.push_str(&format!("  {}\n", p.c("1;31", line)));
                } else {
                    out.push_str(&format!("  {line}\n"));
                }
            }
            out.push('\n');
            out
        }
        "error" => format!("{}\n\n", p.c("31", &format!("! {}", e.text))),
        _ => format!("{}\n", p.c("2", &format!("· {}", e.text))),
    }
}

fn human_size(bytes: u64) -> String {
    match bytes {
        b if b >= 1024 * 1024 => format!("{:.1} MB", b as f64 / (1024.0 * 1024.0)),
        b if b >= 1024 => format!("{} KB", b / 1024),
        b => format!("{b} bytes"),
    }
}

/// Splits `/attach` arguments: quotes group words, and a backslash keeps the next character (as a
/// terminal writes a dragged-in path with spaces).
fn split_paths(s: &str) -> Vec<String> {
    let (mut out, mut cur, mut quote, mut escaped, mut any) = (Vec::new(), String::new(), None::<char>, false, false);
    for c in s.chars() {
        if escaped {
            cur.push(c);
            escaped = false;
            any = true;
        } else if c == '\\' && quote != Some('\'') && !cfg!(windows) {
            escaped = true;
        } else if Some(c) == quote {
            quote = None;
        } else if quote.is_none() && (c == '"' || c == '\'') {
            quote = Some(c);
            any = true;
        } else if quote.is_none() && c.is_whitespace() {
            if any {
                out.push(std::mem::take(&mut cur));
                any = false;
            }
        } else {
            cur.push(c);
            any = true;
        }
    }
    if any {
        out.push(cur);
    }
    out
}

/// Expands a leading ~ to the home folder.
fn expand(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => dirs::home_dir().map(|h| h.join(rest)).unwrap_or_else(|| PathBuf::from(path)),
        None => PathBuf::from(path),
    }
}

fn print(s: &str) {
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(s.as_bytes());
    let _ = out.flush();
}

/// Prints what is added to a conversation, from this process or the app, as it arrives.
struct Feed {
    conversation: String,
    last: u64,
    len: u64,
    /// Events not to print (the messages typed here, which the terminal already shows).
    skip: HashSet<String>,
    errors: usize,
}

impl Feed {
    /// Starts after the newest event, or shows the last `show` first.
    fn new(engine: &Engine, conversation: &str, show: Option<usize>, p: Paint) -> Self {
        let events = engine.store.load(conversation).unwrap_or_default();
        if let Some(n) = show {
            let from = if n == 0 { 0 } else { events.len().saturating_sub(n) };
            if from > 0 {
                print(&p.c("2", &format!("… {from} earlier messages (consilium log -n 0 shows all)\n\n")));
            }
            for e in &events[from..] {
                print(&render(p, e));
            }
        }
        Feed {
            conversation: conversation.into(),
            last: events.last().map_or(0, |e| e.seq),
            len: engine.store.log_len(conversation),
            skip: HashSet::new(),
            errors: 0,
        }
    }

    fn poll(&mut self, engine: &Engine, p: Paint) {
        let len = engine.store.log_len(&self.conversation);
        if len == self.len {
            return;
        }
        self.len = len;
        for e in engine.store.load(&self.conversation).unwrap_or_default() {
            if e.seq <= self.last {
                continue;
            }
            self.last = e.seq;
            if e.kind == "error" {
                self.errors += 1;
            }
            if !self.skip.remove(&e.id) {
                print(&render(p, &e));
            }
        }
    }
}

/// The terminal's side of the engine: says who is thinking in the conversation being shown.
struct Term {
    paint: Paint,
    conversation: Mutex<String>,
    thinking: Mutex<Vec<String>>,
}

impl Ui for Term {
    fn event(&self, _e: &Event) {
        // The feed prints events from the conversation itself, whoever added them.
    }
    fn turn(&self, t: TurnState) {
        if *self.conversation.lock().unwrap() != t.conversation {
            return;
        }
        let mut thinking = self.thinking.lock().unwrap();
        if t.agents.is_empty() || *thinking == t.agents {
            *thinking = t.agents;
            return;
        }
        let names: Vec<String> = t.agents.iter().map(|a| name_of(a)).collect();
        let verb = if names.len() == 1 { "is" } else { "are" };
        print(&self.paint.c("2", &format!("… {} {verb} thinking\n", names.join(" and "))));
        *thinking = t.agents;
    }
    fn activity(&self, activity: &crate::engine::AgentActivity) {
        if *self.conversation.lock().unwrap() != activity.conversation {
            return;
        }
        let detail = if activity.detail.is_empty() {
            String::new()
        } else {
            format!(" · {}", activity.detail)
        };
        print(&self.paint.c("2", &format!("… {}: {}{detail}\n", name_of(&activity.agent), activity.label)));
    }
}

// ---------------------------------------------------------------- commands

/// Runs the terminal command; returns the process's exit code.
pub fn main(args: Vec<String>) -> i32 {
    #[cfg(windows)]
    attach_console();
    let command = args.first().cloned().unwrap_or_default();
    match command.as_str() {
        "help" | "--help" | "-h" => {
            print(&format!("{HELP}\n"));
            return 0;
        }
        "--version" | "-V" => {
            print(&format!("consilium {}\n", env!("CARGO_PKG_VERSION")));
            return 0;
        }
        "install-cli" => {
            return match install() {
                Ok(i) => {
                    print(&format!("Installed {}\n{}\n", i.path, i.note));
                    0
                }
                Err(e) => fail(&e),
            }
        }
        _ => {}
    }
    let opts = match parse(&args[1..]) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("consilium: {e}");
            return 2;
        }
    };
    let Some(data) = crate::data_dir() else { return fail("couldn't find this computer's app data folder") };
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => return fail(&e.to_string()),
    };
    let paint = Paint::new();
    let term = Arc::new(Term { paint, conversation: Mutex::new(String::new()), thinking: Mutex::new(vec![]) });
    let code = rt.block_on(async {
        let spawn = Box::new(|t: engine::Task| {
            tokio::spawn(t);
        });
        let engine = match Engine::open(&data, term.clone(), spawn) {
            Ok(e) => Arc::new(e),
            Err(e) => return fail(&format!("couldn't open {}: {e}", data.display())),
        };
        let r = match command.as_str() {
            "chat" => chat(&engine, &term, opts, paint).await,
            "ask" => ask(&engine, &term, opts, paint).await,
            "say" => say(&engine, opts),
            "log" => log(&engine, opts, paint).await,
            "list" => {
                list(&engine, paint);
                Ok(0)
            }
            "new" => new(&engine, opts),
            "stop" => stop(&engine, opts).await,
            "status" => status(&engine, paint).await,
            "check" => check(&engine, paint).await,
            _ => Err(format!("unknown command {command}")),
        };
        engine.stop_all().await;
        r.unwrap_or_else(|e| fail(&e))
    });
    rt.shutdown_timeout(Duration::from_secs(1));
    code
}

fn fail(e: &str) -> i32 {
    eprintln!("consilium: {e}");
    1
}

/// The conversation the options name, the most recent one, or a new one if there are none.
fn conversation(engine: &Engine, o: &Opts) -> Result<ConversationMeta, String> {
    let meta = if o.new {
        engine.store.create("New conversation")?
    } else {
        let list = engine.store.list();
        match &o.conversation {
            Some(q) => find(&list, q)?,
            None => match list.into_iter().next() {
                Some(m) => m,
                None => engine.store.create("New conversation")?,
            },
        }
    };
    match &o.folder {
        Some(f) => set_folder(engine, &meta.id, f),
        None => Ok(meta),
    }
}

fn set_folder(engine: &Engine, id: &str, folder: &str) -> Result<ConversationMeta, String> {
    let f = folder.trim();
    let clear = f.is_empty() || f.eq_ignore_ascii_case("none");
    engine.store.set_workspace(id, if clear { None } else { Some(f) })
}

/// Who a message goes to: the chosen AIs, or every council member.
fn targets(engine: &Engine, chosen: &Option<Vec<String>>) -> Vec<String> {
    match chosen {
        Some(t) if !t.is_empty() => t.clone(),
        _ => engine.config.get().members,
    }
}

fn names(ids: &[String]) -> String {
    ids.iter().map(|t| name_of(t)).collect::<Vec<_>>().join(", ")
}

fn message(o: &Opts) -> Result<String, String> {
    if !o.words.is_empty() && o.words != ["-"] {
        return Ok(o.words.join(" "));
    }
    // `consilium ask -` or a piped message: read it from stdin.
    if o.words == ["-"] || !std::io::stdin().is_terminal() {
        let mut s = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut s).map_err(|e| e.to_string())?;
        if !s.trim().is_empty() {
            return Ok(s);
        }
    }
    Err("what's the message? (consilium ask \"…\")".into())
}

/// Waits until nobody is answering in `conversation`, here or in the app, printing as it goes.
/// Ctrl-C stops the AIs.
async fn follow_turn(engine: &Arc<Engine>, feed: &mut Feed, p: Paint) {
    let conv = feed.conversation.clone();
    let mut idle_since: Option<std::time::Instant> = None;
    loop {
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(250)) => {}
            _ = tokio::signal::ctrl_c() => {
                print(&p.c("2", "· Stopping the AIs…\n"));
                engine.stop(&conv);
            }
        }
        feed.poll(engine, p);
        if engine.busy(&conv) {
            idle_since = None;
        } else if idle_since.get_or_insert_with(std::time::Instant::now).elapsed() > Duration::from_millis(600) {
            // Idle for a moment, so a turn the app is about to start for this message has begun.
            feed.poll(engine, p);
            return;
        }
    }
}

async fn ask(engine: &Arc<Engine>, term: &Term, o: Opts, p: Paint) -> Result<i32, String> {
    let text = if o.attach.is_empty() { message(&o)? } else { o.words.join(" ") };
    let meta = conversation(engine, &o)?;
    *term.conversation.lock().unwrap() = meta.id.clone();
    let to = targets(engine, &o.to);
    let files = engine.attach(&meta.id, &o.attach.iter().map(|f| expand(f)).collect::<Vec<_>>())?;
    let mut feed = Feed::new(engine, &meta.id, None, p);
    tokio::spawn(engine.clone().watch(false));
    let sent = engine.send_with(&meta.id, &text, files, to, o.rounds.unwrap_or(1))?;
    feed.skip.insert(sent.id);
    follow_turn(engine, &mut feed, p).await;
    Ok(if feed.errors > 0 { 1 } else { 0 })
}

fn say(engine: &Arc<Engine>, o: Opts) -> Result<i32, String> {
    let text = if o.attach.is_empty() { message(&o)? } else { o.words.join(" ") };
    let meta = conversation(engine, &o)?;
    let files = engine.attach(&meta.id, &o.attach.iter().map(|f| expand(f)).collect::<Vec<_>>())?;
    engine.say_with(&meta.id, &text, files)?;
    print(&format!("Added to \"{}\".\n", meta.title));
    Ok(0)
}

async fn log(engine: &Arc<Engine>, o: Opts, p: Paint) -> Result<i32, String> {
    let meta = conversation(engine, &o)?;
    print(&p.c("1", &format!("{}\n\n", meta.title)));
    let mut feed = Feed::new(engine, &meta.id, Some(o.lines.unwrap_or(20)), p);
    if o.follow {
        loop {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(300)) => feed.poll(engine, p),
                _ = tokio::signal::ctrl_c() => break,
            }
        }
    }
    Ok(0)
}

fn ago(ms: u64) -> String {
    let mins = now_ms().saturating_sub(ms) / 60_000;
    match mins {
        0 => "just now".into(),
        1..=59 => format!("{mins} min ago"),
        60..=1439 => format!("{} h ago", mins / 60),
        _ => format!("{} days ago", mins / 1440),
    }
}

fn list(engine: &Engine, p: Paint) {
    let list = engine.store.list();
    if list.is_empty() {
        print("No conversations yet. Start one with consilium chat.\n");
        return;
    }
    for (i, m) in list.iter().enumerate() {
        let busy = if engine.busy(&m.id) { p.c("1;33", "  ● AIs answering") } else { String::new() };
        let folder = m.workspace.as_ref().map(|w| p.c("2", &format!("  [{w}]"))).unwrap_or_default();
        print(&format!("{:>3}  {}  {}{folder}{busy}\n", i + 1, m.title, p.c("2", &ago(m.updated))));
    }
}

fn new(engine: &Engine, o: Opts) -> Result<i32, String> {
    let title = if o.words.is_empty() { "New conversation".to_string() } else { o.words.join(" ") };
    let mut meta = engine.store.create(&title)?;
    if let Some(f) = &o.folder {
        meta = set_folder(engine, &meta.id, f)?;
    }
    print(&format!("Started \"{}\" (conversation 1 in consilium list).\n", meta.title));
    Ok(0)
}

async fn stop(engine: &Arc<Engine>, o: Opts) -> Result<i32, String> {
    let meta = conversation(engine, &o)?;
    if !engine.stop(&meta.id) {
        print(&format!("Nobody is answering in \"{}\".\n", meta.title));
        return Ok(0);
    }
    let started = std::time::Instant::now();
    while engine.busy(&meta.id) && started.elapsed() < engine::STOP_GRACE + Duration::from_secs(2) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    print(&format!("Stopped the AIs in \"{}\".\n", meta.title));
    Ok(0)
}

async fn status(engine: &Arc<Engine>, p: Paint) -> Result<i32, String> {
    let council = engine.config.get();
    let mut tasks = tokio::task::JoinSet::new();
    for a in agents::AGENTS.iter() {
        let (member, access) = (engine.config.is_member(a.id), engine.config.access(a.id));
        tasks.spawn(agents::status(a, member, access));
    }
    let mut infos = Vec::new();
    while let Some(Ok(i)) = tasks.join_next().await {
        infos.push(i);
    }
    infos.sort_by_key(|i| {
        (council.members.iter().position(|m| *m == i.id).unwrap_or(usize::MAX), agents::AGENTS.iter().position(|a| a.id == i.id))
    });
    print(&p.c("1", "Council\n"));
    for i in &infos {
        let state = if i.available {
            p.c("32", "ready")
        } else if i.path.is_some() {
            p.c("33", "not signed in")
        } else {
            p.c("2", "not installed")
        };
        let role = if i.member { format!("member, {:?} access", i.access).to_lowercase() } else { "not in the council".into() };
        print(&format!("  {:<8} {state}  {}\n", i.name, p.c("2", &role)));
        if !i.available && !i.hint.is_empty() && i.member {
            print(&p.c("2", &format!("           {}\n", i.hint)));
        }
    }
    // Plan usage: the model in use and the AI's whole plan, with reset times.
    let snapshot = crate::usage::refresh(engine).await;
    let now = now_ms();
    print(&p.c("1", "\nUsage left\n"));
    for id in &council.members {
        let Some(u) = snapshot.agents.get(id) else { continue };
        let line = |m: &agents::UsageMeter, what: &str| {
            let next = m.windows.iter().filter_map(|w| w.resets_at).filter(|t| *t > now).min();
            let reset = next.map(|t| format!(", next reset {}", crate::usage::when(t, now))).unwrap_or_default();
            format!("  {:<8} {:<34} {:>3}%{}\n", name_of(id), what, m.remaining_percent, p.c("2", &reset))
        };
        if let Some(m) = &u.model {
            let what = if m.shared { format!("{} (shares overall)", m.label) } else { m.label.clone() };
            print(&line(m, &what));
        }
        if let Some(o) = &u.overall {
            print(&line(o, "overall"));
        }
    }
    if let Some(f) = &snapshot.forecast {
        let chance = f.chance_24h.map(|c| format!("{c}% chance of a global Codex reset within 24 h")).unwrap_or_else(|| "no estimate".into());
        print(&format!("  {}\n", p.c("2", &format!("Forecast: {chance} (unofficial; data: {})", crate::usage::FORECAST_SITE))));
    }
    print(&format!(
        "\nDefault project folder: {}\nData: {}\n",
        council.default_workspace.as_deref().unwrap_or("none"),
        engine.store.dir().parent().unwrap_or(engine.store.dir()).display()
    ));
    let busy: Vec<String> = engine.running_ids();
    if !busy.is_empty() {
        let titles: Vec<String> = busy.iter().filter_map(|id| engine.store.meta(id).ok()).map(|m| m.title).collect();
        print(&format!("AIs answering in: {}\n", titles.join(", ")));
    }
    Ok(0)
}

async fn check(engine: &Arc<Engine>, p: Paint) -> Result<i32, String> {
    print("Asking every council member for a one-word reply…\n");
    let results = engine::self_check(engine).await;
    for r in &results {
        let mark = if r.ok { p.c("32", "ok ") } else { p.c("31", "failed") };
        print(&format!("  {:<8} {mark} {:.1}s  {}\n", r.name, r.ms as f64 / 1000.0, p.c("2", &r.detail)));
    }
    Ok(if results.iter().all(|r| r.ok) { 0 } else { 1 })
}

async fn chat(engine: &Arc<Engine>, term: &Arc<Term>, o: Opts, p: Paint) -> Result<i32, String> {
    let mut meta = conversation(engine, &o)?;
    let mut to = o.to.clone();
    let mut rounds = o.rounds.unwrap_or(1);
    *term.conversation.lock().unwrap() = meta.id.clone();
    let header = |meta: &ConversationMeta, to: &Option<Vec<String>>, rounds: u32| {
        let mode = if rounds > 1 { format!("discussing, up to {rounds} answers each") } else { "one answer each".into() };
        let folder = meta.workspace.as_deref().map(|w| format!(" · folder {w}")).unwrap_or_default();
        print(&format!(
            "{}\n{}\n\n",
            p.c("1", &meta.title),
            p.c("2", &format!("Asking {} ({mode}){folder} · /help for commands", names(&targets(engine, to))))
        ));
    };
    header(&meta, &to, rounds);
    let mut feed = Feed::new(engine, &meta.id, Some(o.lines.unwrap_or(10)), p);
    engine.resume_interrupted(Some(&meta.id));
    tokio::spawn(engine.clone().watch(false));

    // Lines typed in the terminal, read on their own thread so answers keep printing meanwhile.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Option<String>>();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            match line {
                Ok(l) => {
                    if tx.send(Some(l)).is_err() {
                        return;
                    }
                }
                Err(_) => break,
            }
        }
        let _ = tx.send(None);
    });

    let mut last_interrupt: Option<std::time::Instant> = None;
    // Files waiting to go with the next message.
    let mut pending: Vec<crate::attach::Attachment> = Vec::new();
    loop {
        tokio::select! {
            line = rx.recv() => {
                let Some(Some(line)) = line else { break };
                let line = line.trim().to_string();
                if line.is_empty() {
                    continue;
                }
                if let Some(cmd) = line.strip_prefix('/') {
                    let (name, rest) = cmd.split_once(' ').map_or((cmd, ""), |(a, b)| (a, b.trim()));
                    match name {
                        "quit" | "exit" | "q" => break,
                        "help" | "?" => print(&format!("{CHAT_HELP}\n")),
                        "to" => match agent_ids(rest) {
                            Ok(ids) => {
                                to = Some(ids);
                                print(&p.c("2", &format!("· Asking {}\n", names(&targets(engine, &to)))));
                            }
                            Err(e) => print(&p.c("31", &format!("! {e}\n"))),
                        },
                        "discuss" => {
                            rounds = rest.parse::<u32>().unwrap_or(3).clamp(2, engine::MAX_ROUNDS);
                            print(&p.c("2", &format!("· Discussing, up to {rounds} answers each\n")));
                        }
                        "ask" => {
                            rounds = 1;
                            print(&p.c("2", "· One answer each\n"));
                        }
                        "stop" => {
                            if !engine.stop(&meta.id) {
                                print(&p.c("2", "· Nobody is answering\n"));
                            }
                        }
                        "say" => match engine.say_with(&meta.id, rest, std::mem::take(&mut pending)) {
                            Ok(e) => {
                                feed.skip.insert(e.id);
                            }
                            Err(e) => print(&p.c("31", &format!("! {e}\n"))),
                        },
                        "attach" => {
                            let paths: Vec<PathBuf> = split_paths(rest).iter().map(|f| expand(f)).collect();
                            if paths.is_empty() {
                                let list: Vec<String> = pending.iter().map(|a| a.name.clone()).collect();
                                print(&p.c("2", &format!("· {}\n", if list.is_empty() { "Nothing attached. /attach FILE…".into() } else { format!("Attached: {}", list.join(", ")) })));
                            } else {
                                match engine.attach(&meta.id, &paths) {
                                    Ok(files) => {
                                        for a in &files {
                                            print(&p.c("2", &format!("· 📎 {} ({}, {}) goes with your next message\n", a.name, a.kind, human_size(a.size))));
                                        }
                                        pending.extend(files);
                                    }
                                    Err(e) => print(&p.c("31", &format!("! {e}\n"))),
                                }
                            }
                        }
                        "detach" => {
                            pending.clear();
                            print(&p.c("2", "· No files attached\n"));
                        }
                        "list" => list(engine, p),
                        "open" | "new" => {
                            let next = if name == "new" {
                                engine.store.create("New conversation")
                            } else {
                                find(&engine.store.list(), rest)
                            };
                            match next {
                                Ok(m) => {
                                    meta = m;
                                    // Attachments belong to the conversation they were made in.
                                    pending.clear();
                                    *term.conversation.lock().unwrap() = meta.id.clone();
                                    print("\n");
                                    header(&meta, &to, rounds);
                                    feed = Feed::new(engine, &meta.id, Some(10), p);
                                    engine.resume_interrupted(Some(&meta.id));
                                }
                                Err(e) => print(&p.c("31", &format!("! {e}\n"))),
                            }
                        }
                        "folder" => {
                            if rest.is_empty() {
                                print(&format!("· {}\n", meta.workspace.as_deref().unwrap_or("No project folder")));
                            } else {
                                match set_folder(engine, &meta.id, rest) {
                                    Ok(m) => {
                                        print(&p.c("2", &format!("· Project folder: {}\n", m.workspace.as_deref().unwrap_or("none"))));
                                        meta = m;
                                    }
                                    Err(e) => print(&p.c("31", &format!("! {e}\n"))),
                                }
                            }
                        }
                        "history" => {
                            let n = rest.parse().unwrap_or(10);
                            let skip = std::mem::take(&mut feed.skip);
                            feed = Feed::new(engine, &meta.id, Some(n), p);
                            feed.skip = skip;
                        }
                        "status" => {
                            let _ = status(engine, p).await;
                        }
                        other => print(&p.c("31", &format!("! no command /{other} (/help lists them)\n"))),
                    }
                    continue;
                }
                match engine.send_with(&meta.id, &line, std::mem::take(&mut pending), targets(engine, &to), rounds) {
                    Ok(e) => {
                        if !e.attachments.is_empty() {
                            let names: Vec<&str> = e.attachments.iter().map(|a| a.name.as_str()).collect();
                            print(&p.c("2", &format!("· Sent with {}\n", names.join(", "))));
                        }
                        feed.skip.insert(e.id);
                    }
                    Err(e) => print(&p.c("31", &format!("! {e}\n"))),
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(250)) => feed.poll(engine, p),
            _ = tokio::signal::ctrl_c() => {
                let again = last_interrupt.is_some_and(|t| t.elapsed() < Duration::from_secs(2));
                if engine.busy(&meta.id) && !again {
                    print(&p.c("2", "· Stopping the AIs… (Ctrl-C again to leave)\n"));
                    engine.stop(&meta.id);
                    last_interrupt = Some(std::time::Instant::now());
                } else {
                    break;
                }
            }
        }
    }
    feed.poll(engine, p);
    if engine.running_here(&meta.id) {
        print(&p.c("2", "· Stopping the AIs before leaving…\n"));
    }
    Ok(0)
}

// ---------------------------------------------------------------- installing the command

/// Whether the `consilium` command is installed for the terminal.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Install {
    pub installed: bool,
    /// Where the command is (or would be) installed.
    pub path: String,
    /// What to type to start.
    pub command: String,
    /// Anything the person needs to do, such as adding the folder to PATH.
    pub note: String,
}

/// The program the command starts: this one (for an AppImage, the AppImage itself rather than
/// its temporary mount).
fn program() -> Option<PathBuf> {
    std::env::var_os("APPIMAGE").map(PathBuf::from).or_else(|| std::env::current_exe().ok())
}

/// Where the command goes: a folder that is on PATH for new terminals by default where possible.
fn launcher_path() -> Option<PathBuf> {
    if cfg!(windows) {
        // On every user's PATH by default.
        dirs::data_local_dir().map(|d| d.join("Microsoft").join("WindowsApps").join("consilium.cmd"))
    } else {
        dirs::home_dir().map(|h| h.join(".local").join("bin").join("consilium"))
    }
}

fn launcher(program: &std::path::Path) -> String {
    let p = program.display().to_string();
    if cfg!(windows) {
        // `start /wait` keeps the terminal waiting while Consilium uses it.
        format!("@echo off\r\nstart \"\" /b /wait \"{p}\" %*\r\n")
    } else {
        let quoted = p.replace('\\', "\\\\").replace('"', "\\\"").replace('$', "\\$").replace('`', "\\`");
        format!("#!/bin/sh\n# The Consilium terminal command (installed by Consilium; `consilium install-cli` renews it).\nexec \"{quoted}\" \"$@\"\n")
    }
}

/// Whether new terminals will find a command in `dir`.
fn on_path(dir: &std::path::Path) -> bool {
    if std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d == dir)) {
        return true;
    }
    if cfg!(windows) {
        return false;
    }
    // An app started from the Dock doesn't see the shell's PATH, so look at the shell's settings.
    let Some(home) = dirs::home_dir() else { return false };
    [".zprofile", ".zshrc", ".bash_profile", ".bashrc", ".profile"]
        .iter()
        .filter_map(|f| std::fs::read_to_string(home.join(f)).ok())
        .any(|s| s.lines().any(|l| !l.trim_start().starts_with('#') && l.contains("PATH") && l.contains(".local/bin")))
}

pub fn install_status() -> Install {
    let path = launcher_path();
    let expected = program().map(|p| launcher(&p));
    let current = path.as_ref().and_then(|p| std::fs::read_to_string(p).ok());
    let installed = current.is_some() && current == expected;
    let dir = path.as_ref().and_then(|p| p.parent().map(|d| d.to_path_buf()));
    let note = match (&current, installed) {
        (Some(_), false) => "Installed for another copy of Consilium; install again to use this one.".to_string(),
        _ if dir.as_deref().is_some_and(|d| !on_path(d)) => path_note(),
        _ => String::new(),
    };
    Install {
        installed,
        path: path.map(|p| p.display().to_string()).unwrap_or_default(),
        command: "consilium chat".into(),
        note,
    }
}

fn path_note() -> String {
    if cfg!(windows) {
        "Open a new terminal window, then type consilium chat.".into()
    } else {
        "Your terminal doesn't look in ~/.local/bin yet. Run this once, then open a new terminal window:\n  echo 'export PATH=\"$HOME/.local/bin:$PATH\"' >> ~/.zprofile ~/.bash_profile".into()
    }
}

/// Installs (or renews) the `consilium` command: a two-line launcher for this program.
pub fn install() -> Result<Install, String> {
    let program = program().ok_or("couldn't tell where Consilium is installed")?;
    let path = launcher_path().ok_or("couldn't find your home folder")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("couldn't create {}: {e}", dir.display()))?;
    }
    std::fs::write(&path, launcher(&program)).map_err(|e| format!("couldn't write {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    }
    let mut status = install_status();
    if status.note.is_empty() {
        status.note = "Type consilium chat in a new terminal window to talk to the council.".into();
    }
    Ok(status)
}

/// The app is built as a windowed program on Windows, which starts without a console; as the
/// terminal command it uses the terminal it was started from (or opens one).
#[cfg(windows)]
fn attach_console() {
    use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};
    use windows_sys::Win32::System::Console::{
        AllocConsole, AttachConsole, GetStdHandle, SetStdHandle, ATTACH_PARENT_PROCESS, STD_ERROR_HANDLE, STD_INPUT_HANDLE,
        STD_OUTPUT_HANDLE,
    };
    // SAFETY: plain Win32 calls with valid, NUL-terminated arguments; handles are only stored.
    unsafe {
        let out = GetStdHandle(STD_OUTPUT_HANDLE);
        if !out.is_null() && out != INVALID_HANDLE_VALUE {
            return; // output already goes somewhere (a pipe, a file, or a console)
        }
        if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
            AllocConsole();
        }
        let open = |name: &str| {
            let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
            CreateFileW(
                wide.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        let input = open("CONIN$");
        if input != INVALID_HANDLE_VALUE {
            SetStdHandle(STD_INPUT_HANDLE, input);
        }
        let output = open("CONOUT$");
        if output != INVALID_HANDLE_VALUE {
            SetStdHandle(STD_OUTPUT_HANDLE, output);
            SetStdHandle(STD_ERROR_HANDLE, output);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn only_known_commands_start_the_terminal_version() {
        assert!(wants_cli(&args("chat")));
        assert!(wants_cli(&args("ask hello")));
        assert!(!wants_cli(&[]), "no command opens the app");
        assert!(!wants_cli(&args("-psn_0_12345")), "macOS launch arguments open the app");
    }

    #[test]
    fn options_and_message_words_are_parsed() {
        let o = parse(&args("hello there --to gpt,Claude -c 2 --discuss 3")).unwrap();
        assert_eq!(o.words, ["hello", "there"]);
        assert_eq!(o.to, Some(vec!["gpt".to_string(), "claude".to_string()]));
        assert_eq!(o.conversation.as_deref(), Some("2"));
        assert_eq!(o.rounds, Some(3));
        assert_eq!(parse(&args("--to all")).unwrap().to, Some(vec![]));
        assert_eq!(parse(&args("--to chatgpt")).unwrap().to, Some(vec!["gpt".to_string()]));
        assert!(parse(&args("--to nobody")).is_err());
        assert!(parse(&args("--bogus")).is_err());
        assert!(parse(&args("-c")).is_err(), "an option without its value");
        assert_eq!(parse(&args("-- --not-an-option")).unwrap().words, ["--not-an-option"]);
    }

    fn meta(id: &str, title: &str) -> ConversationMeta {
        ConversationMeta { id: id.into(), title: title.into(), created: 0, updated: 0, workspace: None }
    }

    #[test]
    fn a_conversation_is_found_by_number_id_or_title() {
        let list = vec![
            meta("aaaaaaaa-0000-0000-0000-000000000000", "Thumbnail latency"),
            meta("bbbbbbbb-0000-0000-0000-000000000000", "Release plan"),
            meta("cccccccc-0000-0000-0000-000000000000", "Release notes"),
        ];
        assert_eq!(find(&list, "2").unwrap().title, "Release plan");
        assert_eq!(find(&list, "cccc").unwrap().title, "Release notes");
        assert_eq!(find(&list, "thumbnail").unwrap().title, "Thumbnail latency");
        assert!(find(&list, "release").unwrap_err().contains("matches 2"));
        assert!(find(&list, "9").is_err());
        assert!(find(&list, "0").is_err());
        assert!(find(&list, "zebra").is_err());
    }

    #[test]
    fn dragged_in_and_quoted_paths_are_split_right() {
        assert_eq!(split_paths(r#"a.png "My Doc.docx" 'b c.pdf'"#), ["a.png", "My Doc.docx", "b c.pdf"]);
        if !cfg!(windows) {
            assert_eq!(split_paths(r"/Users/me/Screen\ Shot\ 1.png  ~/x.txt"), ["/Users/me/Screen Shot 1.png", "~/x.txt"]);
        }
        assert!(split_paths("   ").is_empty());
        assert!(parse(&args("--attach a.png -a b.txt hi")).unwrap().attach == ["a.png", "b.txt"]);
    }

    #[test]
    fn the_launcher_starts_this_program_with_the_arguments() {
        let l = launcher(std::path::Path::new("/Applications/Consilium.app/Contents/MacOS/consilium"));
        if cfg!(windows) {
            assert!(l.contains("start \"\" /b /wait") && l.contains("%*"));
        } else {
            assert!(l.starts_with("#!/bin/sh\n"));
            assert!(l.contains("exec \"/Applications/Consilium.app/Contents/MacOS/consilium\" \"$@\""));
            let odd = launcher(std::path::Path::new("/tmp/a \"b\" $c"));
            assert!(odd.contains(r#"exec "/tmp/a \"b\" \$c" "$@""#), "{odd}");
        }
    }

    #[test]
    fn requests_stand_out_and_names_are_shown() {
        let p = Paint { on: false };
        let e = Event {
            id: "x".into(),
            seq: 1,
            ts: 0,
            conversation: "c".into(),
            sender: "claude".into(),
            kind: "message".into(),
            to: vec![],
            text: "Done.\nREQUEST: pick a name".into(),
            requests: vec![],
            add_ai: vec![],
            usage: None,
            attachments: vec![],
        };
        assert_eq!(render(p, &e), "Claude\n  Done.\n  REQUEST: pick a name\n\n");
        let to_both = Event { sender: "human".into(), to: vec!["gpt".into(), "claude".into()], text: "hi".into(), ..e };
        assert_eq!(render(p, &to_both), "You → ChatGPT, Claude\n  hi\n\n");
    }
}
