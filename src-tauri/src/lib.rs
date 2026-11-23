//! Consilium's back end: conversations, agents, and turn-taking, exposed to the UI as Tauri commands.
//! The same binary is also the `consilium` terminal command (see `cli`); both run turns through
//! `engine`, on one shared data folder.

mod agents;
mod apiagent;
mod attach;
mod autocontinue;
mod bench;
mod build_id;
mod ci;
pub mod auto_run;
pub mod bridge;
pub mod cli;
pub mod escalation;
mod config;
mod customai;
mod council;
mod engine;
mod follow;
mod link;
mod liveui;
mod memory;
mod offload;
pub mod ollama;
pub mod peer_protocol;
pub mod resources;
mod peer_sync;
mod pressure;
mod projects;
pub mod watchdog;
mod prompt;
mod reaper;
mod repo;
mod runcmd;
mod redact;
mod resume;
mod setup;
mod skills;
mod stall;
mod signal_chat;
mod signal_setup;
mod startup_check;
mod safety;
mod searchpath;
mod security_scan;
mod store;
mod tasks;
mod runbooks;
mod toolbox;
mod update;
mod usage;
mod claude_resets;
mod switched;
pub mod wallpaper;
mod worktree;

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
    security: Arc<security_scan::Scanner>,
}

/// Shows what the engine does in the app's window.
struct Window(AppHandle);

impl engine::Ui for Window {
    fn event(&self, e: &Event) {
        let _ = self.0.emit(EVENT, e);
        peer_sync::share(e);
        if let Some(state) = self.0.try_state::<AppState>() {
            auto_run_commands(state.engine.clone(), e);
        }
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
    for agent in agents::all().into_iter() {
        let member = config.is_member(agent.id);
        let access = config.access(agent.id);
        tasks.spawn(async move { agents::status(agent, member, access).await });
    }
    let mut out = Vec::new();
    let now = store::now_ms();
    while let Some(result) = tasks.join_next().await {
        let mut info = result.map_err(|e| format!("AI status check failed: {e}"))?;
        info.benched_until = state.engine.bench.until(&info.id, now);
        info.reset_known = info.benched_until.is_some() && state.engine.bench.reset_known(&info.id);
        info.model = config.model(&info.id);
        out.push(info);
    }
    sort_agents(&mut out, &config.get().members, |a| a.id.as_str());
    Ok(out)
}

/// Members first, in council order; then everyone else in the fixed catalogue order, since the
/// parallel status checks finish in whatever order they happen to.
fn sort_agents<T>(items: &mut [T], members: &[String], id: impl Fn(&T) -> &str) {
    let catalogue = |id: &str| agents::all().into_iter().position(|a| a.id == id).unwrap_or(usize::MAX);
    items.sort_by_key(|a| {
        let id = id(a);
        (members.iter().position(|m| m == id).unwrap_or(usize::MAX), catalogue(id))
    });
}

#[cfg(test)]
mod open_app_tests {
    use super::valid_app_name;

