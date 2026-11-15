//! Consilium's back end: conversations, agents, and turn-taking, exposed to the UI as Tauri commands.
//! The same binary is also the `consilium` terminal command (see `cli`); both run turns through
//! `engine`, on one shared data folder.

mod agents;
mod apiagent;
mod attach;
mod bench;
pub mod cli;
mod config;
mod council;
mod engine;
mod memory;
mod pressure;
mod prompt;
mod redact;
mod resume;
mod setup;
mod store;
mod usage;

use config::Access;
use engine::{Engine, TurnState};
use std::collections::HashMap;
use std::sync::Arc;
use store::{ConversationMeta, Event};
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};

/// The app's bundle identifier; its data folder is named after it (see `data_dir`).
pub const IDENTIFIER: &str = "io.github.ag3nt47.consilium";

/// Where conversations, settings and memory live: the same folder for the app and the terminal
/// command. (This is how Tauri names an app's data folder.) CONSILIUM_DATA points both at another
/// folder, for trying things out without touching real conversations.
pub fn data_dir() -> Option<std::path::PathBuf> {
    data_override().or_else(|| dirs::data_dir().map(|d| d.join(IDENTIFIER)))
}

fn data_override() -> Option<std::path::PathBuf> {
    std::env::var_os("CONSILIUM_DATA").filter(|v| !v.is_empty()).map(std::path::PathBuf::from)
}

struct AppState {
    engine: Arc<Engine>,
}

/// Shows what the engine does in the app's window.
struct Window(AppHandle);

impl engine::Ui for Window {
    fn event(&self, e: &Event) {
        let _ = self.0.emit(EVENT, e);
    }
    fn turn(&self, t: TurnState) {
        let _ = self.0.emit(TURN, t);
    }
    fn activity(&self, activity: &engine::AgentActivity) {
        let _ = self.0.emit(ACTIVITY, activity);
    }
}

const EVENT: &str = "consilium://event";
const TURN: &str = "consilium://turn";
const ACTIVITY: &str = "consilium://activity";

/// Every known AI with its install and sign-in state, council membership and access level.
#[tauri::command]
async fn agents_status(state: State<'_, AppState>) -> Result<Vec<agents::AgentInfo>, String> {
    let config = state.engine.config.clone();
    // Every AI is checked at the same time, so a slow CLI doesn't hold up the others.
    let mut tasks = tokio::task::JoinSet::new();
    for agent in agents::AGENTS.iter() {
        let member = config.is_member(agent.id);
        let access = config.access(agent.id);
        tasks.spawn(async move { agents::status(agent, member, access).await });
    }
    let mut out = Vec::new();
    let now = store::now_ms();
    while let Some(result) = tasks.join_next().await {
        let mut info = result.map_err(|e| format!("AI status check failed: {e}"))?;
        info.benched_until = state.engine.bench.until(&info.id, now);
        out.push(info);
    }
    sort_agents(&mut out, &config.get().members, |a| a.id.as_str());
    Ok(out)
}

