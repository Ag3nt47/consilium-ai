// Typed wrappers around the Rust commands, plus the two live event streams.
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { demoApi } from "./demo";

/** True inside the Consilium app; false in an ordinary browser, where the demo data is shown. */
export interface CatalogTool {
  name: string;
  source: string;
  about: string;
  installed: string | null;
  detectable: boolean;
  install_command: string | null;
}

export interface OllamaModel {
  name: string;
  model: string;
  size: number;
  digest: string;
  details?: {
    format: string;
    family: string;
    parameter_size: string;
    quantization_level: string;
  };
}

export const IN_APP = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

/** One item on the conversation's shared task checklist. */
export interface TaskItem {
  id: number;
  title: string;
  owner?: string | null;
  status: "todo" | "doing" | "blocked" | "done";
  note?: string | null;
  by: string;
  updated: number;
}

export interface TaskBoard {
  tasks: TaskItem[];
  next: number;
}

export type TaskOp =
  | { op: "add"; title: string; owner?: string }
  | { op: "take"; task: string }
  | { op: "done"; task: string }
  | { op: "block"; task: string; note?: string }
  | { op: "hand"; task: string; to: string }
  | { op: "reopen"; task: string }
  | { op: "drop"; task: string };

export type Sender = "human" | "gpt" | "claude" | string;

export interface ChatEvent {
  id: string;
  seq: number;
  ts: number;
  conversation: string;
  sender: Sender;
  kind: "message" | "system" | "error" | "seen";
  to: string[];
  text: string;
  requests: string[];
  /** AIs this message proposes adding to the council (from ADD_AI: lines). */
  add_ai: string[];
  /** Provider-reported counts for the AI turn that produced this event. */
  usage?: TokenUsage | null;
  /** Files the human attached. */
  attachments?: Attachment[];
  /** Message ids handed to this event's sender. Present only on persisted read receipts. */
  seen?: string[];
  /** Transport or bridge the message arrived through (e.g. "Signal"), if not sent directly in-app. */
  via?: string | null;
}

/** A file attached to a message; Consilium keeps its own copy in the conversation's folder. */
export interface Attachment {
  name: string;
  path: string;
  /** "image", "pdf", "text" (including text read out of Word and other documents), or "file". */
  kind: string;
  size: number;
  text_path?: string | null;
}

export interface Conversation {
  id: string;
  title: string;
  created: number;
  updated: number;
  /** Project folder the AIs may work in, subject to each AI's access level. */
  workspace: string | null;
}

/** How much an AI may do in a conversation's project folder. */
export type Access = "chat" | "read" | "edit" | "own" | "unrestricted";

export interface Council {
  members: string[];
  access: Record<string, Access>;
  default_workspace: string | null;
  default_access: Access;
  /** Show codex-reset.com's unofficial forecast of the next global Codex reset. */
  reset_forecast?: boolean;
  /** Exact admin or install commands the human chose to always allow on this computer. */
  saved_grants?: string[];
  /** Start another round automatically when a turn ends with tasks still open (on unless turned off). */
  auto_continue?: boolean;
  /** Full-capacity AIs work in their own git worktree (branch `consilium/<id>`). */
  isolate_agents?: boolean;
  /** Command boxes from a Full-capacity AI run on their own on this computer (admin and installs still wait). */
  auto_run_commands?: boolean;
  /** A newer staged build installs by itself and Consilium restarts into it (on unless turned off). */
  auto_install_updates?: boolean;
}

export const ACCESS_LEVELS: { id: Access; name: string; note: string }[] = [
  { id: "chat", name: "Chat only", note: "Sees only the conversation. No files, no tools." },
  { id: "read", name: "Read project", note: "Can read files in the conversation's project folder." },
  { id: "edit", name: "Edit project", note: "Can read and edit files in the project folder. No shell commands." },
  { id: "own", name: "Full (its own settings)", note: "Uses the AI app's approved tools, including the host's native shell. Administrator/root commands still need a fresh, exact REQUEST and normal OS authentication." },
  { id: "unrestricted", name: "Unrestricted", note: "Runs native PowerShell/Command Prompt on Windows or shells on macOS/Linux, installs software and changes files. Administrator/root elevation still needs a one-command REQUEST." },
];

