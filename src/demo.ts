// Demo mode: the real UI with invented, deterministic data, used only when Consilium's front end
// runs in an ordinary browser (for README screenshots and UI work), never inside the app.
//
//   http://127.0.0.1:1420/?step=4&theme=retro84
//
// `step` (0-5) picks a moment in the scripted conversation; `setup=1` shows the first-run setup
// state; `settings=1` opens Settings. Nothing here touches the network or real conversations.
import type { AgentInfo, ChatEvent, Conversation, TurnState } from "./api";

const params = new URLSearchParams(location.search);
export const demoStep = Math.max(0, Math.min(5, Number(params.get("step") ?? 5)));
export const demoSettings = params.get("settings") === "1";
export const demoTheme = params.get("theme");
const needsSetup = params.get("setup") === "1";

const T0 = Date.UTC(2026, 10, 14, 15, 2);
const CONV = "00000000-0000-4000-8000-000000000001";

const agents: AgentInfo[] = [
  { id: "gpt", name: "ChatGPT", company: "OpenAI", member: true, access: "edit", transport: "local-cli", tool: "Codex CLI", available: true, signed_in: true, version: "codex-cli", path: "codex", hint: "" },
  {
    id: "claude",
    name: "Claude",
    company: "Anthropic",
    member: true,
    access: "edit",
    transport: "local-cli",
    tool: "Claude Code CLI",
    available: !needsSetup,
    signed_in: !needsSetup,
    version: "Claude Code",
    path: "claude",
    hint: needsSetup ? "Run `claude auth login` in a terminal and sign in with your Claude account." : "",
  },
  {
    id: "gemini",
    name: "Gemini",
    company: "Google",
    member: false,
    access: "chat",
    transport: "local-cli",
    tool: "Gemini CLI",
    available: false,
    signed_in: false,
    version: "Gemini CLI",
    path: "gemini",
    hint: "Run `gemini` in a terminal, choose Sign in with Google, then type /quit.",
  },
];

const conversations: Conversation[] = [
  { id: CONV, title: "Caching strategy for the image service", created: T0, updated: T0 + 9 * 60e3, workspace: "/projects/image-service" },
  { id: "00000000-0000-4000-8000-000000000002", title: "Name for the new CLI", created: T0 - 864e5, updated: T0 - 860e5, workspace: null },
  { id: "00000000-0000-4000-8000-000000000003", title: "Review: retry logic in upload.ts", created: T0 - 2 * 864e5, updated: T0 - 2 * 863e5, workspace: null },
];

let seq = 0;
const ev = (sender: string, text: string, minutes: number, kind: ChatEvent["kind"] = "message", to: string[] = []): ChatEvent => ({
  id: `demo-${++seq}`,
  seq,
  ts: T0 + minutes * 60e3,
  conversation: CONV,
  sender,
  kind,
  to,
  text,
  requests: [...text.matchAll(/^\s*REQUEST:\s*(.+)$/gim)].map((m) => m[1]),
  add_ai: [],
});

const script: ChatEvent[] = [
  ev(
    "human",
    "Our image service re-renders thumbnails on every request and p95 latency is 900 ms. Should we cache at the CDN, in Redis, or both? Discuss.",
    0,
    "message",
    ["gpt", "claude"],
  ),
  ev(
    "gpt",
    "Start at the CDN. Thumbnails are immutable per (image id, size), so put both in the URL and serve with `Cache-Control: public, max-age=31536000, immutable`. Hit rates above 95% are typical, and that removes most renders outright.\n\nRedis only helps with the misses, and it adds a second invalidation path to keep correct.",
    1,
  ),
  ev(
    "claude",
    "Agree on the CDN first, with one correction: the misses are exactly the 900 ms requests users notice, and a cold CDN after a deploy or purge will send all of them to the renderer at once. I'd add request coalescing in the service (one render per key in flight) rather than Redis. It's about 30 lines and has no new infrastructure.\n\n@gpt: do you see a case where Redis still earns its keep here?",
    3,
  ),
  ev(
    "gpt",
    "Only if renders are shared across many service instances and the CDN miss rate stays high, and neither is true from what we've seen. Coalescing covers the thundering-herd case. I withdraw the Redis suggestion.\n\nREQUEST: Confirm thumbnails never change for a given image id, or tell us how edits are versioned.",
    5,
  ),
  ev("human", "Discussion paused: a question for you is waiting.", 5.2, "system"),
];

/** Events visible at each demo step, plus whether an agent is "thinking". */
function stepState(step: number): { events: ChatEvent[]; thinking: string | null } {
  switch (step) {
    case 0:
      return { events: [], thinking: null };
    case 1:
      return { events: script.slice(0, 1), thinking: "gpt" };
    case 2:
      return { events: script.slice(0, 2), thinking: "claude" };
    case 3:
      return { events: script.slice(0, 3), thinking: "gpt" };
    default:
      return { events: script, thinking: null };
  }
}

const state = stepState(demoStep);

export const demoApi = {
  agents: async () => agents,
  list: async () => (demoStep === 0 ? [{ ...conversations[0], title: "New conversation" }, ...conversations.slice(1)] : conversations),
  create: async () => conversations[0],
  load: async () => state.events,
  rename: async () => conversations[0],
  remove: async () => undefined,
  exportMarkdown: async () => "",
  send: async () => state.events[0],
  stop: async () => true,
  running: async () => (state.thinking ? [CONV] : []),
  setup: async () => "Demo",
  setMember: async () => undefined,
  setAccess: async () => undefined,
  setWorkspace: async () => conversations[0],
  pickFolder: async () => null,
  getCouncil: async () => ({ members: ["gpt", "claude"], access: {}, default_workspace: "/projects", default_access: "edit" as const }),
  setDefaultAccess: async () => undefined,
  setDefaultWorkspace: async () => "/projects",
  getMemory: async () => "- The human prefers short, direct answers (Claude, 2026-11-14)\n- Thumbnails are immutable per image id (ChatGPT, 2026-11-14)\n",
  setMemory: async () => undefined,
  selfCheck: async () => [
    { id: "gpt", name: "ChatGPT", ok: true, ms: 9400, detail: "OK" },
    { id: "claude", name: "Claude", ok: true, ms: 6100, detail: "OK" },
  ],
};
export const demoAddAi = params.get("addai") === "1";

export const demoTurn: TurnState | null = state.thinking
  ? { conversation: CONV, state: "running", agent: state.thinking, round: demoStep >= 3 ? 2 : 1, rounds: 3 }
  : null;

export const demoDraft = demoStep === 0 ? "Our image service re-renders thumbnails on every request and p95 latency is 900 ms…" : "";
