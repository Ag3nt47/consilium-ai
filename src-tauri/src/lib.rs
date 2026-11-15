//! Consilium's back end: conversations, agents, and turn-taking, exposed to the UI as Tauri commands.

mod agents;
mod bench;
mod config;
mod council;
mod memory;
mod prompt;
mod resume;
mod setup;
mod store;

use config::{Access, Config};
use memory::Memory;
use resume::{Journal, Pending};
use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use store::{ConversationMeta, Event, Store};
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};
use tokio_util::sync::CancellationToken;

/// Upper bound on discussion rounds, whatever the UI asks for, so agents can't loop indefinitely.
pub const MAX_ROUNDS: u32 = 8;

/// How long Stop or an interrupt waits for a turn's AIs to shut down before letting it go anyway,
/// so the conversation is never stuck as "still answering".
const STOP_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

#[cfg(test)]
mod tests {
    #[test]
    fn today_is_a_plausible_iso_date() {
        let t = super::today();
        assert_eq!(t.len(), 10);
        assert!(t.starts_with("20") && t.as_bytes()[4] == b'-' && t.as_bytes()[7] == b'-');
    }
}

struct AppState {
    store: Arc<Store>,
    config: Arc<Config>,
    /// Where each running turn has got to, so it can resume after a quit or crash.
    journal: Arc<Journal>,
    /// Council memory shared by every AI across conversations.
    memory: Arc<Memory>,
    /// AIs that ran out of usage, and until when.
    bench: Arc<bench::Bench>,
        /// Conversations with a turn in progress.
        running: Arc<Mutex<HashMap<String, Running>>>,
    }

    /// A turn in progress in a conversation.
    #[derive(Clone)]
    struct Running {
        /// The turn's id (see `resume::Pending::turn`).
        turn: String,
        token: CancellationToken,
        /// Takes the human's further messages while the turn runs.
        council: Arc<council::Council>,
    }

#[derive(Clone, Serialize)]
struct TurnState {
    conversation: String,
    /// "running" or "idle".
    state: String,
    /// The first AI working on an answer, if any.
    agent: Option<String>,
    /// Every AI working on an answer right now; they answer at the same time.
    agents: Vec<String>,
    round: u32,
    rounds: u32,
}

const EVENT: &str = "consilium://event";
const TURN: &str = "consilium://turn";

/// Every known AI with its install and sign-in state, council membership and access level.
#[tauri::command]
async fn agents_status(state: State<'_, AppState>) -> Result<Vec<agents::AgentInfo>, String> {
    let config = state.config.clone();
    let mut out = Vec::new();
    let now = store::now_ms();
    for a in agents::AGENTS.iter() {
        let mut info = agents::status(a, config.is_member(a.id), config.access(a.id)).await;
        info.benched_until = state.bench.until(a.id, now);
        out.push(info);
    }
    // Members first, in council order.
    let order = config.get().members;
    out.sort_by_key(|a| order.iter().position(|m| *m == a.id).unwrap_or(usize::MAX));
    Ok(out)
}

/// Adds an AI to the council or removes it. The UI asks the person to confirm first, since a new
/// member receives the conversations it is addressed in.
#[tauri::command]
fn set_member(state: State<AppState>, agent: String, member: bool) -> Result<(), String> {
    state.config.set_member(&agent, member).map(|_| ())
}

/// Lets an AI that ran out of usage be asked again now, without waiting for its reset time.
#[tauri::command]
fn unbench(state: State<AppState>, agent: String) {
    state.bench.clear(&agent);
}

#[tauri::command]
fn set_access(state: State<AppState>, agent: String, access: Access) -> Result<(), String> {
    state.config.set_access(&agent, access).map(|_| ())
}

#[tauri::command]
fn set_default_access(state: State<AppState>, access: Access) -> Result<(), String> {
    state.config.set_default_access(access).map(|_| ())
}

#[tauri::command]
fn get_council(state: State<AppState>) -> config::Council {
    state.config.get()
}

#[tauri::command]
fn set_default_workspace(state: State<AppState>, folder: Option<String>) -> Result<Option<String>, String> {
    state.config.set_default_workspace(folder.as_deref()).map(|c| c.default_workspace)
}