export interface AgentInfo {
  id: string;
  name: string;
  company: string;
  member: boolean;
  access: Access;
  transport: string;
  /** An AI the person added themselves (it can be deleted). */
  custom?: boolean;
  tool: string;
  available: boolean;
  signed_in: boolean | null;
  version: string | null;
  path: string | null;
  /** Filled in separately from `api.usage()`, since usage probes are slow. */
  usage?: UsageStatus | null;
  hint: string;
  /** When it ran out of usage: the time (ms since the epoch) its limit resets. */
  benched_until?: number | null;
  /** Whether that time came from the AI's own tool, not Consilium's retry guess. */
  reset_known?: boolean;
  /** The model chosen in its council box (#29); null means the CLI's own default. */
  model?: string | null;
}

/** One model in an AI's council-box picker (#29). */
export interface ModelChoice {
  id: string;
  name: string;
}

export interface UsageWindow {
  label: string;
  remaining_percent: number;
  /** When this limit resets (ms since the epoch), if the provider says. */
  resets_at?: number | null;
}

/** The limits of the model in use, or of the AI's whole plan. */
export interface UsageMeterInfo {
  /** The model's name, or the AI's. */
  label: string;
  remaining_percent: number;
  windows: UsageWindow[];
  /** The model has no limit of its own and draws on the overall one. */
  shared: boolean;
}

export interface UsageStatus {
  /** Lowest remaining applicable limit; this is the window that will stop the AI first. */
  remaining_percent: number;
  windows: UsageWindow[];
  /** The model in use (changes when the AI is switched to another model). */
  model?: UsageMeterInfo | null;
  /** The AI's whole plan, across its models. */
  overall?: UsageMeterInfo | null;
}

/** codex-reset.com's unofficial forecast of the next global Codex usage reset. */
export interface ResetForecast {
  chance_24h: number | null;
  chance_48h: number | null;
  confidence: string | null;
  last_reset_at: number | null;
  official: string | null;
  fetched_at: number;
}

export const FORECAST_SITE = "https://codex-reset.com/";

/** claude-resets.com's history of announced Claude Code usage resets (history, not a forecast; CC BY 4.0). */
export interface ClaudeResetHistory {
  last_reset_at: number | null;
  last_reset_scope: string | null;
  last_reset_url: string | null;
  reset_count: number | null;
  mean_gap_days: number | null;
  banked: { date: number | null; scope: string | null; usable_until: number | null }[];
  fetched_at: number;
}

export const CLAUDE_RESETS_SITE = "https://claude-resets.com/";

/** The `consilium` terminal command. */
export interface CliInstall {
  installed: boolean;
  path: string;
  command: string;
  note: string;
}

/** Progress of the in-app Signal setup. The full phone number is never included. */
export interface SignalSetupState {
  phase: "idle" | "linking" | "starting" | "verifying" | "verified" | "failed";
  link_uri: string | null;
  code: string | null;
  detail: string;
  device_name: string;
}

export interface MessagingSetup {
  enabled: boolean;
  endpoint: string;
  /** Masked, e.g. "•••• 4821"; empty when no number is saved. */
  account_hint: string;
  allowed_acis: string[];
  auto_forward_decisions: boolean;
  signal_cli: string | null;
  can_install: boolean;
  /** The password-free background install is running. */
  install_running: boolean;
  /** Why the last background install stopped, if it failed. */
  install_error: string | null;
  config_path: string;
}

export interface MessagingChange {
  enabled: boolean;
  /** null keeps the saved number. */
  account: string | null;
  endpoint: string;
  allowed_acis: string[];
  auto_forward_decisions: boolean;
}

export interface ConnectorStatus {
  id: string;
  name: string;
  /** "off" = not set up, "ok" = answered the last check, "down" = set up but not answering. */
  state: "off" | "ok" | "down";
  detail: string;
}

export interface SystemPressure {
  percent: number;
  cpu_percent: number;
  memory_percent: number;
  cores: number;
  total_memory_mb: number;
  available_memory_mb?: number;
  label: string;
  hardware_profile: "conservative" | "balanced" | "performance";
  adaptive_mode: "eco" | "balanced" | "full";
  recommended_parallel_agents: number;
  /** Per-AI share of the PC (0-100), keyed by agent id; only AIs with a running process appear. */
  agents?: Record<string, number>;
}