/// Members first, in council order; then everyone else in the fixed catalogue order, since the
/// parallel status checks finish in whatever order they happen to.
fn sort_agents<T>(items: &mut [T], members: &[String], id: impl Fn(&T) -> &str) {
    let catalogue = |id: &str| agents::AGENTS.iter().position(|a| a.id == id).unwrap_or(usize::MAX);
    items.sort_by_key(|a| {
        let id = id(a);
        (members.iter().position(|m| m == id).unwrap_or(usize::MAX), catalogue(id))
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn agents_sort_members_first_then_catalogue_order() {
        let members = vec!["claude".to_string(), "gpt".to_string()];
        // Arrival order of the parallel checks varies; the result must not.
        for start in [
            ["gemini", "gpt", "claude"],
            ["gpt", "claude", "gemini"],
            ["claude", "gemini", "gpt"],
        ] {
            let mut ids: Vec<&str> = start.to_vec();
            super::sort_agents(&mut ids, &members, |s| *s);
            assert_eq!(ids, ["claude", "gpt", "gemini"]);
        }
        // With no members, the catalogue order (gpt, claude, gemini) applies.
        let mut ids = vec!["gemini", "claude", "gpt"];
        super::sort_agents(&mut ids, &[], |s| *s);
        assert_eq!(ids, ["gpt", "claude", "gemini"]);
    }
}

/// Remaining plan usage for each council member that reports it, keyed by AI id. Separate from
/// `agents_status` so the slow usage probes never delay sign-in state.
#[tauri::command]
async fn agents_usage(state: State<'_, AppState>) -> Result<HashMap<String, agents::UsageStatus>, String> {
    // Saved for the AIs too, and for the terminal command (see `usage`).
    Ok(usage::refresh(&state.engine).await.agents)
}

/// codex-reset.com's forecast of the next global Codex reset, when that setting is on.
#[tauri::command]
fn reset_forecast(state: State<AppState>) -> Option<usage::Forecast> {
    if !state.engine.config.get().reset_forecast {
        return None;
    }
    usage::load(&state.engine.data).forecast
}

#[tauri::command]
fn set_reset_forecast(state: State<AppState>, on: bool) -> Result<(), String> {
    state.engine.config.set_reset_forecast(on).map(|_| ())
}

/// Adds an AI to the council or removes it. The UI asks the person to confirm first, since a new
/// member receives the conversations it is addressed in.
#[tauri::command]
fn set_member(state: State<AppState>, agent: String, member: bool) -> Result<(), String> {
    state.engine.config.set_member(&agent, member).map(|_| ())
}

/// Lets an AI that ran out of usage be asked again now, without waiting for its reset time.
#[tauri::command]
fn unbench(state: State<AppState>, agent: String) {
    state.engine.bench.clear(&agent);
}

#[tauri::command]
fn set_access(state: State<AppState>, agent: String, access: Access) -> Result<(), String> {
    state.engine.config.set_access(&agent, access).map(|_| ())
}

#[tauri::command]
fn set_default_access(state: State<AppState>, access: Access) -> Result<(), String> {
    state.engine.config.set_default_access(access).map(|_| ())
}

#[tauri::command]
fn get_council(state: State<AppState>) -> config::Council {
    state.engine.config.get()
}

#[tauri::command]
fn set_default_workspace(state: State<AppState>, folder: Option<String>) -> Result<Option<String>, String> {
    state.engine.config.set_default_workspace(folder.as_deref()).map(|c| c.default_workspace)
}

#[tauri::command]
fn get_default_workspace(state: State<AppState>) -> Option<String> {
    state.engine.config.get().default_workspace
}

#[tauri::command]
fn get_memory(state: State<AppState>) -> String {
    state.engine.memory.read()
}

#[tauri::command]
fn set_memory(state: State<AppState>, text: String) -> Result<(), String> {
    state.engine.memory.write(&text)
}

#[tauri::command]
fn set_workspace(state: State<AppState>, id: String, folder: Option<String>) -> Result<ConversationMeta, String> {
    state.engine.store.set_workspace(&id, folder.as_deref())
}

/// Provider-reported token totals for the open conversation and across all conversations since
/// local midnight. The front end supplies its local-day boundary because Rust has no UI timezone.
#[tauri::command]
fn token_usage_report(
    state: State<AppState>,
    id: String,
    day_start: u64,
) -> Result<store::TokenUsageReport, String> {
    state.engine.store.token_usage_report(&id, day_start)
}

#[tauri::command]
fn list_conversations(state: State<AppState>) -> Vec<ConversationMeta> {
    state.engine.store.list()
}

#[tauri::command]
fn new_conversation(state: State<AppState>) -> Result<ConversationMeta, String> {
    state.engine.store.create("New conversation")
}

#[tauri::command]
fn load_conversation(state: State<AppState>, id: String) -> Result<Vec<Event>, String> {
    state.engine.store.load(&id)
}

#[tauri::command]
fn rename_conversation(state: State<AppState>, id: String, title: String) -> Result<ConversationMeta, String> {
    state.engine.store.rename(&id, &title)
}

#[tauri::command]
fn delete_conversation(state: State<AppState>, id: String) -> Result<(), String> {
    if state.engine.busy(&id) {
        return Err("stop the conversation before deleting it".into());
    }
    state.engine.store.delete(&id)
}

/// Voice chat changes how every AI should answer, so switching it posts a Consilium note that all
/// of them read. The UI works out whether voice is on from the latest of these notes.
const VOICE_ON: &str = "Voice chat is on: the human hears your replies read aloud and speaks their messages, \
which arrive transcribed and may contain recognition mistakes. Answer in short, natural spoken sentences, \
under about 80 words unless asked for more. Don't use markdown, code blocks, tables, lists or URLs; \
describe code and file changes in words. Keep REQUEST: lines short yes/no questions.";
const VOICE_OFF: &str = "Voice chat is off: the human is reading and typing again. Write normally.";

#[tauri::command]
fn set_voice(state: State<AppState>, id: String, on: bool) -> Result<Event, String> {
    let text = if on { VOICE_ON } else { VOICE_OFF };
    state.engine.note(&id, text)
}

#[tauri::command]
fn export_markdown(state: State<AppState>, id: String) -> Result<String, String> {
    let meta = state.engine.store.meta(&id)?;
    let mut md = format!("# {}\n\n", meta.title);
    for e in state.engine.store.load(&id)?.iter().filter(|e| e.kind == "message") {
        md.push_str(&format!("**{}:**\n\n{}\n\n---\n\n", prompt::label(&e.sender), e.text));
    }
    Ok(md)
}

/// Records the human's message, then asks the addressed council members (see `Engine::send`).
/// Sending while the AIs are answering doesn't interrupt them; Stop is what interrupts.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
async fn send_message(
    state: State<'_, AppState>,
    id: String,
    text: String,
    targets: Vec<String>,
    rounds: u32,
    interrupt: Option<bool>,
    attachments: Option<Vec<attach::Attachment>>,
) -> Result<Event, String> {
    // `interrupt` is accepted from older UIs and ignored: sending never interrupts.
    let _ = interrupt;
    state.engine.send_with(&id, &text, attachments.unwrap_or_default(), targets, rounds)
}

/// Saves files (chosen, or dropped on the window) for the next message in `id`.
#[tauri::command]
async fn attach_files(state: State<'_, AppState>, id: String, paths: Vec<String>) -> Result<Vec<attach::Attachment>, String> {
    let paths: Vec<std::path::PathBuf> = paths.into_iter().map(std::path::PathBuf::from).collect();
    state.engine.attach(&id, &paths)
}

/// Saves pasted file contents (a screenshot, say) for the next message in `id`.
#[tauri::command]
async fn attach_data(state: State<'_, AppState>, id: String, name: String, data: Vec<u8>) -> Result<attach::Attachment, String> {
    state.engine.attach_bytes(&id, &name, &data)
}

/// Opens an attachment of conversation `id` in its usual app.
#[tauri::command]
fn open_attachment(app: AppHandle, state: State<AppState>, id: String, attachment: attach::Attachment) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    if !attach::belongs(&state.engine.store.files_dir(&id), &attachment) {
        return Err("that isn't one of this conversation's attachments".into());
    }
    app.opener().open_path(&attachment.path, None::<&str>).map_err(|e| e.to_string())
}

/// Shown to the human and the AIs after an update to this version.
const WHATS_NEW: &str = "What's new: REQUEST lines have Approve and Deny buttons; a Voice Chat / Text Chat \
switch; Grok and DeepSeek can join through Add AI with an API key; secrets are masked before anything is saved; \
stalled CLI prompts become a REQUEST instead of a freeze; a devil's-advocate reviewer from round 3; a glass theme, \
a Cmd+K command palette and live working/frozen lights on each AI.";

/// The version that last ran is kept in the app's data folder; after an update, a note in the
/// conversations with a turn to resume, and in the most recent one, tells the human and the AIs
/// what changed.
fn announce_update(engine: &Engine, data: &std::path::Path) {
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
    let mut ids: Vec<String> = engine.journal.pending_conversations();
    let list = engine.store.list();
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
        let _ = engine.note(&id, &note);
    }
}