#[tauri::command]
fn get_default_workspace(state: State<AppState>) -> Option<String> {
    state.config.get().default_workspace
}

#[tauri::command]
fn get_memory(state: State<AppState>) -> String {
    state.memory.read()
}

#[tauri::command]
fn set_memory(state: State<AppState>, text: String) -> Result<(), String> {
    state.memory.write(&text)
}

#[tauri::command]
fn set_workspace(state: State<AppState>, id: String, folder: Option<String>) -> Result<ConversationMeta, String> {
    state.store.set_workspace(&id, folder.as_deref())
}

#[tauri::command]
fn list_conversations(state: State<AppState>) -> Vec<ConversationMeta> {
    state.store.list()
}

#[tauri::command]
fn new_conversation(state: State<AppState>) -> Result<ConversationMeta, String> {
    state.store.create("New conversation")
}

#[tauri::command]
fn load_conversation(state: State<AppState>, id: String) -> Result<Vec<Event>, String> {
    state.store.load(&id)
}

#[tauri::command]
fn rename_conversation(state: State<AppState>, id: String, title: String) -> Result<ConversationMeta, String> {
    state.store.rename(&id, &title)
}

#[tauri::command]
fn delete_conversation(state: State<AppState>, id: String) -> Result<(), String> {
    if state.running.lock().unwrap().contains_key(&id) {
        return Err("stop the conversation before deleting it".into());
    }
    state.store.delete(&id)
}

#[tauri::command]
fn export_markdown(state: State<AppState>, id: String) -> Result<String, String> {
    let meta = state.store.meta(&id)?;
    let mut md = format!("# {}\n\n", meta.title);
    for e in state.store.load(&id)?.iter().filter(|e| e.kind == "message") {
        md.push_str(&format!("**{}:**\n\n{}\n\n---\n\n", prompt::label(&e.sender), e.text));
    }
    Ok(md)
}

/// Records the human's message, then asks the addressed council members, who all answer at the
/// same time and react to each other's messages as they arrive (see `council`).
///
/// `targets` are agent ids. `rounds` is 1 for a single answer from each; more lets each AI answer
/// up to that many times as the discussion goes on, up to MAX_ROUNDS. A discussion ends when
/// nobody has anything new to react to, when every AI passes, or when any AI asks the human
/// something (a REQUEST: line).
///
/// Sending while the AIs are answering doesn't interrupt them: the message joins the running turn,
/// every AI reads it (one in the middle of an answer, as soon as it has finished), and those it
/// addresses who weren't taking part join in. Stop is what interrupts.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
async fn send_message(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    text: String,
    targets: Vec<String>,
    rounds: u32,
    interrupt: Option<bool>,
) -> Result<Event, String> {
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err("the message is empty".into());
    }
    // `interrupt` is accepted from older UIs and ignored: sending never interrupts.
    let _ = interrupt;
    let config = state.config.clone();
    let mut targets: Vec<String> = targets.into_iter().filter(|t| config.is_member(t)).collect();
    for m in prompt::mentioned_agents(&text) {
        if config.is_member(&m) && !targets.contains(&m) {
            targets.push(m);
        }
    }
    let rounds = rounds.clamp(1, MAX_ROUNDS);

    let (event, pending, live) = {
        let mut running = state.running.lock().unwrap();
        let event = state.store.append(&id, "human", "message", targets.clone(), &text)?;
        let _ = app.emit(EVENT, &event);
        if let Some(r) = running.get(&id) {
            if r.council.join(&event, &targets) {
                state.journal.join(&id, &r.turn, &targets);
                return Ok(event);
            }
            // That turn has just ended; this message starts a new one.
        }
        if targets.is_empty() {
            return Ok(event);
        }
        let pending = Pending::new(&id, targets, rounds);
        let live = prepare(&app, &state, &pending);
        running.insert(id.clone(), live.clone());
        (event, pending, live)
    };
    start_turns(&app, &state, pending, live);
    Ok(event)
}