export interface SecurityScanStatus {
  installed: boolean;
  verified: boolean;
  tool_path: string;
  enabled: boolean;
  interval_hours: number;
  auto_remove_malicious_extensions: boolean;
  running: boolean;
  last_started_ms: number;
  last_finished_ms: number;
  last_exit_code: number | null;
  last_summary: string;
  last_report: string | null;
  paused_reason: string;
}

export interface TokenUsage {
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number;
  cache_creation_tokens: number;
  thinking_tokens: number;
  total_tokens: number;
}

export interface TokenUsageReport {
  conversation: Record<string, TokenUsage>;
  today: Record<string, TokenUsage>;
}

export interface CheckResult {
  id: string;
  name: string;
  ok: boolean;
  ms: number;
  detail: string;
}

export interface TurnState {
  conversation: string;
  state: "running" | "idle";
  agent: string | null;
  /** Every AI working on an answer right now; they answer at the same time. */
  agents?: string[];
  round: number;
  rounds: number;
}

/** Live provider telemetry. Observable phases/tool actions only; never hidden reasoning text. */
export interface AgentActivity {
  conversation: string;
  agent: string;
  kind: "preparing" | "planning" | "thinking" | "reading" | "researching" | "coding" | "running" | "tool" | "delegating" | "reporting" | "working" | string;
  label: string;
  detail: string;
  started_at: number;
  updated_at: number;
}

/** One paired computer, as the direct link sees it (src-tauri/src/link.rs). */
export type LinkPeer = {
  node_id: string;
  name: string;
  connected: boolean;
  /** The address in use, e.g. "192.168.1.20:8470"; the fastest answering route wins. */
  route: string | null;
  local_interface?: string | null;
  rtt_ms: number | null;
  last_seen_ms: number | null;
  /** Packets sent but not yet acknowledged; re-sent when the link comes back. */
  queued: number;
  /** What the AIs on that computer are answering right now, e.g. "Claude in Plans". */
  activity: string[];
  /** Its project folder's git state, e.g. "consilium-ai · main · 2 changed". */
  git: string | null;
  /** The Consilium build it runs; absent from computers on older versions. */
  build?: Build | null;
  /** Every AI on both computers and whether it has heard, and been heard by, the other side. */
  handshake?: AiHandshake[];
  /** Known candidate routes (wired Ethernet, Wi-Fi, direct cable) for failover. */
  addrs: string[];
};

export type RouteTest = {
  addr: string;
  kind: string;
  reachable: boolean;
  rtt_ms?: number;
  local_ip?: string | null;
  local_interface?: string | null;
  status: string;
};

export type PeerTestReport = {
  node_id: string;
  name: string;
  connected: boolean;
  active_route: string | null;
  handshake_ok: boolean;
  rtt_ms: number | null;
  routes: RouteTest[];
  queued_outgoing: number;
  summary: string;
};

export type LinkStatus = {
  enabled: boolean;
  node_id: string;
  name: string;
  port: number;
  /** This computer's addresses, to type on the other computer if needed. */
  addrs: string[];
  pairing_code_expires_ms: number | null;
  peers: LinkPeer[];
  error: string | null;
  /** The build this computer runs, and what it is doing to match the newest linked computer. */
  build?: Build;
  follow?: FollowStatus;
};

/** The communication stress test with one linked computer (src-tauri/src/link.rs). */
export type StressReport = {
  computer: string;
  routes: { addr: string; kind: string; ok: boolean; ms: number | null; local_interface?: string | null; local_addr?: string | null; detail: string }[];
  restored: StressReport["routes"][number] | null;
};

/** One AI's side of the handshake with a linked computer (src-tauri/src/link.rs). */
export type AiHandshake = {
  ai: string;
  side: "here" | "there";
  /** How the AIs on the other computer address it, e.g. "@claude-lukes-mbp". */
  handle: string;
  /** It has read a message from an AI on the other computer and answered. */
  reads: boolean;
  /** An AI on the other computer has read one of its messages and answered. */
  read: boolean;
};