    #[test]
    fn app_names_cannot_carry_flags_paths_or_shell_syntax() {
        assert!(valid_app_name("Safari"));
        assert!(valid_app_name("Visual Studio Code"));
        assert!(valid_app_name("Notepad++"));
        assert!(!valid_app_name(""));
        assert!(!valid_app_name("-n Safari"));
        assert!(!valid_app_name("/bin/sh"));
        assert!(!valid_app_name("Safari; rm -rf ~"));
        assert!(!valid_app_name("x' -Verb RunAs"));
        assert!(!valid_app_name("calc & del"));
        assert!(!valid_app_name("x$(id)"));
        assert!(!valid_app_name(&"a".repeat(61)));
    }
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

/// claude-resets.com's history of Claude Code resets, when the reset-forecast setting is on.
#[tauri::command]
fn claude_reset_history(state: State<AppState>) -> Option<claude_resets::ClaudeResets> {
    if !state.engine.config.get().reset_forecast {
        return None;
    }
    usage::load(&state.engine.data).claude_resets
}

/// codex-reset.com's forecast of the next global Codex reset, when that setting is on.
#[tauri::command]
fn reset_forecast(state: State<AppState>) -> Option<usage::Forecast> {
    if !state.engine.config.get().reset_forecast {
        return None;
    }
    usage::load(&state.engine.data).forecast
}

/// "Always allow" on a REQUEST: saves the exact elevated/install command(s) it names, so every AI
/// may run them on this computer from now on without asking again. Only the human's click reaches
/// this; requests without a machine-readable command are refused.
#[tauri::command]
fn save_approval(state: State<AppState>, request: String) -> Result<config::Council, String> {
    let grants = prompt::grants_in_request(request.trim());
    if grants.is_empty() {
        return Err("this request has no exact command to remember".into());
    }
    state.engine.config.save_grants(&grants)
}

/// Settings → Tools: the catalog of not-bundled tools, with install state and pinned commands.
#[tauri::command]
fn tool_catalog() -> Vec<toolbox::CatalogEntry> {
    toolbox::catalog(data_dir().as_deref())
}

#[tauri::command]
async fn local_tool_inventory() -> Vec<String> {
    toolbox::inventory().into_iter().map(|(name, _)| name).collect()
}

#[tauri::command]
fn forget_approval(state: State<AppState>, grant: String) -> Result<config::Council, String> {
    state.engine.config.forget_grant(&grant)
}

#[tauri::command]
fn set_auto_continue(state: State<AppState>, on: bool) -> Result<(), String> {
    state.engine.config.set_auto_continue(on).map(|_| ())
}

/// Switches an AI on or off in one conversation (the Start/Stop button in its council box).
#[tauri::command]
fn set_ai_on(state: State<AppState>, conversation: String, agent: String, on: bool) -> Result<Vec<String>, String> {
    state.engine.set_ai_on(&conversation, &agent, on)
}

/// "Auto-run this chat" on or off for one conversation (after the warning). A note tells the AIs.
#[tauri::command]
fn set_chat_auto_run(state: State<AppState>, conversation: String, on: bool) -> Result<bool, String> {
    let now = auto_run::set(state.engine.store.dir(), &conversation, on)?;
    let text = if now {
        "Auto-run is on in this chat: every command box an AI posts here runs by itself, including ones that delete, overwrite, install or need admin rights, and the output goes back to it. The human can turn it off with the red Auto-run badge."
    } else {
        "Auto-run is off in this chat: command boxes wait for the human's Run click again."
    };
    let _ = state.engine.note(&conversation, text);
    if now {
        catch_up_auto_run(state.engine.clone(), conversation.clone());
    }
    Ok(now)
}

/// Whether "Auto-run this chat" is on for `conversation`.
#[tauri::command]
fn chat_auto_run(state: State<AppState>, conversation: String) -> bool {
    auto_run::is_on(state.engine.store.dir(), &conversation)
}

/// The AIs switched off in one conversation.
#[tauri::command]
fn switched_off(state: State<AppState>, conversation: String) -> Vec<String> {
    state.engine.switched_off(&conversation)
}

#[tauri::command]
fn set_auto_run_commands(state: State<AppState>, on: bool) -> Result<(), String> {
    state.engine.config.set_auto_run_commands(on).map(|_| ())
}

#[tauri::command]
fn set_isolate_agents(state: State<AppState>, on: bool) -> Result<(), String> {
    state.engine.config.set_isolate_agents(on).map(|_| ())
}

/// The project folder a conversation uses (its own, else the council default).
fn workspace_for(state: &AppState, conversation: Option<String>) -> Result<std::path::PathBuf, String> {
    conversation
        .and_then(|c| state.engine.store.meta(&c).ok())
        .and_then(|m| m.workspace)
        .or_else(|| state.engine.config.get().default_workspace)
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_dir())
        .ok_or_else(|| "no project folder is set".to_string())
}

/// Each AI's isolated copy of the project and what merging it would bring in.
#[tauri::command]
async fn agent_branches(state: State<'_, AppState>, conversation: Option<String>) -> Result<Vec<worktree::BranchInfo>, String> {
    let ws = workspace_for(&state, conversation)?;
    let data = state.engine.data.clone();
    tokio::task::spawn_blocking(move || worktree::list(&data, &ws)).await.map_err(|e| e.to_string())
}

/// Merges an AI's branch into the person's checkout. The UI asks first.
#[tauri::command]
async fn merge_agent_branch(state: State<'_, AppState>, conversation: Option<String>, repo: String, agent: String) -> Result<String, String> {
    let ws = workspace_for(&state, conversation)?;
    tokio::task::spawn_blocking(move || worktree::merge(&ws, &repo, &agent)).await.map_err(|e| e.to_string())?
}

/// Deletes an AI's copy and branch, unmerged work included. The UI asks first.
#[tauri::command]
async fn discard_agent_branch(state: State<'_, AppState>, conversation: Option<String>, repo: String, agent: String) -> Result<(), String> {
    let ws = workspace_for(&state, conversation)?;
    let data = state.engine.data.clone();
    tokio::task::spawn_blocking(move || worktree::discard(&data, &ws, &repo, &agent)).await.map_err(|e| e.to_string())?
}

#[tauri::command]
fn set_reset_forecast(state: State<AppState>, on: bool) -> Result<(), String> {
    state.engine.config.set_reset_forecast(on).map(|_| ())
}

/// Adds an AI to the council or removes it. The UI asks the person to confirm first, since a new
/// member receives the conversations it is addressed in.
#[tauri::command]
fn set_member(state: State<AppState>, agent: String, member: bool) -> Result<(), String> {
    let e = &state.engine;
    let joined = member && !e.config.get().members.contains(&agent);
    e.config.set_member(&agent, member)?;
    // A new AI: tell the council in the most recently used conversation, and, if no reset
    // forecast is known for it, ask one AI to look for one on the web.
    if joined {
        if let (Some(a), Some(c)) = (agents::by_id(&agent), e.store.list().into_iter().next()) {
            let text = usage::join_note(&agent, a.name);
            let _ = if usage::forecast_source(&agent).is_some() { e.note(&c.id, &text) } else { e.note_for_council(&c.id, &text) };
        }
    }
    Ok(())
}

/// Lets an AI that ran out of usage be asked again now, without waiting for its reset time.
#[tauri::command]
fn unbench(state: State<AppState>, agent: String) {
    state.engine.bench.clear(&agent);
}

#[tauri::command]
fn set_access(state: State<AppState>, agent: String, access: Access) -> Result<(), String> {
    // A waiting Antigravity process was started with the old access; it would never be used.
    agents::drop_spares();
    state.engine.config.set_access(&agent, access).map(|_| ())
}

/// The model one AI uses from its next turn (#29); None goes back to its CLI's default.
#[tauri::command]
async fn set_model(state: State<'_, AppState>, agent: String, model: Option<String>) -> Result<(), String> {
    agents::drop_spares();
    let chosen = model.as_deref().map(str::trim).filter(|m| !m.is_empty());
    if let Some(m) = chosen {
        let a = agents::by_id(&agent).ok_or_else(|| format!("unknown AI {agent}"))?;
        let offered = agents::list_models(a).await;
        if m.starts_with('-') || (!offered.is_empty() && !offered.iter().any(|c| c.id == m)) {
            return Err(format!("{m} isn't a model {} offers", a.name));
        }
    }
    state.engine.config.set_model(&agent, chosen).map(|_| ())
}

/// The models one AI's CLI offers, for its council-box picker (#29).
#[tauri::command]
async fn list_models(agent: String) -> Result<Vec<agents::ModelChoice>, String> {
    let a = agents::by_id(&agent).ok_or_else(|| format!("unknown AI {agent}"))?;
    Ok(agents::list_models(a).await)
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

/// The conversation's shared task board.
#[tauri::command]
fn tasks_get(state: State<AppState>, id: String) -> Result<tasks::Board, String> {
    if !store::valid_id(&id) {
        return Err("Unknown conversation.".into());
    }
    Ok(tasks::load(state.engine.store.dir(), &id))
}

/// A change the human made to the task board; Consilium notes it in the conversation.
#[tauri::command]
fn tasks_apply(state: State<AppState>, id: String, op: tasks::Op) -> Result<tasks::Board, String> {
    if !store::valid_id(&id) {
        return Err("Unknown conversation.".into());
    }
    if let Some(owner) = peer_sync::board_owner(&id) {
        return Err(format!("This board is kept on {owner}, so every computer sees the same tasks. Change it there, or ask an AI here with a TASK line."));
    }
    let dir = state.engine.store.dir();
    if let Some(note) = tasks::apply(dir, &id, "human", &[op])? {
        state.engine.note(&id, &note)?;
        peer_sync::share_board(&id);
    }
    Ok(tasks::load(dir, &id))
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

#[tauri::command]
async fn attach_clipboard_image(app: AppHandle, state: State<'_, AppState>, id: String) -> Result<attach::Attachment, String> {
    use tauri_plugin_clipboard_manager::ClipboardExt;

    let image = app.clipboard().read_image().map_err(|error| format!("Couldn't read a clipboard image: {error}"))?;
    if image.rgba().len() as u64 > attach::MAX_BYTES {
        return Err("The clipboard image is too large. Copy a smaller image and try again.".into());
    }
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, image.width(), image.height());
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|error| error.to_string())?;
        writer.write_image_data(image.rgba()).map_err(|error| error.to_string())?;
    }
    state.engine.attach_bytes(&id, "clipboard-1.png", &bytes)
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

/// Whether `name` is safe to pass to the system's app launcher: letters, digits, spaces and a few
/// punctuation marks only, so it can't carry flags, paths, quotes or shell syntax.
fn valid_app_name(name: &str) -> bool {
    let n = name.trim();
    !n.is_empty()
        && n.len() <= 60
        && n.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && n.chars().all(|c| c.is_ascii_alphanumeric() || " .+-".contains(c))
}

/// Opens an installed app by name. Called only after the human confirms a spoken "open <app>" in
/// Consilium's own dialog.
#[tauri::command]
fn open_app(name: String) -> Result<(), String> {
    let name = name.trim().to_string();
    if !valid_app_name(&name) {
        return Err("that isn't an app name Consilium can open".into());
    }
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = std::process::Command::new("open");
        c.args(["-a", &name]);
        c
    };
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = std::process::Command::new("powershell");
        c.args(["-NoProfile", "-Command", "Start-Process", "-FilePath", &format!("'{name}'")]);
        c
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut cmd = {
        let mut c = std::process::Command::new("gtk-launch");
        c.arg(name.to_lowercase().replace(' ', "-"));
        c
    };
    let out = cmd.output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("couldn't open {name}: {}", String::from_utf8_lossy(&out.stderr).trim()))
    }
}

