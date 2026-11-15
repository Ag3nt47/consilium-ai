//! Consilium's back end: conversations, agents, and turn-taking, exposed to the UI as Tauri commands.

mod agents;
mod config;
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

/// An answer that @mentions another council member brings it into the same round, at most this
/// many times per round, so AIs can ask each other things without looping.
pub const MAX_PULLED_IN: usize = 2;

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
    let mut tasks = tokio::task::JoinSet::new();
    for agent in agents::AGENTS.iter() {
        let member = config.is_member(agent.id);
        let access = config.access(agent.id);
        tasks.spawn(async move { agents::status(agent, member, access).await });
    }
    let mut out = Vec::new();
    while let Some(result) = tasks.join_next().await {
        out.push(result.map_err(|e| format!("AI status check failed: {e}"))?);
    }
    // Members first, in council order; then everyone else in the fixed catalogue order, since the
    // parallel checks above finish in whatever order they happen to.
    let order = config.get().members;
    let catalogue = |id: &str| {
        agents::AGENTS
            .iter()
            .position(|a| a.id == id)
            .unwrap_or(usize::MAX)
    };
    out.sort_by_key(|a| {
        (
            order.iter().position(|m| *m == a.id).unwrap_or(usize::MAX),
            catalogue(&a.id),
        )
    });
    Ok(out)
}

