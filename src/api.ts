// Typed wrappers around the Rust commands, plus the two live event streams.
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

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
}

export interface Conversation {
  id: string;
  title: string;
  created: number;
  updated: number;
}

export interface AgentInfo {
  id: string;
  name: string;
  transport: string;
  tool: string;
  available: boolean;
  signed_in: boolean | null;
  version: string | null;
  path: string | null;
  hint: string;
}

export interface TurnState {
  conversation: string;
  state: "running" | "idle";
  agent: string | null;
  round: number;
  rounds: number;
}

export const api = {
  agents: () => invoke<AgentInfo[]>("agents_status"),
  list: () => invoke<Conversation[]>("list_conversations"),
  create: () => invoke<Conversation>("new_conversation"),
  load: (id: string) => invoke<ChatEvent[]>("load_conversation", { id }),
  rename: (id: string, title: string) => invoke<Conversation>("rename_conversation", { id, title }),
  remove: (id: string) => invoke<void>("delete_conversation", { id }),
  exportMarkdown: (id: string) => invoke<string>("export_markdown", { id }),
  send: (id: string, text: string, targets: string[], rounds: number) =>
    invoke<ChatEvent>("send_message", { id, text, targets, rounds }),
  stop: (id: string) => invoke<boolean>("stop", { id }),
  running: () => invoke<string[]>("running"),
  /** Opens a terminal with the official install or sign-in command; the person presses Enter to run it. */
  setup: (agent: string, action: "install" | "signin") => invoke<string>("setup_agent", { agent, action }),
};

export function onEvent(fn: (e: ChatEvent) => void): Promise<UnlistenFn> {
  return listen<ChatEvent>("consilium://event", (msg) => fn(msg.payload));
}

export function onTurn(fn: (t: TurnState) => void): Promise<UnlistenFn> {
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