/// Shown to the human and the AIs after an update to this version.
const WHATS_NEW: &str = "What's new: the Stop button in an AI's council box now also ends what that AI is doing right now in the chat (its CLI, or a local model's request), not only its next turns; the other AIs carry on.";

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
    let mut note = format!("Consilium was updated from {before} to {now} and restarted. {WHATS_NEW}");
    if let Some(ledger) = safety::read_latest_ledger(data) {
        if !ledger.snapshots.is_empty() {
            let total: usize = ledger.snapshots.iter().map(|s| s.tracked_files.len() + s.untracked_files.len()).sum();
            note.push_str(&format!(
                " Session safety: preserved {total} uncommitted file(s) across {} project(s) on branch consilium/session-safety.",
                ledger.snapshots.len()
            ));
        }
    }
    for id in ids {
        let _ = engine.note(&id, &note);
    }
}

/// A newer build staged by `scripts/stage-update.sh`, if any; the window offers Update Now for it.
#[tauri::command]
fn update_available() -> Option<update::Staged> {
    data_dir().and_then(|d| update::staged(&d))
}

/// Live state for the sidebar emblem while an installer downloads from GitHub.
#[tauri::command]
fn update_download_status() -> update::DownloadStatus {
    update::download_status()
}

/// Downloads and stages this platform's release update from GitHub.
#[tauri::command]
async fn stage_release_update(version: String, url: String, digest: Option<String>) -> Result<(), String> {
    let data = data_dir().ok_or("can't find Consilium's data folder")?;
    update::stage_from_url(&data, &version, &url, digest.as_deref().unwrap_or("")).await
}

#[tauri::command]
async fn request_draft_installers() -> Result<Option<String>, String> {
    let data = data_dir().ok_or("can't find Consilium's data folder")?;
    tauri::async_runtime::spawn_blocking(move || ci::request_draft_installers(&data))
        .await
        .map_err(|error| error.to_string())?
}

/// "deb" or "rpm" when this Linux copy came from a system package, so the update check
/// downloads the matching package instead of the AppImage.
#[tauri::command]
fn linux_package_kind() -> Option<&'static str> {
    update::linux_package_kind()
}

/// Update Now: tells every conversation with AIs at work that the app is updating, starts the
/// installer and quits. The cut-off turns resume after the relaunch, like after any restart.
#[tauri::command]
fn update_now(app: AppHandle, state: State<AppState>) -> Result<(), String> {
    install_staged(&app, &state.engine)
}

/// Installs the staged build and quits, so Consilium restarts into it (Update Now, or the
/// automatic install).
fn install_staged(app: &AppHandle, engine: &Arc<Engine>) -> Result<(), String> {
    let data = data_dir().ok_or("can't find Consilium's data folder")?;
    let staged = update::staged(&data).ok_or("no newer build is staged")?;

    // Save a snapshot of each project's uncommitted work before closing the app
    let snapshots = safety::snapshot_all(&data, engine, &staged.version);

    let mut note = format!(
        "Update Now: Consilium is installing {} and restarting in a few seconds. Hold here: work in \
         progress is paused and resumes automatically after the restart, so stop at this point and \
         continue from `git status` and `git diff` when you come back.",
        staged.version
    );
    if !snapshots.is_empty() {
        let total: usize = snapshots.iter().map(|s| s.tracked_files.len() + s.untracked_files.len()).sum();
        note.push_str(&format!(
            " A safety snapshot of {total} uncommitted file(s) across {} project(s) was saved to branch consilium/session-safety.",
            snapshots.len()
        ));
    }
    // Start the installer first: if it can't run, nobody is told to stop for an update that won't come.
    let build = update::staged_build(&data);
    update::start_install(&data)?;
    // The linked computers update too (queued durably, so it goes out after the restart at worst).
    if let Some(b) = &build {
        peer_sync::announce_update(b);
    }
    for id in engine.running_ids() {
        let _ = engine.note(&id, &note);
    }
    // Save the window's size and full-screen state now, so the relaunch opens the same way.
    {
        use tauri_plugin_window_state::{AppHandleExt, StateFlags};
        let _ = app.save_window_state(StateFlags::all());
    }
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(300));
        app.exit(0);
    });
    Ok(())
}