/** A Consilium build (src-tauri/src/build_id.rs): two builds of one version differ in `number`. */
export type Build = {
  version: string;
  /** Commits in its history, so it only grows; 0 when unknown. */
  number: number;
  /** Short commit id, with "+dirty" when built with uncommitted changes. */
  commit: string;
};

/** Keeping this computer on the newest build a linked computer runs (src-tauri/src/follow.rs). */
export type FollowStatus = {
  state: "idle" | "waiting" | "preparing" | "staged" | "failed";
  target: string | null;
  from: string | null;
  detail: string | null;
};

/** e.g. "0.3.37 #812 (76debf3a1b2c)", or the version alone when the rest is unknown. */
export function buildLabel(b: Build): string {
  if (!b.number) return b.version;
  return `${b.version} #${b.number}${b.commit ? ` (${b.commit})` : ""}`;
}

/** How `b` stands against `mine`: same version compares by build number (see build_id.rs). */
export function buildCompare(b: Build, mine: Build): "newer" | "older" | "same" {
  const parse = (v: string) => v.split(/[.+-]/).map(Number).filter((n) => Number.isInteger(n));
  const [x, y] = [parse(b.version), parse(mine.version)];
  for (let i = 0; i < Math.max(x.length, y.length); i++) {
    if ((x[i] ?? -1) !== (y[i] ?? -1)) return (x[i] ?? -1) > (y[i] ?? -1) ? "newer" : "older";
  }
  if (b.number === mine.number) return "same";
  return b.number > mine.number ? "newer" : "older";
}

