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

export const IN_APP = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

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

/** The `consilium` terminal command. */
export interface CliInstall {
  installed: boolean;
  path: string;
  command: string;
  note: string;
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
  rtt_ms: number | null;
  last_seen_ms: number | null;
  /** Packets sent but not yet acknowledged; re-sent when the link comes back. */
  queued: number;
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
};

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
  openAttachment: (id: string, attachment: Attachment) => invoke<void>("open_attachment", { id, attachment }),
  /** Runs command box `index` of AI message `eventId` once; its output goes to that AI as your message. */
  linkStatus: () => invoke<LinkStatus>("link_status"),
  linkSetEnabled: (on: boolean) => invoke<LinkStatus>("link_set_enabled", { on }),
  /** Shows a one-time 6-digit code (valid 5 minutes, 3 tries) and turns the link on. */
  linkPairHost: () => invoke<{ code: string; expires_ms: number }>("link_pair_host"),
  linkPairCancel: () => invoke<void>("link_pair_cancel"),
  linkPairJoin: (address: string, code: string) => invoke<LinkPeer>("link_pair_join", { address, code }),
  linkUnpair: (nodeId: string) => invoke<LinkStatus>("link_unpair", { nodeId }),
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
  saveApproval: (request: string) => invoke<Council>("save_approval", { request }),
  forgetApproval: (grant: string) => invoke<Council>("forget_approval", { grant }),
  /** Settings → Tools: not-bundled tools the AIs know about, with install state and pinned commands. */
  toolCatalog: () => invoke<CatalogTool[]>("tool_catalog"),
  /** Whether the `consilium` terminal command is installed. */
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
  setAccess: (agent: string, access: Access) => invoke<void>("set_access", { agent, access }),
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
  /** Samples host PC CPU and memory pressure (0-100%). */
  systemPressure: () => invoke<SystemPressure>("system_pressure"),
  securityScanStatus: () => invoke<SecurityScanStatus>("security_scan_status"),
  securityScanSave: (settings: { enabled: boolean; interval_hours: number }) =>
    invoke<SecurityScanStatus>("security_scan_save", { settings }),
  securityScanRun: () => invoke<SecurityScanStatus>("security_scan_run"),
  messagingStatus: () => invoke<ConnectorStatus[]>("messaging_status"),
  /** Messaging settings, phone number masked. */
  messagingConfig: () => invoke<MessagingSetup>("messaging_config"),
  /** Validates and saves messaging settings on this computer. */
  messagingSave: (change: MessagingChange) => invoke<MessagingSetup>("messaging_save", { change }),
  /** Opens a terminal for one fixed setup step. */
  messagingSetup: (action: "install" | "link" | "start") => invoke<string>("messaging_setup", { action }),
  /** A newer build staged for Update Now, or null. */
  updateAvailable: () => invoke<StagedUpdate | null>("update_available"),
  /** Pauses the AIs, installs the staged build and relaunches; their turns resume afterwards. */
  updateNow: () => invoke<void>("update_now"),
  /** A screen-only update that applies without a restart, or null. */
  uiUpdateAvailable: () => invoke<{ version: string } | null>("ui_update_available"),
  /** Makes the staged screen live; reload the page afterwards. */
  uiUpdateApply: () => invoke<string>("ui_update_apply"),
  /** Tells the app this page started, so it isn't rolled back. */
  uiLoaded: () => invoke<string | null>("ui_loaded"),
  /** Fetches today's Bing wallpaper metadata and image data URL. */
  getDailyWallpaper: () => invoke<DailyWallpaper | null>("get_daily_wallpaper"),
};

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

export function senderName(sender: Sender, agents: AgentInfo[]): string {
  if (sender === "human") return "You";
  return agents.find((a) => a.id === sender)?.name ?? sender;
}