/// Stops the turn running in `conversation`, if any, and waits for its AIs to shut down. One that
/// hasn't finished after STOP_GRACE is let go: the conversation is free again either way, and the
/// old turn can no longer change what is recorded for it.
async fn stop_and_wait(app: &AppHandle, state: &AppState, conversation: &str) -> bool {
    let entry = state.running.lock().unwrap().get(conversation).cloned();
    let Some(Running { turn, token, .. }) = entry else { return false };
    token.cancel();
    let started = std::time::Instant::now();
    while started.elapsed() < STOP_GRACE {
        let still = state.running.lock().unwrap().get(conversation).is_some_and(|r| r.turn == turn);
        if !still {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    release(app, &state.running, &state.journal, conversation, &turn);
    true
}

/// Marks the turn `turn` as over: the conversation can take new messages, and it won't resume.
/// Does nothing to a newer turn that has replaced it.
fn release(
    app: &AppHandle,
    running: &Mutex<HashMap<String, Running>>,
    journal: &Journal,
    conversation: &str,
    turn: &str,
) {
    journal.finish(conversation, turn);
    let mut r = running.lock().unwrap();
    if r.get(conversation).is_some_and(|r| r.turn == turn) {
        r.remove(conversation);
        drop(r);
        let _ = app.emit(
            TURN,
            TurnState { conversation: conversation.into(), state: "idle".into(), agent: None, agents: vec![], round: 0, rounds: 0 },
        );
    }
}

/// Sets up a turn; `start_turns` runs it. The caller records it in `running` first, so a turn
/// that ends at once can't leave the conversation marked as running.
fn prepare(app: &AppHandle, state: &AppState, pending: &Pending) -> Running {
    let host = Arc::new(AppHost {
        app: app.clone(),
        store: state.store.clone(),
        config: state.config.clone(),
        memory: state.memory.clone(),
        journal: state.journal.clone(),
        conversation: pending.conversation.clone(),
        turn: pending.turn.clone(),
        rounds: pending.rounds,
    });
    let turn = council::Turn {
        conversation: pending.conversation.clone(),
        targets: pending.targets.clone(),
        rounds: pending.rounds,
        members: state.config.get().members,
    };
    let token = CancellationToken::new();
    let council = council::Council::new(turn, state.store.clone(), state.bench.clone(), host, token.clone());
    Running { turn: pending.turn.clone(), token, council: Arc::new(council) }
}

/// Runs a turn in the background. Its journal entry is removed when it ends or is stopped; if the
/// app quits first, the entry stays and the turn continues on the next launch.
fn start_turns(app: &AppHandle, state: &AppState, pending: Pending, live: Running) {
    state.journal.begin(pending.clone());
    let (app, journal, running) = (app.clone(), state.journal.clone(), state.running.clone());
    tauri::async_runtime::spawn(async move {
        live.council.run().await;
        release(&app, &running, &journal, &pending.conversation, &pending.turn);
    });
}

/// Continues turns that a quit, crash, restart or update cut short. This always runs at launch.
fn resume_interrupted(app: &AppHandle, state: &AppState) {
    let (resume, give_up) = state.journal.interrupted();
    for p in give_up {
        if let Ok(e) = state.store.append(
            &p.conversation,
            council::CONSILIUM,
            "system",
            vec![],
            "Couldn't resume the AIs' turn after two tries. Send your message again to continue.",
        ) {
            let _ = app.emit(EVENT, &e);
        }
    }
    for mut p in resume {
        if state.store.meta(&p.conversation).is_err() {
            state.journal.finish(&p.conversation, &p.turn);
            continue;
        }
        if p.turn.is_empty() {
            // Recorded by a version without turn ids.
            state.journal.finish(&p.conversation, "");
            p.turn = uuid::Uuid::new_v4().to_string();
        }
        if let Ok(e) = state.store.append(
            &p.conversation,
            council::CONSILIUM,
            "system",
            vec![],
            "Consilium restarted; the council is carrying on where it left off.",
        ) {
            let _ = app.emit(EVENT, &e);
        }
        let live = prepare(app, state, &p);
        state.running.lock().unwrap().insert(p.conversation.clone(), live.clone());
        start_turns(app, state, p, live);
    }
}

/// Shown to the human and the AIs after an update to this version.
const WHATS_NEW: &str = "What's new: the human can now send messages while you are answering without \
interrupting anyone; each new message joins the discussion and you all read it and respond. Every AI answers \
as soon as it's ready and reacts to new messages on \
its own time, instead of waiting for a turn. An AI that runs out of usage sits out until its limit resets \
while the others carry on, and another member takes its turn if nobody else is answering. Stop and \
interrupting now always work within a few seconds, even if an AI left a program running. Conversations \
cut short by a restart resume automatically.";

/// The version that last ran is kept in the app's data folder; after an update, a note in the
/// conversations with a turn to resume, and in the most recent one, tells the human and the AIs
/// what changed.
fn announce_update(app: &AppHandle, state: &AppState, data: &std::path::Path) {
    let file = data.join("last-version.txt");
    let now = env!("CARGO_PKG_VERSION");
    let before = std::fs::read_to_string(&file).unwrap_or_default();
    let _ = std::fs::write(&file, now);
    let before = before.trim();
    if before == now {
        return;
    }
    // The first run of a version that records this: an existing install is being updated if it
    // already has conversations.
    let before = if before.is_empty() { "the previous version" } else { before };
    let mut ids: Vec<String> = state.journal.pending_conversations();
    let list = state.store.list();
    if list.is_empty() {
        return;
    }
    if let Some(latest) = list.first() {
        if !ids.contains(&latest.id) {
            ids.push(latest.id.clone());
        }
    }
    let note = format!("Consilium was updated from {before} to {now} and restarted. {WHATS_NEW}");
    for id in ids {
        if let Ok(e) = state.store.append(&id, council::CONSILIUM, "system", vec![], &note) {
            let _ = app.emit(EVENT, &e);
        }
    }
}

/// How a turn reaches the real AIs and the UI.
struct AppHost {
    app: AppHandle,
    store: Arc<Store>,
    config: Arc<Config>,
    memory: Arc<Memory>,
    journal: Arc<Journal>,
    conversation: String,
    turn: String,
    rounds: u32,
}

impl council::Host for AppHost {
    fn ask(
        &self,
        agent: &'static agents::Agent,
        answer: u32,
        rounds: u32,
        events: Vec<Event>,
        cancel: CancellationToken,
    ) -> council::Reply {
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
        let access = if workspace.is_some() { self.config.access(agent.id) } else { Access::Chat };
        let notes = workspace.as_deref().and_then(|w| memory::project_notes(std::path::Path::new(w)));
        let shared = self.memory.for_prompt();
        let members = self.config.get().members;
        let ctx = prompt::Context {
            members: &members,
            workspace: workspace.as_deref(),
            access,
            memory: shared.as_deref(),
            project_notes: notes.as_deref(),
        };
        let system = prompt::system(agent.id, answer, rounds, &ctx);
        let transcript = prompt::transcript(&events, agent.id);
        let ws_path = workspace.map(PathBuf::from);
        Box::pin(async move { agents::run(agent, access, ws_path.as_deref(), &system, &transcript, &cancel).await })
    }

    fn event(&self, e: &Event) {
        if e.kind == "message" && e.sender != "human" {
            self.journal.progressed(&self.conversation, &self.turn);
        }
        let _ = self.app.emit(EVENT, e);
    }

    fn thinking(&self, agents: Vec<String>, round: u32) {
        let _ = self.app.emit(
            TURN,
            TurnState {
                conversation: self.conversation.clone(),
                state: "running".into(),
                agent: agents.first().cloned(),
                agents,
                round,
                rounds: self.rounds,
            },
        );
    }

    fn installed(&self, agent: &'static agents::Agent) -> bool {
        agents::find(agents::effective_kind(agent)).is_some()
    }

    fn remember(&self, agent: &'static agents::Agent, reply: &str) -> Option<String> {
        let facts = memory::remember_lines(reply);
        if facts.is_empty() {
            return None;
        }
        let added = self.memory.remember(agent.name, &today(), &facts).ok()?;
        (!added.is_empty()).then(|| format!("{} saved to council memory: {}", agent.name, added.join("; ")))
    }
}

/// Today's date as YYYY-MM-DD (UTC), for memory notes.
fn today() -> String {
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

#[derive(Clone, Serialize)]
struct CheckResult {
    id: String,
    name: String,
    ok: bool,
    /// How long the answer took, in milliseconds.
    ms: u64,
    /// The reply (trimmed) or the error.
    detail: String,
}

/// Self-check: asks every council member, at the same time, for a one-word reply, with chat-only
/// access in an empty folder. Each check costs about one short message of that AI's allowance.
#[tauri::command]
async fn self_check(state: State<'_, AppState>) -> Result<Vec<CheckResult>, String> {
    let members = state.config.get().members;
    let mut tasks = Vec::new();
    for id in members {
        let Some(agent) = agents::by_id(&id) else { continue };
        let bench = state.bench.clone();
        tasks.push(tauri::async_runtime::spawn(async move {
            let system = "This is an automatic health check from Consilium, not a conversation. Don't use tools \
or run commands. Reply with exactly the word OK and nothing else.";
            let started = std::time::Instant::now();
            let token = CancellationToken::new();
            let r = tokio::time::timeout(
                std::time::Duration::from_secs(180),
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
                    if let Some(until) = bench::usage_limit(&e, store::now_ms()) {
                        bench.bench(agent.id, until);
                    }
                    (false, e)
                }
                Err(_) => {
                    token.cancel();
                    (false, "no answer within 3 minutes".into())
                }
            };
            CheckResult { id: agent.id.into(), name: agent.name.into(), ok, ms, detail }
        }));
    }
    let mut out = Vec::new();
    for t in tasks {
        if let Ok(r) = t.await {
            out.push(r);
        }
    }
    Ok(out)
}

/// Stops the AIs in a conversation. Returns at once; the conversation is free again as soon as
/// they have shut down, and after STOP_GRACE at the latest.
#[tauri::command]
fn stop(app: AppHandle, state: State<AppState>, id: String) -> bool {
    let running = state.running.lock().unwrap().contains_key(&id);
    if running {
        let app2 = app.clone();
        tauri::async_runtime::spawn(async move {
            let state = app2.state::<AppState>();
            stop_and_wait(&app2, &state, &id).await;
        });
    }
    running
}

#[tauri::command]
fn running(state: State<AppState>) -> Vec<String> {
    state.running.lock().unwrap().keys().cloned().collect()
}

/// Opens a terminal that installs or signs in an agent, after the person presses Enter.
/// `action` is "install" or "signin"; the command itself is chosen by the back end.
#[tauri::command]
fn setup_agent(agent: String, action: String) -> Result<String, String> {
    let step = setup::step(&agent, &action)?;
    setup::open_in_terminal(&step)?;
    Ok(step.title.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let data = app.path().app_data_dir()?;
            let store = Store::new(data.join("conversations"))?;
            let config = Config::load(data.join("council.json"));
            let journal = Journal::load(data.join("turns.json"));
            let memory = Memory::new(data.join("memory.md"));
            app.manage(AppState {
                store: Arc::new(store),
                config: Arc::new(config),
                journal: Arc::new(journal),
                memory: Arc::new(memory),
                bench: Arc::new(bench::Bench::new()),
                running: Arc::new(Mutex::new(HashMap::new())),
            });
            let state = app.state::<AppState>();
            announce_update(app.handle(), &state, &data);
            resume_interrupted(app.handle(), &state);
            Ok(())
        })
        // On macOS, closing the window hides it and the AIs keep working; the Dock icon brings it
        // back, and Quit (Cmd+Q) quits. Elsewhere closing quits, and the turn resumes next launch.
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                if cfg!(target_os = "macos") {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            agents_status,
            set_member,
            set_access,
            set_workspace,
            set_default_workspace,
            get_default_workspace,
            set_default_access,
            get_council,
            get_memory,
            set_memory,
            list_conversations,
            new_conversation,
            load_conversation,
            rename_conversation,
            delete_conversation,
            export_markdown,
            send_message,
            stop,
            running,
            setup_agent,
            self_check,
            unbench
        ])
        .build(tauri::generate_context!())
        .expect("error while starting Consilium")
        .run(|app, event| {
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen { .. } = event {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.show();
                    let _ = w.set_focus();
                }
            }
            let _ = (app, event);
        });
}