const appApi = {
  agents: () => invoke<AgentInfo[]>("agents_status"),
  /** Remaining plan usage per council member id; members that don't report it are absent. */
  usage: () => invoke<Record<string, UsageStatus>>("agents_usage"),
  list: () => invoke<Conversation[]>("list_conversations"),
  create: () => invoke<Conversation>("new_conversation"),
  load: (id: string) => invoke<ChatEvent[]>("load_conversation", { id }),
  /** Token totals for one conversation and across all conversations since local midnight. */
  tokenUsage: (id: string, dayStart: number) => invoke<TokenUsageReport>("token_usage_report", { id, dayStart }),
  rename: (id: string, title: string) => invoke<Conversation>("rename_conversation", { id, title }),
  remove: (id: string) => invoke<void>("delete_conversation", { id }),
  exportMarkdown: (id: string) => invoke<string>("export_markdown", { id }),
  setVoice: (id: string, on: boolean) => invoke<ChatEvent>("set_voice", { id, on }),
  /** Sent while the AIs are answering, the message joins the running turn; it never stops it. */
  send: (id: string, text: string, targets: string[], rounds: number, interrupt = false, attachments: Attachment[] = []) =>
    invoke<ChatEvent>("send_message", { id, text, targets, rounds, interrupt, attachments }),
  /** Saves files (by path) for the next message; Consilium keeps its own copies. */
  attachFiles: (id: string, paths: string[]) => invoke<Attachment[]>("attach_files", { id, paths }),
  /** Saves pasted file contents (a screenshot, say) for the next message. */
  attachData: (id: string, name: string, data: number[]) => invoke<Attachment>("attach_data", { id, name, data }),
  attachClipboardImage: (id: string) => invoke<Attachment>("attach_clipboard_image", { id }),
  openAttachment: (id: string, attachment: Attachment) => invoke<void>("open_attachment", { id, attachment }),
  /** Opens an installed app by name, after the human confirms a spoken "open <app>". */
  openApp: (name: string) => invoke<void>("open_app", { name }),
  /** Runs command box `index` of AI message `eventId` once; its output goes to that AI as your message. */
  linkDiscoverLan: () => invoke<string[]>("link_discover_lan"),
  linkStatus: () => invoke<LinkStatus>("link_status"),
  linkSetEnabled: (on: boolean) => invoke<LinkStatus>("link_set_enabled", { on }),
  /** Shows a one-time 6-digit code (valid 5 minutes, 3 tries) and turns the link on. */
  linkPairHost: () => invoke<{ code: string; expires_ms: number }>("link_pair_host"),
  linkPairCancel: () => invoke<void>("link_pair_cancel"),
  linkPairJoin: (address: string, code: string) => invoke<LinkPeer>("link_pair_join", { address, code }),
  linkUnpair: (nodeId: string) => invoke<LinkStatus>("link_unpair", { nodeId }),
  linkTestPeer: (nodeId: string) => invoke<PeerTestReport>("link_test_peer", { nodeId }),
  linkAddPeerRoute: (nodeId: string, address: string) =>
    invoke<string[]>("link_add_peer_route", { nodeId, address }),
  linkRefreshRoutes: (nodeId: string) =>
    invoke<string[]>("link_refresh_routes", { nodeId }),
  linkInbox: () => invoke<unknown[]>("link_inbox"),
  linkSend: (payload: unknown) => invoke<string>("link_send", { payload }),
  runCommand: (id: string, eventId: string, index: number) => invoke<ChatEvent>("run_command", { id, eventId, index }),
  /** Shows the system file picker; returns the chosen files. */
  pickFiles: async (): Promise<string[]> => {
    const r = await openDialog({ multiple: true, directory: false, title: "Attach files" });
    return Array.isArray(r) ? r : typeof r === "string" ? [r] : [];
  },
  stop: (id: string) => invoke<boolean>("stop", { id }),
  /** Lets an AI that ran out of usage be asked again now. */
  unbench: (agent: string) => invoke<void>("unbench", { agent }),
  running: () => invoke<string[]>("running"),
  /** The forecast of the next global Codex reset, when that setting is on. */
  resetForecast: () => invoke<ResetForecast | null>("reset_forecast"),
  setResetForecast: (on: boolean) => invoke<void>("set_reset_forecast", { on }),
  /** claude-resets.com's history of Claude Code resets, when the reset-forecast setting is on. */
  claudeResetHistory: () => invoke<ClaudeResetHistory | null>("claude_reset_history"),
  setAutoContinue: (on: boolean) => invoke<void>("set_auto_continue", { on }),
  setIsolateAgents: (on: boolean) => invoke<void>("set_isolate_agents", { on }),
  setAutoRunCommands: (on: boolean) => invoke<void>("set_auto_run_commands", { on }),
  setAutoInstallUpdates: (on: boolean) => invoke<void>("set_auto_install_updates", { on }),
  /** True once after a linked computer announced it is updating: check GitHub now. */
  releaseCheckRequested: () => invoke<boolean>("release_check_requested"),
  requestPeerUpdate: () => invoke<number>("request_peer_update"),
  /** Restarts Consilium on this computer, targeted linked computers, or all computers. */
  restartApp: (targets: string[]) => invoke<void>("restart_app", { targets }),
  /** Moves the link onto each route in turn, verifies it, and posts the results in the chat. */
  stressTestLink: () => invoke<StressReport[]>("stress_test_link"),
  agentBranches: (conversation?: string) => invoke<AgentBranch[]>("agent_branches", { conversation }),
  mergeAgentBranch: (repo: string, agent: string, conversation?: string) =>
    invoke<string>("merge_agent_branch", { conversation, repo, agent }),
  discardAgentBranch: (repo: string, agent: string, conversation?: string) =>
    invoke<void>("discard_agent_branch", { conversation, repo, agent }),
  /** The conversation's shared task checklist. */
  tasksGet: (id: string) => invoke<TaskBoard>("tasks_get", { id }),
  /** A change by the human; Consilium also notes it in the chat. */
  tasksApply: (id: string, op: TaskOp) => invoke<TaskBoard>("tasks_apply", { id, op }),
  saveApproval: (request: string) => invoke<Council>("save_approval", { request }),
  forgetApproval: (grant: string) => invoke<Council>("forget_approval", { grant }),
  /** Settings → Tools: not-bundled tools the AIs know about, with install state and pinned commands. */
  toolCatalog: () => invoke<CatalogTool[]>("tool_catalog"),
  localToolInventory: () => invoke<string[]>("local_tool_inventory"),
  /** Whether the `consilium` terminal command is installed. */
  quickSummonAvailable: () => invoke<boolean>("quick_summon_available"),
  cliStatus: () => invoke<CliInstall>("cli_status"),
  installCli: () => invoke<CliInstall>("install_cli"),
  /** Asks every council member for a one-word reply, in parallel. Uses a little of each AI's allowance. */
  selfCheck: () => invoke<CheckResult[]>("self_check"),
  getCouncil: () => invoke<Council>("get_council"),
  setDefaultAccess: (access: Access) => invoke<void>("set_default_access", { access }),
  setDefaultWorkspace: (folder: string | null) => invoke<string | null>("set_default_workspace", { folder }),
  getMemory: () => invoke<string>("get_memory"),
  setMemory: (text: string) => invoke<void>("set_memory", { text }),
  setMember: (agent: string, member: boolean) => invoke<void>("set_member", { agent, member }),
  /** Switches an AI on or off in one conversation; returns the AIs now off there. */
  setAiOn: (conversation: string, agent: string, on: boolean) => invoke<string[]>("set_ai_on", { conversation, agent, on }),
  setChatAutoRun: (conversation: string, on: boolean) => invoke<boolean>("set_chat_auto_run", { conversation, on }),
  chatAutoRun: (conversation: string) => invoke<boolean>("chat_auto_run", { conversation }),
  switchedOff: (conversation: string) => invoke<string[]>("switched_off", { conversation }),
  setAccess: (agent: string, access: Access) => invoke<void>("set_access", { agent, access }),
  setModel: (agent: string, model: string | null) => invoke<void>("set_model", { agent, model }),
  listModels: (agent: string) => invoke<ModelChoice[]>("list_models", { agent }),
  setWorkspace: (id: string, folder: string | null) => invoke<Conversation>("set_workspace", { id, folder }),
  /** Shows the system folder picker; returns the chosen folder or null. */
  pickFolder: async (): Promise<string | null> => {
    const r = await openDialog({ directory: true, multiple: false, title: "Choose the project folder for this conversation" });
    return typeof r === "string" ? r : null;
  },
  /** Opens a terminal with the official install or sign-in command; the person presses Enter to run it. */
  setup: (agent: string, action: "install" | "signin") => invoke<string>("setup_agent", { agent, action }),
  /** Saves an API-key AI's key on this computer (empty removes it). The key is never returned. */
  setApiKey: (agent: string, key: string) => invoke<void>("set_api_key", { agent, key }),
  addCustomAi: (name: string, baseUrl: string, model: string, local: boolean, key: string) =>
    invoke<string>("add_custom_ai", { name, baseUrl, model, local, key }),
  removeCustomAi: (id: string) => invoke<void>("remove_custom_ai", { id }),
  probeOllama: (baseUrl?: string) => invoke<OllamaModel[]>("probe_ollama", { baseUrl }),
  importOllamaModel: (model: OllamaModel, baseUrl?: string) =>
    invoke<string>("import_ollama_model", { model, baseUrl }),
  /** Samples host PC CPU and memory pressure (0-100%). */
  systemPressure: () => invoke<SystemPressure>("system_pressure"),
  requestPeerResources: () => invoke<void>("request_peer_resources"),
  peerResources: () => invoke<Record<string, {
    os?: string;
    build_arch?: string;
    total_memory_mb?: number;
    available_memory_mb?: number;
    memory_percent: number;
    tools?: string[];
    updated_at: number;
  }>>("get_peer_resources"),
  securityScanStatus: () => invoke<SecurityScanStatus>("security_scan_status"),
  securityScanSave: (settings: {
    enabled: boolean;
    interval_hours: number;
    auto_remove_malicious_extensions?: boolean;
  }) => invoke<SecurityScanStatus>("security_scan_save", { settings }),
  securityScanRun: () => invoke<SecurityScanStatus>("security_scan_run"),
  messagingStatus: () => invoke<ConnectorStatus[]>("messaging_status"),
  /** Messaging settings, phone number masked. */
  messagingConfig: () => invoke<MessagingSetup>("messaging_config"),
  /** Validates and saves messaging settings on this computer. */
  messagingSave: (change: MessagingChange) => invoke<MessagingSetup>("messaging_save", { change }),
  /** Opens a terminal for one fixed setup step. */
  messagingSetup: (action: "install" | "link" | "start") => invoke<string>("messaging_setup", { action }),
  /** Starts the in-app Signal setup: QR code, background start, then a Note to Self test. */
  signalSetupBegin: () => invoke<SignalSetupState>("signal_setup_begin"),
  signalSetupState: () => invoke<SignalSetupState>("signal_setup_state"),
  signalSetupCancel: () => invoke<void>("signal_setup_cancel"),
  /** A newer build staged for Update Now, or null. */
  updateAvailable: () => invoke<StagedUpdate | null>("update_available"),
  /** Whether a GitHub release installer is currently downloading. */
  updateDownloadStatus: () => invoke<UpdateDownloadStatus>("update_download_status"),
  requestDraftInstallers: () => invoke<string | null>("request_draft_installers"),
  /** Pauses the AIs, installs the staged build and relaunches; their turns resume afterwards. */
  updateNow: () => invoke<void>("update_now"),
  /** Downloads, verifies and stages this platform's GitHub release installer. */
  stageReleaseUpdate: (version: string, url: string, digest: string) =>
    invoke<void>("stage_release_update", { version, url, digest }),
  /** "deb" or "rpm" when this Linux copy was installed from a system package, else null. */
  linuxPackageKind: () => invoke<"deb" | "rpm" | null>("linux_package_kind"),
  /** A screen-only update that applies without a restart, or null. */
  uiUpdateAvailable: () => invoke<{ version: string } | null>("ui_update_available"),
  /** Makes the staged screen live; reload the page afterwards. */
  uiUpdateApply: () => invoke<string>("ui_update_apply"),
  /** Tells the app this page started, so it isn't rolled back. */
  uiLoaded: () => invoke<string | null>("ui_loaded"),
  /** Fetches today's Bing wallpaper metadata and image data URL. */
  getDailyWallpaper: () => invoke<DailyWallpaper | null>("get_daily_wallpaper"),
  /** Reads the most recent session safety snapshot ledger. */
  sessionSafetyStatus: () => invoke<SafetyLedger | null>("session_safety_status"),
  /** Restores a repository's session safety snapshot (merges tracked stash and restores untracked files). */
  sessionSafetyRestore: (repo?: string) => invoke<string>("session_safety_restore", { repo }),
};

