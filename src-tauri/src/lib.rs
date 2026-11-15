//! Consilium's back end: conversations, agents, and turn-taking, exposed to the UI as Tauri commands.

mod agents;
mod config;
mod prompt;
mod resume;
mod setup;
mod store;

use config::{Access, Config};
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

struct AppState {
    store: Arc<Store>,
    config: Arc<Config>,
    /// Where each running turn has got to, so it can resume after a quit or crash.
    journal: Arc<Journal>,
    /// Conversations with a turn in progress, and how to stop it.
    running: Arc<Mutex<HashMap<String, CancellationToken>>>,
}

#[derive(Clone, Serialize)]
struct TurnState {
    conversation: String,
    /// "running" or "idle".
    state: String,
    /// The agent currently being asked, if any.
    agent: Option<String>,
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
    for a in agents::AGENTS.iter() {
        out.push(agents::status(a, config.is_member(a.id), config.access(a.id)).await);
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

#[tauri::command]
fn set_access(state: State<AppState>, agent: String, access: Access) -> Result<(), String> {
    state.config.set_access(&agent, access).map(|_| ())
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

/// Records the human's message, then asks the addressed council members in turn.
///
/// `targets` are agent ids in the order they should answer. `rounds` is 1 for a single answer from
/// each; more makes the agents discuss among themselves, up to MAX_ROUNDS. A discussion ends early
/// when every agent passes, or when any agent asks the human something (a REQUEST: line).
///
/// With `interrupt`, a turn already running in this conversation is stopped first (its CLI and
/// everything it started are killed) and the new message is sent once that has finished.
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
    if interrupt.unwrap_or(false) {
        let token = state.running.lock().unwrap().get(&id).cloned();
        if let Some(token) = token {
            token.cancel();
            // The cancelled turn removes itself once its CLI is gone; wait up to 5 seconds for that.
            for _ in 0..100 {
                if !state.running.lock().unwrap().contains_key(&id) {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        }
    }
    let config = state.config.clone();
    let mut targets: Vec<String> = targets.into_iter().filter(|t| config.is_member(t)).collect();
    for m in prompt::mentioned_agents(&text) {
        if config.is_member(&m) && !targets.contains(&m) {
            targets.push(m);
        }
    }
    let rounds = rounds.clamp(1, MAX_ROUNDS);

    let (event, token) = {
        let mut running = state.running.lock().unwrap();
        if running.contains_key(&id) {
            return Err("the agents are still answering; press Stop, or send again to interrupt".into());
        }
        let event = state.store.append(&id, "human", "message", targets.clone(), &text)?;
        let _ = app.emit(EVENT, &event);
        if targets.is_empty() {
            return Ok(event);
        }
        let token = CancellationToken::new();
        running.insert(id.clone(), token.clone());
        (event, token)
    };
    start_turns(&app, &state, Pending::new(&id, targets, rounds), token);
    Ok(event)
}

/// Runs a turn in the background, recording its position in the journal as it goes. The journal
/// entry is removed when the turn ends or is stopped; if the app quits first, it stays, and the
/// turn continues on the next launch.
fn start_turns(app: &AppHandle, state: &AppState, pending: Pending, token: CancellationToken) {
    state.journal.begin(pending.clone());
    let (app, store, config, journal, running) = (
        app.clone(),
        state.store.clone(),
        state.config.clone(),
        state.journal.clone(),
        state.running.clone(),
    );
    tauri::async_runtime::spawn(async move {
        let conv = pending.conversation.clone();
        run_turns(&app, &store, &config, &journal, &pending, &token).await;
        journal.finish(&conv);
        running.lock().unwrap().remove(&conv);
        let _ = app.emit(
            TURN,
            TurnState { conversation: conv, state: "idle".into(), agent: None, round: 0, rounds: pending.rounds },
        );
    });
}

/// Continues turns that a quit or crash cut short.
fn resume_interrupted(app: &AppHandle, state: &AppState) {
    let (resume, give_up) = state.journal.interrupted();
    for p in give_up {
        let _ = state.store.append(
            &p.conversation,
            "human",
            "system",
            vec![],
            "Couldn't resume the AIs' turn after two tries. Send your message again to continue.",
        );
    }
    for p in resume {
        if state.store.meta(&p.conversation).is_err() {
            state.journal.finish(&p.conversation);
            continue;
        }
        let _ = state.store.append(&p.conversation, "human", "system", vec![], "Consilium restarted; the council is carrying on where it left off.");
        let token = CancellationToken::new();
        state.running.lock().unwrap().insert(p.conversation.clone(), token.clone());
        start_turns(app, state, p, token);
    }
}

async fn run_turns(
    app: &AppHandle,
    store: &Store,
    config: &Config,
    journal: &Journal,
    pending: &Pending,
    cancel: &CancellationToken,
) {
    let (conv, targets, rounds) = (pending.conversation.as_str(), &pending.targets, pending.rounds);
    let members = config.get().members;
    for round in pending.round..=rounds {
        let mut passes = 0;
        let mut asked_human = false;
        let skip = if round == pending.round { pending.index } else { 0 };
        for (index, agent_id) in targets.iter().enumerate().skip(skip) {
            if cancel.is_cancelled() {
                return;
            }
            journal.at(conv, round, index);
            let Some(agent) = agents::by_id(agent_id) else { continue };
            let _ = app.emit(
                TURN,
                TurnState {
                    conversation: conv.into(),
                    state: "running".into(),
                    agent: Some(agent.id.into()),
                    round,
                    rounds,
                },
            );
            let events = store.load(conv).unwrap_or_default();
            // Read the folder fresh each turn, so a change in the UI applies from the next answer.
            let workspace = store.meta(conv).ok().and_then(|m| m.workspace);
            let access = if workspace.is_some() { config.access(agent.id) } else { Access::Chat };
            let ctx = prompt::Context { members: &members, workspace: workspace.as_deref(), access };
            let system = prompt::system(agent.id, round, rounds, &ctx);
            let transcript = prompt::transcript(&events, agent.id);
            let ws_path = workspace.as_ref().map(PathBuf::from);
            let event = match agents::run(agent, access, ws_path.as_deref(), &system, &transcript, cancel).await {
                Ok(reply) if rounds > 1 && prompt::is_pass(&reply) => {
                    passes += 1;
                    store.append(conv, agent.id, "system", vec![], &format!("{} had nothing to add.", agent.name))
                }
                Ok(reply) => {
                    let to = prompt::mentioned_agents(&reply);
                    let ev = store.append(conv, agent.id, "message", to, &reply);
                    if ev.as_ref().is_ok_and(|e| !e.requests.is_empty()) {
                        asked_human = true;
                    }
                    ev
                }
                Err(why) if why == "stopped" => {
                    let ev = store.append(conv, agent.id, "system", vec![], &format!("Stopped {}.", agent.name));
                    if let Ok(e) = ev {
                        let _ = app.emit(EVENT, &e);
                    }
                    return;
                }
                Err(why) => store.append(conv, agent.id, "error", vec![], &format!("{} couldn't answer: {why}", agent.name)),
            };
            if let Ok(e) = event {
                let _ = app.emit(EVENT, &e);
            }
            let (next_round, next_index) = pending.after(round, index);
            journal.at(conv, next_round, next_index);
        }
        if rounds > 1 && (asked_human || passes == targets.len() - skip) {
            let note = if asked_human {
                "Discussion paused: a question for you is waiting."
            } else {
                "Discussion ended: nobody had more to add."
            };
            if let Ok(e) = store.append(conv, "human", "system", vec![], note) {
                let _ = app.emit(EVENT, &e);
            }
            return;
        }
    }
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
                Ok(Ok(reply)) => (true, reply.chars().take(80).collect()),
                Ok(Err(e)) => (false, e),
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

#[tauri::command]
fn stop(state: State<AppState>, id: String) -> bool {
    match state.running.lock().unwrap().get(&id) {
        Some(token) => {
            token.cancel();
            true
        }
        None => false,
    }
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
            app.manage(AppState {
                store: Arc::new(store),
                config: Arc::new(config),
                journal: Arc::new(journal),
                running: Arc::new(Mutex::new(HashMap::new())),
            });
            let state = app.state::<AppState>();
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
            self_check
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