/// Automatic updates: when the setting is on (it is by default), a newer staged build (a GitHub
/// release, or the build a linked computer runs, prepared by `follow`) is installed without a
/// click and Consilium restarts into it; the AIs' work resumes after, as after Update Now. It waits
/// while a command is running, so nothing is cut off halfway, and a failed install is retried
/// after `AUTO_INSTALL_RETRY_MS`, once, with a note in the conversation.
fn start_auto_install(app: AppHandle, engine: Arc<Engine>) {
    const CHECK_EVERY: std::time::Duration = std::time::Duration::from_secs(30);
    const AUTO_INSTALL_RETRY_MS: u64 = 30 * 60_000;
    tauri::async_runtime::spawn(async move {
        let mut failed: Option<(String, u64)> = None;
        loop {
            tokio::time::sleep(CHECK_EVERY).await;
            let Some(data) = data_dir() else { continue };
            // A linked computer's Request Update counts even with automatic install off.
            let requested = follow::install_wanted(store::now_ms());
            if !(engine.config.get().auto_install_updates || requested) || runcmd::running() > 0 {
                continue;
            }
            let Some(staged) = update::staged(&data) else { continue };
            if requested {
                follow::clear_install_request();
            }
            if failed.as_ref().is_some_and(|(b, at)| *b == staged.build && store::now_ms() < at + AUTO_INSTALL_RETRY_MS) {
                continue;
            }
            let newest = engine.store.list().into_iter().find(|c| !peer_sync::is_mirror(&c.id)).map(|c| c.id);
            if let Some(id) = &newest {
                let _ = engine.note(
                    id,
                    &format!(
                        "Installing Consilium {} automatically (now {}) and restarting; the AIs' work resumes after. \
                         Turn this off in Settings → Updates.",
                        staged.build, staged.current_build
                    ),
                );
            }
            if let Err(e) = install_staged(&app, &engine) {
                failed = Some((staged.build.clone(), store::now_ms()));
                if let Some(id) = &newest {
                    let _ = engine.note(id, &format!("The automatic install of Consilium {} didn't start: {e}. Trying again in 30 minutes; Update Now works too.", staged.build));
                }
            }
        }
    });
}

/// Runs the communication stress test with every linked computer and posts each result in the
/// newest conversation as a status note. `when` says what started it.
async fn stress_test_and_post(engine: &Arc<Engine>, when: &str) -> Result<Vec<link::StressReport>, String> {
    let link = link_handle().ok_or("linking isn't running")?;
    let reports = link.stress_test().await?;
    if let Some(id) = engine.store.list().into_iter().find(|c| !peer_sync::is_mirror(&c.id)).map(|c| c.id) {
        for r in &reports {
            let _ = engine.note(&id, &r.summary(when));
        }
    }
    Ok(reports)
}

/// The Communication stress test button (Settings → Multi-Computer Sync).
#[tauri::command]
async fn stress_test_link(state: State<'_, AppState>) -> Result<Vec<link::StressReport>, String> {
    let engine = state.engine.clone();
    stress_test_and_post(&engine, "requested").await
}

/// After every start, once a linked computer is connected, runs the stress test and posts it.
fn stress_test_after_start(engine: Arc<Engine>) {
    tauri::async_runtime::spawn(async move {
        for _ in 0..120 {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            let Some(link) = link_handle() else { continue };
            if !link.enabled() {
                return;
            }
            if link.status().peers.iter().any(|p| p.connected) {
                // A moment for the AIs' resumed work and the catch-up to settle first.
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                let _ = stress_test_and_post(&engine, "after restart").await;
                return;
            }
        }
    });
}

/// Request Update: asks every linked computer to update too. Returns how many it went to.
#[tauri::command]
fn request_peer_update() -> Result<usize, String> {
    peer_sync::request_update()
}

/// Restarts Consilium on this computer, targeted linked computers, or all computers.
#[tauri::command]
fn restart_app(targets: Vec<String>) -> Result<(), String> {
    let my_name = link_handle().map(|l| l.status().name).unwrap_or_default();
    let restart_all = targets.iter().any(|t| t == "all");
    let restart_self = restart_all || targets.iter().any(|t| t == "this" || (!my_name.is_empty() && t.eq_ignore_ascii_case(&my_name)));

    for target in &targets {
        if target != "this" && (my_name.is_empty() || !target.eq_ignore_ascii_case(&my_name)) {
            let _ = peer_sync::request_restart(if restart_all { None } else { Some(target.as_str()) });
            if restart_all {
                break;
            }
        }
    }

    if restart_self {
        restart_this_computer();
    }
    Ok(())
}

/// Whether a linked computer announced an update since the last call, so the window checks GitHub now.
#[tauri::command]
fn release_check_requested() -> bool {
    follow::take_release_check()
}

/// Turns automatic install-and-restart on or off (Settings → Updates).
#[tauri::command]
fn set_auto_install_updates(state: State<AppState>, on: bool) -> Result<(), String> {
    state.engine.config.set_auto_install_updates(on).map(|_| ())
}

/// Status of the most recent session safety snapshot.
#[tauri::command]
fn session_safety_status() -> Option<safety::SafetyLedger> {
    let data = data_dir()?;
    safety::read_latest_ledger(&data)
}

/// Restores a project's session safety snapshot (merges tracked stash and restores untracked files).
#[tauri::command]
fn session_safety_restore(repo: Option<String>) -> Result<String, String> {
    let data = data_dir().ok_or("can't find data folder")?;
    let path = match repo {
        Some(p) => std::path::PathBuf::from(p),
        None => std::env::current_dir().map_err(|e| e.to_string())?,
    };
    safety::restore_repo(&path, &data)
}

/// A screen-only update staged by `scripts/stage-ui.mjs` for this engine; applies without a restart.
#[tauri::command]
fn ui_update_available() -> Option<liveui::StagedUi> {
    let data = data_dir()?;
    LIVE.get()?.staged(&data)
}

/// Makes the staged page live; the window then reloads itself and calls `ui_loaded`.
#[tauri::command]
fn ui_update_apply() -> Result<String, String> {
    let data = data_dir().ok_or("can't find Consilium's data folder")?;
    LIVE.get().ok_or("instant updates aren't available in this build")?.apply(&data)
}

/// The page confirms it started; an unconfirmed new page is replaced by the built-in one.
#[tauri::command]
fn ui_loaded() -> Option<String> {
    let live = LIVE.get()?;
    live.confirm();
    live.version()
}

static LIVE: std::sync::OnceLock<Arc<liveui::Live>> = std::sync::OnceLock::new();

/// Stands in for the built-in assets for a moment while they are wrapped.
struct NoAssets;
impl<R: tauri::Runtime> tauri::Assets<R> for NoAssets {
    fn get(&self, _: &tauri::utils::assets::AssetKey) -> Option<std::borrow::Cow<'_, [u8]>> {
        None
    }
    fn iter(&self) -> Box<tauri::utils::assets::AssetsIter<'_>> {
        Box::new(std::iter::empty())
    }
    fn csp_hashes(
        &self,
        _: &tauri::utils::assets::AssetKey,
    ) -> Box<dyn Iterator<Item = tauri::utils::assets::CspHash<'_>> + '_> {
        Box::new(std::iter::empty())
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
    if !agents::is_api(a.kind) {
        return Err(format!("{} signs in through its own app, not a key", a.name));
    }
    apiagent::set_key(a.id, &key)
}