export interface SnapshotReport {
  repo: string;
  name: string;
  commit: string | null;
  tracked_files: string[];
  untracked_files: string[];
}

export interface SafetyLedger {
  timestamp: number;
  version: string;
  snapshots: SnapshotReport[];
}

export interface DailyWallpaper {
  data_url: string;
  title?: string;
  copyright?: string;
  date: string;
}

export interface StagedUpdate {
  version: string;
  current: string;
}

export interface UpdateDownloadStatus {
  active: boolean;
  version: string;
}

export const api: typeof appApi = IN_APP ? appApi : (demoApi as unknown as typeof appApi);

export function onEvent(fn: (e: ChatEvent) => void): Promise<UnlistenFn> {
  if (!IN_APP) return Promise.resolve(() => undefined);
  return listen<ChatEvent>("consilium://event", (msg) => fn(msg.payload));
}

export function onTurn(fn: (t: TurnState) => void): Promise<UnlistenFn> {
  if (!IN_APP) return Promise.resolve(() => undefined);
  return listen<TurnState>("consilium://turn", (msg) => fn(msg.payload));
}

export function onActivity(fn: (activity: AgentActivity) => void): Promise<UnlistenFn> {
  if (!IN_APP) return Promise.resolve(() => undefined);
  return listen<AgentActivity>("consilium://activity", (msg) => fn(msg.payload));
}

export function senderName(sender: Sender, agents: AgentInfo[], via?: string | null): string {
  if (sender === "human") return via ? `You (${via})` : "You";
  const name = agents.find((a) => a.id === sender)?.name ?? sender;
  // An AI from another linked computer is named for it, e.g. "Claude (MacBook)".
  return via ? `${name} (${via})` : name;
}

/** One AI's isolated copy of one repository (see `worktree.rs`). */
export interface AgentBranch {
  agent: string;
  repo: string;
  branch: string;
  /** Commits a merge would bring in. */
  ahead: number;
  /** Commits on the person's checkout the copy doesn't have yet. */
  behind: number;
  /** Uncommitted files in the copy; not included in a merge. */
  uncommitted: number;
  /** "<short hash> <subject>", newest first, at most 20. */
  commits: string[];
}


export function onQuickSummon(fn: () => void): Promise<UnlistenFn> {
  return listen("consilium://quick-summon", fn);
}
