// Typed wrappers around the Rust commands, plus the two live event streams.
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { demoApi } from "./demo";

/** True inside the Consilium app; false in an ordinary browser, where the demo data is shown. */
export const IN_APP = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

export type Sender = "human" | "gpt" | "claude" | string;

export interface ChatEvent {
  id: string;
  seq: number;
  ts: number;
  conversation: string;
  sender: Sender;
  kind: "message" | "system" | "error";
  to: string[];
  text: string;
  requests: string[];
  /** AIs this message proposes adding to the council (from ADD_AI: lines). */
  add_ai: string[];
  /** Provider-reported counts for the AI turn that produced this event. */
  usage?: TokenUsage | null;
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
}

export const ACCESS_LEVELS: { id: Access; name: string; note: string }[] = [
  { id: "chat", name: "Chat only", note: "Sees only the conversation. No files, no tools." },
  { id: "read", name: "Read project", note: "Can read files in the conversation's project folder." },
  { id: "edit", name: "Edit project", note: "Can read and edit files in the project folder. No shell commands." },
  { id: "own", name: "Full (its own settings)", note: "Works in the project folder with the permissions you already approved in that AI's own app. Anything that would need your approval is refused, since nobody can approve it in the background." },
  { id: "unrestricted", name: "Unrestricted", note: "No approval checks or sandbox: runs any command, installs software and changes files in the project folder." },
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

export interface UsageStatus {
  /** Lowest remaining applicable limit; this is the window that will stop the AI first. */
  remaining_percent: number;
  windows: { label: string; remaining_percent: number }[];
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
  /** Sent while the AIs are answering, the message joins the running turn; it never stops it. */
  send: (id: string, text: string, targets: string[], rounds: number, interrupt = false) =>
    invoke<ChatEvent>("send_message", { id, text, targets, rounds, interrupt }),
  stop: (id: string) => invoke<boolean>("stop", { id }),
  /** Lets an AI that ran out of usage be asked again now. */
  unbench: (agent: string) => invoke<void>("unbench", { agent }),
  running: () => invoke<string[]>("running"),
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
};

export const api: typeof appApi = IN_APP ? appApi : (demoApi as unknown as typeof appApi);

export function onEvent(fn: (e: ChatEvent) => void): Promise<UnlistenFn> {
  if (!IN_APP) return Promise.resolve(() => undefined);
  return listen<ChatEvent>("consilium://event", (msg) => fn(msg.payload));
}

export function onTurn(fn: (t: TurnState) => void): Promise<UnlistenFn> {
  if (!IN_APP) return Promise.resolve(() => undefined);
  return listen<TurnState>("consilium://turn", (msg) => fn(msg.payload));
}

export const AGENT_COLORS: Record<string, string> = {
  human: "var(--c-human)",
  gpt: "var(--c-gpt)",
  claude: "var(--c-claude)",
};

export function senderName(sender: Sender, agents: AgentInfo[]): string {
  if (sender === "human") return "You";
  return agents.find((a) => a.id === sender)?.name ?? sender;
}