/// Adds an AI of the person's own: any OpenAI-compatible API, or a model on this computer or the
/// home network. The key, if any, is stored like the other API keys and never sent back.
#[tauri::command]
fn add_custom_ai(name: String, base_url: String, model: String, local: bool, key: String) -> Result<String, String> {
    let id = customai::add(&name, &base_url, &model, local)?;
    if !key.trim().is_empty() {
        if let Err(e) = apiagent::set_key(&id, &key) {
            let _ = customai::remove(&id);
            return Err(e);
        }
    }
    Ok(id)
}

/// Deletes an AI the person added, with its key. It leaves the council first.
#[tauri::command]
fn remove_custom_ai(state: State<'_, AppState>, id: String) -> Result<(), String> {
    if customai::by_id(&id).is_none() {
        return Err("Only AIs you added yourself can be deleted.".into());
    }
    if state.engine.config.is_member(&id) {
        state.engine.config.set_member(&id, false)?;
    }
    let _ = apiagent::set_key(&id, "");
    customai::remove(&id)
}

/// Probes a local Ollama daemon for installed models via its `/api/tags` endpoint.
#[tauri::command]
async fn probe_ollama(base_url: Option<String>) -> Result<Vec<ollama::OllamaTagModel>, String> {
    ollama::list_models(base_url.as_deref()).await
}

/// Registers an Ollama model as a Custom AI council member.
#[tauri::command]
fn import_ollama_model(model: ollama::OllamaTagModel, base_url: Option<String>) -> Result<String, String> {
    let custom = ollama::to_custom_ai(&model, base_url.as_deref())?;
    customai::add(&custom.name, &custom.base_url, &custom.model, custom.local)
}

/// Returns the newest host resources reported by each linked peer computer.
#[tauri::command]
fn get_peer_resources() -> Result<std::collections::HashMap<String, peer_sync::PeerResourceReport>, String> {
    Ok(peer_sync::peer_resource_reports())
}

#[tauri::command]
fn request_peer_resources() -> Result<String, String> {
    let Some(link) = link_handle().filter(|link| link.enabled() && link.has_peers()) else {
        return Err("No paired computers are available.".into());
    };
    link.send_connected(peer_protocol::PacketKind::Chat, serde_json::json!({
        "conversation": "",
        "title": "",
        "id": uuid::Uuid::new_v4().to_string(),
        "sender": "system",
        "kind": "resources_request",
        "to": [],
        "text": "",
    }))
}

/// Broadcasts this computer's host resources to linked peers.
#[tauri::command]
fn broadcast_host_resources() -> Result<(), String> {
    let data_dir = crate::data_dir();
    peer_sync::share_resources(data_dir.as_deref())
}

/// Runs block `index` of AI message `event_id` (the human clicked Run on its command box), then
/// sends the output to that AI as the human's message. The command text is read from the stored
/// message, never taken from the screen, and each block runs at most once.
#[tauri::command]
async fn run_command(state: State<'_, AppState>, id: String, event_id: String, index: usize) -> Result<Event, String> {
    run_block(state.engine.clone(), &id, &event_id, index).await
}

/// "Always run" (Settings): once the human turned it on, command boxes in a reply from a
/// Full-capacity AI run without a Run click, one after another, each reporting back as Run does.
/// Boxes that need admin rights, install software or that Consilium refuses still wait for the
/// human (`runcmd::auto_runnable`).
fn auto_run_commands(engine: Arc<Engine>, e: &Event) {
    if e.kind != "message" || e.sender == "human" || e.sender == "consilium" || !e.text.contains("```") {
        return;
    }
    // Only replies arriving now: an old box shown again (a reload, another process's log) waits
    // for the human, and a conversation another computer owns runs its commands there.
    if store::now_ms().saturating_sub(e.ts) > 120_000 || peer_sync::is_mirror(&e.conversation) {
        return;
    }
    let blocks = runcmd::fenced_blocks(&e.text);
    // "Auto-run this chat" (the human accepted its warning for this conversation): every box from an
    // AI of this council runs, risky, admin and install ones included. Only what Consilium refuses
    // outright (a shell this computer can't run, another computer's command) still waits.
    if auto_run::is_on(engine.store.dir(), &e.conversation) && engine.config.is_member(&e.sender) {
        let all: Vec<usize> = blocks.iter().enumerate().filter(|(_, b)| b.closed && runcmd::check(b).is_ok()).map(|(i, _)| i).collect();
        run_in_turn(engine, e, all);
        return;
    }
    let config = engine.config.get();
    if !config.auto_run_commands || engine.config.access(&e.sender) != Access::Unrestricted {
        return;
    }
    let runnable: Vec<usize> = blocks.iter().enumerate().filter(|(_, b)| runcmd::auto_runnable(b)).map(|(i, _)| i).collect();
    // A box that could do lasting damage waits for a click even with Always run on, so a Deny can
    // never come too late; a note says which one and why.
    for (i, b) in blocks.iter().enumerate() {
        if let Some(why) = runcmd::risky(&b.code).filter(|_| b.closed && runcmd::check(b).is_ok()) {
            let _ = engine.note(
                &e.conversation,
                &format!(
                    "Always run held back command box {} from {}: {why}. It runs only if you click Run.",
                    i + 1,
                    prompt::label(&e.sender)
                ),
            );
        }
    }
    run_in_turn(engine, e, runnable);
}

/// Runs the chosen command boxes of `e` one after another, each reporting back as Run does.
fn run_in_turn(engine: Arc<Engine>, e: &Event, runnable: Vec<usize>) {
    if runnable.is_empty() {
        return;
    }
    let (conversation, event_id) = (e.conversation.clone(), e.id.clone());
    tauri::async_runtime::spawn(async move {
        for index in runnable {
            if let Err(err) = run_block(engine.clone(), &conversation, &event_id, index).await {
                eprintln!("consilium: always-run skipped {event_id}#{index}: {err}");
            }
        }
    });
}