/// Self-check: asks every council member, at the same time, for a one-word reply, with chat-only
/// access in an empty folder. Each check costs about one short message of that AI's allowance.
#[tauri::command]
async fn self_check(state: State<'_, AppState>) -> Result<Vec<engine::CheckResult>, String> {
    Ok(engine::self_check(&state.engine).await)
}

/// Stops the AIs in a conversation. Returns at once; the conversation is free again as soon as
/// they have shut down, and after STOP_GRACE at the latest.
#[tauri::command]
fn stop(state: State<AppState>, id: String) -> bool {
    state.engine.stop(&id)
}

/// Conversations whose AIs are working, here or in the terminal.
#[tauri::command]
fn running(state: State<AppState>) -> Vec<String> {
    state.engine.running_ids()
}

/// Whether the `consilium` terminal command is installed, and where.
#[tauri::command]
fn cli_status() -> cli::Install {
    cli::install_status()
}

/// Installs the `consilium` terminal command (a small launcher for this app).
#[tauri::command]
fn install_cli() -> Result<cli::Install, String> {
    cli::install()
}

/// Opens a terminal that installs or signs in an agent, after the person presses Enter.
/// `action` is "install" or "signin"; the command itself is chosen by the back end.
#[tauri::command]
fn setup_agent(agent: String, action: String) -> Result<String, String> {
    let step = setup::step(&agent, &action)?;
    setup::open_in_terminal(&step)?;
    Ok(step.title.to_string())
}

/// Saves an API-key agent's key (empty removes it). The key arrives from the Add AI window, is
/// stored only on this computer and is never sent back to the frontend.
#[tauri::command]
fn set_api_key(agent: String, key: String) -> Result<(), String> {
    let a = agents::by_id(&agent).ok_or("unknown agent")?;
    if agents::api_provider(a.kind).is_none() {
        return Err(format!("{} signs in through its own app, not a key", a.name));
    }
    apiagent::set_key(a.id, &key)
}

/// Samples host PC CPU and memory pressure (0-100%, green to red).
#[tauri::command]
fn system_pressure() -> pressure::SystemPressure {
    pressure::sample()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let data = match data_override() {
                Some(d) => d,
                None => app.path().app_data_dir()?,
            };
            let ui = Arc::new(Window(app.handle().clone()));
            let spawn = Box::new(|task: engine::Task| {
                tauri::async_runtime::spawn(task);
            });
            let engine = Arc::new(Engine::open(&data, ui, spawn)?);
            announce_update(&engine, &data);
            engine.resume_interrupted(None);
            // Keeps the window in step with the terminal command.
            tauri::async_runtime::spawn(engine.clone().watch(true));
            app.manage(AppState { engine });
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
            set_voice,
            send_message,
            stop,
            running,
            setup_agent,
            set_api_key,
            system_pressure,
            self_check,
            unbench,
            cli_status,
            install_cli,
            reset_forecast,
            set_reset_forecast,
            attach_files,
            attach_data,
            open_attachment
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