/// Remaining plan usage for each council member that reports it, keyed by AI id. Separate from
/// `agents_status` so the slow usage probes never delay sign-in state.
#[tauri::command]
async fn agents_usage(
    state: State<'_, AppState>,
) -> Result<HashMap<String, agents::UsageStatus>, String> {
    let config = state.config.clone();
    let mut tasks = tokio::task::JoinSet::new();
    for agent in agents::AGENTS.iter().filter(|a| config.is_member(a.id)) {
        tasks.spawn(async move { (agent.id.to_string(), agents::usage_of(agent).await) });
    }
    let mut out = HashMap::new();
    while let Some(result) = tasks.join_next().await {
        if let (id, Some(usage)) = result.map_err(|e| format!("Usage check failed: {e}"))? {
            out.insert(id, usage);
        }
    }
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
fn set_default_access(state: State<AppState>, access: Access) -> Result<(), String> {
    state.config.set_default_access(access).map(|_| ())
}

#[tauri::command]
fn get_council(state: State<AppState>) -> config::Council {
    state.config.get()
}

#[tauri::command]
fn set_default_workspace(
    state: State<AppState>,
    folder: Option<String>,
) -> Result<Option<String>, String> {
    state
        .config
        .set_default_workspace(folder.as_deref())
        .map(|c| c.default_workspace)
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
fn set_workspace(
    state: State<AppState>,
    id: String,
    folder: Option<String>,
) -> Result<ConversationMeta, String> {
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

/// Provider-reported token totals for the open conversation and across all conversations since
/// local midnight. The front end supplies its local-day boundary because Rust has no UI timezone.
#[tauri::command]
fn token_usage_report(
    state: State<AppState>,
    id: String,
    day_start: u64,
) -> Result<store::TokenUsageReport, String> {
    state.store.token_usage_report(&id, day_start)
}

#[tauri::command]
fn rename_conversation(
    state: State<AppState>,
    id: String,
    title: String,
) -> Result<ConversationMeta, String> {
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
    for e in state
        .store
        .load(&id)?
        .iter()
        .filter(|e| e.kind == "message")
    {
        md.push_str(&format!(
            "**{}:**\n\n{}\n\n---\n\n",
            prompt::label(&e.sender),
            e.text
        ));
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
    let mut targets: Vec<String> = targets
        .into_iter()
        .filter(|t| config.is_member(t))
        .collect();
    for m in prompt::mentioned_agents(&text) {
        if config.is_member(&m) && !targets.contains(&m) {
            targets.push(m);
        }
    }
    let rounds = rounds.clamp(1, MAX_ROUNDS);

    let (event, token) = {
        let mut running = state.running.lock().unwrap();
        if running.contains_key(&id) {
            return Err(
                "the agents are still answering; press Stop, or send again to interrupt".into(),
            );
        }
        let event = state
            .store
            .append(&id, "human", "message", targets.clone(), &text)?;
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
    let (app, store, config, journal, memory, running) = (
        app.clone(),
        state.store.clone(),
        state.config.clone(),
        state.journal.clone(),
        state.memory.clone(),
        state.running.clone(),
    );
    tauri::async_runtime::spawn(async move {
        let conv = pending.conversation.clone();
        run_turns(&app, &store, &config, &journal, &memory, &pending, &token).await;
        journal.finish(&conv);
        running.lock().unwrap().remove(&conv);
        let _ = app.emit(
            TURN,
            TurnState {
                conversation: conv,
                state: "idle".into(),
                agent: None,
                round: 0,
                rounds: pending.rounds,
            },
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
        let _ = state.store.append(
            &p.conversation,
            "human",
            "system",
            vec![],
            "Consilium restarted; the council is carrying on where it left off.",
        );
        let token = CancellationToken::new();
        state
            .running
            .lock()
            .unwrap()
            .insert(p.conversation.clone(), token.clone());
        start_turns(app, state, p, token);
    }
}

async fn run_turns(
    app: &AppHandle,
    store: &Store,
    config: &Config,
    journal: &Journal,
    memory: &Memory,
    pending: &Pending,
    cancel: &CancellationToken,
) {
    let (conv, targets, rounds) = (
        pending.conversation.as_str(),
        &pending.targets,
        pending.rounds,
    );
    let members = config.get().members;
    for round in pending.round..=rounds {
        let mut passes = 0;
        let mut answered = 0;
        let mut asked_human = false;
        let skip = if round == pending.round {
            pending.index
        } else {
            0
        };
        // Who answers this round: the addressed AIs, plus any a reply @mentions (see MAX_PULLED_IN).
        let mut queue: Vec<String> = targets.clone();
        let mut pulled_in = 0;
        let mut index = skip;
        while index < queue.len() {
            if cancel.is_cancelled() {
                return;
            }
            let addressed = index < targets.len();
            if addressed {
                journal.at(conv, round, index);
            }
            let Some(agent) = agents::by_id(&queue[index]) else {
                index += 1;
                continue;
            };
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
            // A conversation without its own folder uses the council's default one, if set.
            let workspace = store
                .meta(conv)
                .ok()
                .and_then(|m| m.workspace)
                .or_else(|| config.get().default_workspace)
                .filter(|w| std::path::Path::new(w).is_dir());
            let access = if workspace.is_some() {
                config.access(agent.id)
            } else {
                Access::Chat
            };
            let notes = workspace
                .as_deref()
                .and_then(|w| memory::project_notes(std::path::Path::new(w)));
            let shared = memory.for_prompt();
            let ctx = prompt::Context {
                members: &members,
                workspace: workspace.as_deref(),
                access,
                memory: shared.as_deref(),
                project_notes: notes.as_deref(),
            };
            let system = prompt::system(agent.id, round, rounds, &ctx);
            let transcript = prompt::transcript(&events, agent.id);
            let ws_path = workspace.as_ref().map(PathBuf::from);
            let event = match agents::run_with_usage(
                agent,
                access,
                ws_path.as_deref(),
                &system,
                &transcript,
                cancel,
            )
            .await
            {
                Ok(reply) if rounds > 1 && prompt::is_pass(&reply.text) => {
                    passes += 1;
                    store.append_with_usage(
                        conv,
                        agent.id,
                        "system",
                        vec![],
                        &format!("{} had nothing to add.", agent.name),
                        reply.usage,
                    )
                }
                Ok(reply) => {
                    let to = prompt::mentioned_agents(&reply.text);
                    for m in &to {
                        if pulled_in < MAX_PULLED_IN
                            && m != agent.id
                            && members.contains(m)
                            && !queue[index + 1..].contains(m)
                        {
                            queue.push(m.clone());
                            pulled_in += 1;
                        }
                    }
                    let ev = store.append_with_usage(
                        conv,
                        agent.id,
                        "message",
                        to,
                        &reply.text,
                        reply.usage,
                    );
                    if ev.as_ref().is_ok_and(|e| !e.requests.is_empty()) {
                        asked_human = true;
                    }
                    let facts = memory::remember_lines(&reply.text);
                    if !facts.is_empty() {
                        let date = today();
                        if let Ok(added) = memory.remember(agent.name, &date, &facts) {
                            if !added.is_empty() {
                                if let Ok(e) = ev.as_ref() {
                                    let _ = app.emit(EVENT, e);
                                }
                                let note = format!(
                                    "{} saved to council memory: {}",
                                    agent.name,
                                    added.join("; ")
                                );
                                if let Ok(n) = store.append(conv, agent.id, "system", vec![], &note)
                                {
                                    let _ = app.emit(EVENT, &n);
                                }
                            }
                        }
                    }
                    ev
                }
                Err(why) if why == "stopped" => {
                    let ev = store.append(
                        conv,
                        agent.id,
                        "system",
                        vec![],
                        &format!("Stopped {}.", agent.name),
                    );
                    if let Ok(e) = ev {
                        let _ = app.emit(EVENT, &e);
                    }
                    return;
                }
                Err(why) => store.append(
                    conv,
                    agent.id,
                    "error",
                    vec![],
                    &format!("{} couldn't answer: {why}", agent.name),
                ),
            };
            if let Ok(e) = event {
                let _ = app.emit(EVENT, &e);
            }
            answered += 1;
            if addressed {
                let (next_round, next_index) = pending.after(round, index);
                journal.at(conv, next_round, next_index);
            }
            index += 1;
        }
        if rounds > 1 && (asked_human || passes == answered) {
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
        let Some(agent) = agents::by_id(&id) else {
            continue;
        };
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
            let memory = Memory::new(data.join("memory.md"));
            app.manage(AppState {
                store: Arc::new(store),
                config: Arc::new(config),
                journal: Arc::new(journal),
                memory: Arc::new(memory),
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
            agents_usage,
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
            token_usage_report,
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