/// When Auto-run is turned on in a chat, runs the AI boxes posted since the human last wrote that
/// are still waiting (under `autocontinue::CATCH_UP_MS` old): auto-run only sees a box as it
/// arrives, so ones posted while it was off would otherwise wait for a click forever. A note names
/// them first and they start after `autocontinue::DELAY_SECS`, so the human can Deny any of them.
fn catch_up_auto_run(engine: Arc<Engine>, conversation: String) {
    if crate::peer_sync::is_mirror(&conversation) {
        return;
    }
    let Ok(events) = engine.store.load(&conversation) else { return };
    let from = crate::autocontinue::since_human_start(&events);
    let picks: Vec<(String, usize)> =
        crate::autocontinue::waiting_boxes(&events, from, crate::store::now_ms(), crate::autocontinue::CATCH_UP_MS)
            .into_iter()
            .filter(|(id, _)| events.iter().any(|e| &e.id == id && engine.config.is_member(&e.sender)))
            .collect();
    if picks.is_empty() {
        return;
    }
    let list = picks.iter().map(|(id, i)| format!("ref {}#{i}", &id[..id.len().min(8)])).collect::<Vec<_>>().join(", ");
    let _ = engine.note(
        &conversation,
        &format!(
            "Auto-run will run {} command box(es) that were waiting, in {} s: {list}. Click Deny on any you don't want run.",
            picks.len(),
            crate::autocontinue::DELAY_SECS
        ),
    );
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(crate::autocontinue::DELAY_SECS)).await;
        for (event_id, index) in picks {
            if !crate::auto_run::is_on(engine.store.dir(), &conversation) {
                return;
            }
            if let Err(err) = run_block(engine.clone(), &conversation, &event_id, index).await {
                eprintln!("consilium: auto-run catch-up skipped {event_id}#{index}: {err}");
            }
        }
    });
}

async fn run_block(engine: Arc<Engine>, id: &str, event_id: &str, index: usize) -> Result<Event, String> {
    let (id, event_id) = (id.to_string(), event_id.to_string());
    let events = engine.store.load(&id)?;
    let (event, block, guard) = runcmd::take(&events, &event_id, index)?;
    let cwd = engine
        .store
        .meta(&id)
        .ok()
        .and_then(|m| m.workspace)
        .or_else(|| engine.config.get().default_workspace)
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_dir())
        .or_else(dirs::home_dir)
        .ok_or("no folder to run the command in")?;
    let scratch = engine.data.join("run");
    runcmd::check(&block)?;
    // Durable receipt before anything starts: a crash mid-run can't lead to a second run.
    runcmd::mark_started(&engine.data.join("run-started"), &id, &event_id, index)?;
    let outcome = runcmd::run(&block, &cwd, &scratch).await?;
    drop(guard);
    let text = runcmd::report(&prompt::label(&event.sender), &event_id, index, &outcome);
    engine.send_with(&id, &text, Vec::new(), vec![event.sender.clone()], 1)
}

/// Clones a public GitHub repo into a private folder (running none of its code) and inspects it.
#[tauri::command]
async fn ingest_repo(url: String) -> Result<repo::RepoReport, String> {
    tauri::async_runtime::spawn_blocking(move || repo::ingest(&url))
        .await
        .map_err(|e| e.to_string())?
}

/// Versions of the projects being worked on in this folder: this PC's against GitHub's.
#[tauri::command]
async fn project_versions(workspace: Option<String>) -> Vec<projects::ProjectVersion> {
    let Some(folder) = workspace.filter(|w| !w.is_empty()) else { return Vec::new() };
    tauri::async_runtime::spawn_blocking(move || projects::active(std::path::Path::new(&folder)))
        .await
        .unwrap_or_default()
}

/// GitHub checks for the active projects in this folder: passing, failing or still running.
#[tauri::command]
async fn ci_status(workspace: Option<String>) -> Vec<ci::CiStatus> {
    let Some(folder) = workspace.filter(|w| !w.is_empty()) else { return Vec::new() };
    tauri::async_runtime::spawn_blocking(move || ci::for_workspace(std::path::Path::new(&folder)))
        .await
        .unwrap_or_default()
}

/// Checks each third-party messaging connection (Signal for now) and reports whether it answers.
#[tauri::command]
async fn messaging_status() -> Vec<bridge::ConnectorStatus> {
    tauri::async_runtime::spawn_blocking(|| {
        let cfg = bridge::BridgeConfig::load(&bridge::config_path(None));
        vec![match cfg {
            Ok(c) => bridge::probe_signal(&c, std::time::Duration::from_secs(2)),
            Err(_) => bridge::ConnectorStatus {
                id: "signal",
                name: "Signal",
                state: "down",
                detail: "messaging.json can't be read; fix or remove it".into(),
            },
        }]
    })
    .await
    .unwrap_or_default()
}

/// Messaging settings for the setup panel, with the phone number masked.
#[tauri::command]
fn messaging_config() -> Result<bridge::SetupView, String> {
    let path = bridge::config_path(None);
    Ok(bridge::view(&bridge::BridgeConfig::load(&path)?, &path))
}

/// Validates and saves messaging settings (file mode 0600, never in git).
#[tauri::command]
fn messaging_save(change: bridge::SetupChange) -> Result<bridge::SetupView, String> {
    let path = bridge::config_path(None);
    let current = bridge::BridgeConfig::load(&path).unwrap_or_default();
    let next = bridge::apply_change(&current, change)?;
    next.save(&path)?;
    Ok(bridge::view(&next, &path))
}

/// Opens a terminal for one fixed messaging setup step: "install", "link" or "start".
#[tauri::command]
fn messaging_setup(action: String) -> Result<String, String> {
    let cfg = bridge::BridgeConfig::load(&bridge::config_path(None))?;
    if action == "install" {
        if let Some(note) = bridge::start_install()? {
            return Ok(note.to_string());
        }
    }
    let (title, command) = bridge::setup_command(&action, &cfg)?;
    setup::open_in_terminal(&setup::Step { title, command })?;
    Ok(title.to_string())
}

/// Starts the in-app Signal setup: QR code, background daemon, then a Note to Self round trip.
#[tauri::command]
fn signal_setup_begin() -> Result<signal_setup::SetupState, String> {
    signal_setup::begin(bridge::config_path(None))
}

/// Where the in-app Signal setup has got to.
#[tauri::command]
fn signal_setup_state() -> signal_setup::SetupState {
    signal_setup::state()
}

/// Stops a Signal link in progress.
#[tauri::command]
fn signal_setup_cancel() {
    signal_setup::cancel()
}

/// Samples host PC CPU and memory pressure (0-100%, green to red).
#[tauri::command]
fn system_pressure() -> pressure::SystemPressure {
    pressure::sample()
}

/// Read-only spyware scan scheduling. The external scanner is installed only after a separate,
/// exact human approval; these commands merely configure and observe the adapter.
#[tauri::command]
fn security_scan_status(state: State<AppState>) -> security_scan::ScanStatus {
    state.security.status()
}

