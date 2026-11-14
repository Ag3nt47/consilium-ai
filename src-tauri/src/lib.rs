//! Consilium's back end: conversations, agents, and turn-taking, exposed to the UI as Tauri commands.

mod agents;
mod prompt;
mod setup;
mod store;

use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use store::{ConversationMeta, Event, Store};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio_util::sync::CancellationToken;

/// Upper bound on discussion rounds, whatever the UI asks for, so agents can't loop indefinitely.
pub const MAX_ROUNDS: u32 = 8;

struct AppState {
    store: Arc<Store>,
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

#[tauri::command]
async fn agents_status() -> Vec<agents::AgentInfo> {
    let mut out = Vec::new();
    for a in agents::AGENTS.iter() {
        out.push(agents::status(a).await);
    }
    out
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

/// Records the human's message, then asks the addressed agents in turn.
///
/// `targets` are agent ids in the order they should answer. `rounds` is 1 for a single answer from
/// each; more makes the agents discuss among themselves, up to MAX_ROUNDS. A discussion ends early
/// when every agent passes, or when any agent asks the human something (a REQUEST: line).
#[tauri::command]
fn send_message(
    app: AppHandle,
    state: State<AppState>,
    id: String,
    text: String,
    targets: Vec<String>,
    rounds: u32,
) -> Result<Event, String> {
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err("the message is empty".into());
    }
    let mut targets: Vec<String> = targets.into_iter().filter(|t| agents::by_id(t).is_some()).collect();
    for m in prompt::mentioned_agents(&text) {
        if !targets.contains(&m) {
            targets.push(m);
        }
    }
    let rounds = rounds.clamp(1, MAX_ROUNDS);

    let cancel = {
        let mut running = state.running.lock().unwrap();
        if running.contains_key(&id) {
            return Err("the agents are still answering; wait, or press Stop".into());
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
    let (event, token) = cancel;
    let store = state.store.clone();
    let running = state.running.clone();
    let conv = id.clone();
    tauri::async_runtime::spawn(async move {
        run_turns(&app, &store, &conv, &targets, rounds, &token).await;
        running.lock().unwrap().remove(&conv);
        let _ = app.emit(TURN, TurnState { conversation: conv, state: "idle".into(), agent: None, round: 0, rounds });
    });
    Ok(event)
}

async fn run_turns(
    app: &AppHandle,
    store: &Store,
    conv: &str,
    targets: &[String],
    rounds: u32,
    cancel: &CancellationToken,
) {
    for round in 1..=rounds {
        let mut passes = 0;
        let mut asked_human = false;
        for agent_id in targets {
            if cancel.is_cancelled() {
                return;
            }
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
            let system = prompt::system(agent.id, round, rounds);
            let transcript = prompt::transcript(&events, agent.id);
            let event = match agents::run(agent, &system, &transcript, cancel).await {
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
        }
        if rounds > 1 && (asked_human || passes == targets.len()) {
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

/// Opens a terminal that installs or signs in an agent, after the person presses Enter.
/// `action` is "install" or "signin"; the command itself is chosen by the back end.
#[tauri::command]
fn setup_agent(agent: String, action: String) -> Result<String, String> {
    let step = setup::step(&agent, &action)?;
    setup::open_in_terminal(&step)?;
    Ok(step.title.to_string())
}

#[tauri::command]
fn running(state: State<AppState>) -> Vec<String> {
    state.running.lock().unwrap().keys().cloned().collect()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let dir = app.path().app_data_dir()?.join("conversations");
            let store = Store::new(dir)?;
            app.manage(AppState { store: Arc::new(store), running: Arc::new(Mutex::new(HashMap::new())) });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            agents_status,
            list_conversations,
            new_conversation,
            load_conversation,
            rename_conversation,
            delete_conversation,
            export_markdown,
            send_message,
            stop,
            running,
            setup_agent
        ])
        .run(tauri::generate_context!())
        .expect("error while running Consilium");
}