#[tauri::command]
fn security_scan_save(
    state: State<AppState>,
    settings: security_scan::ScanSettings,
) -> Result<security_scan::ScanStatus, String> {
    state.security.save_settings(settings)
}

#[tauri::command]
fn security_scan_run(state: State<AppState>) -> Result<security_scan::ScanStatus, String> {
    state.security.request_run()
}

/// Fetches today's Bing daily wallpaper and caches it locally, returning a base64 data URL.
#[tauri::command]
async fn get_daily_wallpaper() -> Result<Option<wallpaper::DailyWallpaper>, String> {
    let client = reqwest::Client::builder()
        .user_agent("ConsiliumDesktopApp/1.0")
        .build()
        .map_err(|e| format!("failed to create HTTP client: {e}"))?;

    let dir = wallpaper::wallpaper_dir();
    match wallpaper::fetch_and_cache_daily_wallpaper(&client, &dir).await {
        Ok(wp) => Ok(Some(wp)),
        Err(e) => {
            if let Some(fallback) = wallpaper::get_fallback_wallpaper(&dir) {
                Ok(Some(fallback))
            } else {
                eprintln!("wallpaper fetch error: {e}");
                Ok(None)
            }
        }
    }
}

static RESTART_HOOK: std::sync::OnceLock<Box<dyn Fn() + Send + Sync>> = std::sync::OnceLock::new();

/// Restarts Consilium on this computer, saving window state and relaunching.
pub fn restart_this_computer() {
    if let Some(f) = RESTART_HOOK.get() {
        f();
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
static LINK: std::sync::OnceLock<link::Link> = std::sync::OnceLock::new();

/// Whether this computer should answer a phone message (see `link::answers_signal`). True if
/// the link never started.
pub fn link_answers_signal() -> bool {
    LINK.get().is_none_or(|l| l.answers_signal())
}

pub(crate) fn link_handle() -> Option<&'static link::Link> {
    LINK.get()
}

fn the_link() -> Result<&'static link::Link, String> {
    LINK.get().ok_or_else(|| "the computer link couldn't start; see the log".to_string())
}

// Link commands are async so they run on the runtime that the link's tasks need.
#[tauri::command]
async fn link_status() -> Result<link::LinkStatus, String> {
    Ok(the_link()?.status())
}

#[tauri::command]
async fn link_discover_lan() -> Result<Vec<String>, String> {
    link::discover_lan(link::PORT).await
}

#[tauri::command]
async fn link_set_enabled(on: bool) -> Result<link::LinkStatus, String> {
    let l = the_link()?;
    l.set_enabled(on)?;
    Ok(l.status())
}

#[tauri::command]
async fn link_pair_host() -> Result<link::PairingCode, String> {
    the_link()?.begin_pairing()
}

#[tauri::command]
async fn link_pair_cancel() -> Result<(), String> {
    the_link()?.cancel_pairing();
    Ok(())
}

#[tauri::command]
async fn link_pair_join(address: String, code: String) -> Result<link::PeerView, String> {
    the_link()?.join(&address, &code).await
}

#[tauri::command]
async fn link_unpair(node_id: String) -> Result<link::LinkStatus, String> {
    let l = the_link()?;
    l.unpair(&node_id)?;
    Ok(l.status())
}

#[tauri::command]
async fn link_test_peer(node_id: String) -> Result<link::PeerTestReport, String> {
    the_link()?.test_peer(&node_id).await
}

#[tauri::command]
async fn link_test_ai_handshake(node_id: String) -> Result<Vec<link::AiHandshake>, String> {
    the_link()?.test_ai_handshake(&node_id).await
}

#[tauri::command]
async fn link_add_peer_route(node_id: String, address: String) -> Result<Vec<String>, String> {
    the_link()?.add_peer_route(&node_id, &address)
}

#[tauri::command]
async fn link_refresh_routes(node_id: String) -> Result<Vec<String>, String> {
    the_link()?.refresh_routes(&node_id)
}

/// Everything received from paired computers, oldest first, saved before it was acknowledged.
#[tauri::command]
async fn link_inbox() -> Result<Vec<peer_protocol::PeerPacket>, String> {
    Ok(the_link()?.inbox())
}

/// Sends a chat payload to every paired computer; queued until each one acknowledges it.
#[tauri::command]
async fn link_send(payload: serde_json::Value) -> Result<String, String> {
    the_link()?.send(peer_protocol::PacketKind::Chat, payload)
}

struct QuickSummon(bool);

#[tauri::command]
fn quick_summon_available(shortcut: State<QuickSummon>) -> bool {
    shortcut.inner().0
}

fn register_quick_summon(app: &AppHandle) -> bool {
    use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

    let primary = if cfg!(target_os = "macos") { Modifiers::SUPER } else { Modifiers::CONTROL };
    let shortcut = Shortcut::new(Some(primary | Modifiers::ALT), Code::Space);
    let plugin = tauri_plugin_global_shortcut::Builder::new()
        .with_handler(move |app, pressed, event| {
            if pressed != &shortcut || event.state() != ShortcutState::Pressed {
                return;
            }
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.unminimize();
                let _ = window.set_focus();
                let _ = window.emit("consilium://quick-summon", ());
            }
        })
        .build();
    if app.plugin(plugin).is_err() {
        return false;
    }
    app.global_shortcut().register(shortcut).is_ok()
}

pub fn run() {
    let mut context = tauri::generate_context!();
    if let Some(data) = data_dir() {
        let live = liveui::Live::open(&data);
        let built_in = context.set_assets(Box::new(NoAssets));
        context.set_assets(Box::new(liveui::LiveAssets { built_in, live: live.clone() }));
        let _ = LIVE.set(live);
    }
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        // Remembers the window's size, position, maximized and full-screen state across restarts.
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .setup(|app| {
            app.manage(QuickSummon(register_quick_summon(app.handle())));
            let handle = app.handle().clone();
            let _ = RESTART_HOOK.set(Box::new(move || {
                use tauri_plugin_window_state::{AppHandleExt, StateFlags};
                let _ = handle.save_window_state(StateFlags::all());
                let h = handle.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(300));
                    h.restart();
                });
            }));
            signal_setup::autostart(bridge::config_path(None));
            let data = match data_override() {
                Some(d) => d,
                None => app.path().app_data_dir()?,
            };
            let ui = Arc::new(Window(app.handle().clone()));
            let spawn = Box::new(|task: engine::Task| {
                tauri::async_runtime::spawn(task);
            });
            let engine = Arc::new(Engine::open(&data, ui, spawn)?);
            let security = security_scan::Scanner::open(&data);
            announce_update(&engine, &data);
            // End AI turns orphaned by a force-quit or freeze, then resume them. Off the setup
            // thread: checking processes (and the 1.5 s grace period when one is found) used to
            // hold the window back on every start.
            {
                let (engine, data) = (engine.clone(), data.clone());
                std::thread::spawn(move || {
                    reaper::reap_orphans(&data);
                    engine.resume_interrupted(None);
                });
            }
            // Keeps the window in step with the terminal command.
            tauri::async_runtime::spawn(engine.clone().watch(true));
            tauri::async_runtime::spawn(security.clone().watch(engine.clone()));
            // Reads each active project's GitHub checks and posts a note when they finish.
            tauri::async_runtime::spawn(ci::watch(engine.clone()));
            // Carries the newest conversation to and from Signal once setup is verified.
            signal_chat::run(engine.clone(), bridge::config_path(None));
            // Checks every connection once connections have had time to come up, then reports
            // the result in the newest conversation and to the phone.
            tauri::async_runtime::spawn(startup_check::run(engine.clone(), data.clone()));
            // Boxes posted while Consilium was closed: once connections have had time to come up,
            // catch up each Auto-run chat the same way turning Auto-run on does (note, delay, Deny).
            {
                let engine = engine.clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs(15)).await;
                    for c in engine.store.list() {
                        if auto_run::is_on(engine.store.dir(), &c.id) {
                            catch_up_auto_run(engine.clone(), c.id);
                        }
                    }
                });
            }
            app.manage(AppState { engine, security });
            // Direct link to the user's other paired computers. Off until they pair or turn it on.
            let emitter = app.handle().clone();
            let handler: link::Handler = Arc::new(move |p: &peer_protocol::PeerPacket| {
                // Chat from another computer goes into its mirror conversation, then the window.
                if let Some(e) = peer_sync::receive(p) {
                    let _ = emitter.emit(EVENT, &e);
                }
                emitter.emit("link-packet", p).map_err(|e| e.to_string())
            });
            match link::Link::open(&data.join("link"), link::PORT, handler) {
                Ok(l) => {
                    if l.enabled() {
                        let started = l.clone();
                        tauri::async_runtime::spawn(async move { started.start() });
                    }
                    let _ = LINK.set(l);
                }
                Err(e) => eprintln!("link: {e}"),
            }
            peer_sync::init(app.state::<AppState>().engine.clone(), &data.join("link"));
            // Keeps this computer on the newest build any linked computer runs.
            follow::start(app.state::<AppState>().engine.clone(), data.clone());
            // Installs newer builds by itself and restarts, unless turned off in Settings.
            start_auto_install(app.handle().clone(), app.state::<AppState>().engine.clone());
            // Restarts the app if it freezes (a lock that stays taken, a stalled runtime).
            stall::start(app.handle().clone(), app.state::<AppState>().engine.clone(), data.clone());
            // Keeps the council's local models (Ollama) loaded, so they answer without a cold start.
            ollama::keep_warm(app.state::<AppState>().engine.config.clone());
            // Starts Gemini's Antigravity for the most recent chat's folder, ready for its first answer.
            {
                let engine = app.state::<AppState>().engine.clone();
                tauri::async_runtime::spawn(async move {
                    let Some(gemini) = agents::by_id("gemini").filter(|_| engine.config.is_member("gemini")) else { return };
                    let folder = engine
                        .store
                        .list()
                        .into_iter()
                        .max_by_key(|m| m.updated)
                        .and_then(|m| m.workspace)
                        .or_else(|| engine.config.get().default_workspace)
                        .map(std::path::PathBuf::from)
                        .filter(|p| p.is_dir());
                    let Some(folder) = folder else { return };
                    let access = agents::effective_access("gemini", engine.config.access("gemini"), true);
                    agents::prewarm_antigravity(gemini, access, &folder, engine.config.model("gemini").as_deref()).await;
                });
            }
            // Tests every route to the linked computers after each start and posts the result.
            stress_test_after_start(app.state::<AppState>().engine.clone());
            // Falls back to the built-in page if an instantly applied one never starts.
            let handle = app.handle().clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(std::time::Duration::from_secs(2));
                let (Some(live), Some(data)) = (LIVE.get(), data_dir()) else { return };
                if live.check(&data) {
                    if let Some(w) = handle.get_webview_window("main") {
                        let _ = w.eval("location.reload()");
                    }
                }
            });
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
            tasks_get,
            tasks_apply,
            agents_status,
            agents_usage,
            set_member,
            set_access,
            set_model,
            list_models,
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
            add_custom_ai,
            remove_custom_ai,
            probe_ollama,
            import_ollama_model,
            system_pressure,
            security_scan_status,
            security_scan_save,
            security_scan_run,
            messaging_status,
            messaging_config,
            messaging_save,
            messaging_setup,
            signal_setup_begin,
            signal_setup_state,
            signal_setup_cancel,
            project_versions,
            ci_status,
            ingest_repo,
            run_command,
            update_available,
            update_download_status,
            update_now,
            stage_release_update,
            request_draft_installers,
            linux_package_kind,
            ui_update_available,
            ui_update_apply,
            ui_loaded,
            self_check,
            unbench,
            quick_summon_available,
            cli_status,
            install_cli,
            reset_forecast,
            claude_reset_history,
            set_reset_forecast,
            set_auto_continue,
            set_ai_on,
            set_chat_auto_run,
            chat_auto_run,
            switched_off,
            set_isolate_agents,
            set_auto_run_commands,
            set_auto_install_updates,
            stress_test_link,
            release_check_requested,
            request_peer_update,
            restart_app,
            agent_branches,
            merge_agent_branch,
            discard_agent_branch,
            save_approval,
            tool_catalog,
            local_tool_inventory,
            runbooks::list_runbooks,
            runbooks::read_runbook,
            forget_approval,
            attach_files,
            attach_data,
            attach_clipboard_image,
            open_attachment,
            open_app,
            get_daily_wallpaper,
            link_status,
            link_discover_lan,
            link_set_enabled,
            link_pair_host,
            link_pair_cancel,
            link_pair_join,
            link_unpair,
            link_test_peer,
            link_test_ai_handshake,
            link_add_peer_route,
            link_refresh_routes,
            link_inbox,
            link_send,
            session_safety_status,
            session_safety_restore,
            get_peer_resources,
            request_peer_resources,
            broadcast_host_resources
        ])
        .build(context)
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

