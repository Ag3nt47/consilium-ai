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
export const demoVoice = params.get("voice") === "1";
export const demoWallpaper = params.get("wallpaper") === "1";
export const demoAddAi = params.get("addai") === "1";
const needsSetup = params.get("setup") === "1";

const T0 = Date.UTC(2026, 10, 14, 15, 2);
const CONV = "00000000-0000-4000-8000-000000000001";

const agents: AgentInfo[] = [
  { id: "gpt", name: "ChatGPT", company: "OpenAI", member: true, access: "edit", transport: "local-cli", tool: "Codex CLI", available: true, signed_in: true, version: "codex-cli", path: "codex", usage: { remaining_percent: 74, windows: [{ label: "5-hour limit", remaining_percent: 74 }, { label: "Weekly limit", remaining_percent: 88 }] }, hint: "" },
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
    usage: needsSetup ? null : { remaining_percent: 41, windows: [{ label: "5-hour limit", remaining_percent: 78 }, { label: "Weekly limit", remaining_percent: 41 }] },
    hint: needsSetup ? "Run `claude auth login` in a terminal and sign in with your Claude account." : "",
  },
  {
    id: "gemini",
    name: "Gemini",
    company: "Google",
    member: !demoAddAi,
    access: "edit",
    transport: "local-cli",
    tool: demoAddAi ? "Gemini CLI" : "Antigravity CLI",
    available: !demoAddAi,
    signed_in: !demoAddAi,
    version: demoAddAi ? "Gemini CLI" : "agy",
    path: demoAddAi ? "gemini" : "agy",
    usage: demoAddAi ? null : { remaining_percent: 72, windows: [{ label: "5-hour limit", remaining_percent: 72 }, { label: "Weekly limit", remaining_percent: 82 }] },
    hint: demoAddAi ? "Run `gemini` in a terminal, choose Sign in with Google, then type /quit." : "",
  },
];

const conversations: Conversation[] = [
  { id: CONV, title: "Caching strategy for the image service", created: T0, updated: T0 + 9 * 60e3, workspace: "/projects/image-service" },
  { id: "00000000-0000-4000-8000-000000000002", title: "Name for the new CLI", created: T0 - 864e5, updated: T0 - 860e5, workspace: null },
  { id: "00000000-0000-4000-8000-000000000003", title: "Review: retry logic in upload.ts", created: T0 - 2 * 864e5, updated: T0 - 2 * 863e5, workspace: null },
];

let seq = 0;
const ev = (
  sender: string,
  text: string,
  minutes: number,
  kind: ChatEvent["kind"] = "message",
  to: string[] = [],
  usage?: ChatEvent["usage"],
  seen?: string[],
): ChatEvent => ({
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
  seen: seen ?? [],
  usage: usage ?? null,
});

const script: ChatEvent[] = [
  ...(demoVoice
    ? [ev("consilium", "Voice chat is on: the human hears your replies read aloud and speaks their messages.", 0.05, "system")]
    : []),
  ev(
    "human",
    "Our image service re-renders thumbnails on every request and p95 latency is 900 ms. Should we cache at the CDN, in Redis, or both? Discuss.",
    0.1,
    "message",
    ["gpt", "claude"],
  ),
  ev("gpt", "", 0.2, "seen", [], null, ["demo-1"]),
  ev("claude", "", 0.3, "seen", [], null, ["demo-1"]),
  ev(
    "gpt",
    "Start at the CDN. Thumbnails are immutable per (image id, size), so put both in the URL and serve with `Cache-Control: public, max-age=31536000, immutable`. Hit rates above 95% are typical, and that removes most renders outright.\n\nRedis only helps with the misses, and it adds a second invalidation path to keep correct.",
    1,
    "message",
    [],
    { input_tokens: 1420, output_tokens: 310, cache_read_tokens: 0, cache_creation_tokens: 0, thinking_tokens: 0, total_tokens: 1730 },
  ),
  ev("claude", "", 1.2, "seen", [], null, ["demo-4"]),
  ev(
    "claude",
    "Agree on the CDN first, with one correction: the misses are exactly the 900 ms requests users notice, and a cold CDN after a deploy or purge will send all of them to the renderer at once. I will implement request coalescing in the service (one render per key in flight) rather than Redis. It's about 30 lines and has no new infrastructure.\n\n@gpt: do you see a case where Redis still earns its keep here?",
    3,
    "message",
    [],
    { input_tokens: 1850, output_tokens: 420, cache_read_tokens: 1200, cache_creation_tokens: 0, thinking_tokens: 0, total_tokens: 2270 },
  ),
  ev("gpt", "", 3.2, "seen", [], null, ["demo-6"]),
  ev(
    "gpt",
    "Only if renders are shared across many service instances and the CDN miss rate stays high, and neither is true from what we've seen. Coalescing covers the thundering-herd case. I withdraw the Redis suggestion.\n\nREQUEST: Confirm thumbnails never change for a given image id, or tell us how edits are versioned.",
    5,
    "message",
    [],
    { input_tokens: 2100, output_tokens: 380, cache_read_tokens: 0, cache_creation_tokens: 0, thinking_tokens: 0, total_tokens: 2480 },
  ),
  ev("claude", "", 5.2, "seen", [], null, ["demo-8"]),
  ev("human", "Discussion paused: a question for you is waiting.", 5.3, "system"),
];

/** Events visible at each demo step, plus whether an agent is "thinking". */
function stepState(step: number): { events: ChatEvent[]; thinking: string | null } {
  const off = demoVoice ? 1 : 0;
  switch (step) {
    case 0:
      return { events: demoVoice ? script.slice(0, 1) : [], thinking: null };
    case 1:
      return { events: script.slice(0, off + 3), thinking: "gpt" };
    case 2:
      return { events: script.slice(0, off + 5), thinking: "claude" };
    case 3:
      return { events: script.slice(0, off + 7), thinking: "gpt" };
    case 4:
      return { events: script.slice(0, off + 9), thinking: null };
    default:
      return { events: script, thinking: null };
  }
}

const state = stepState(demoStep);

export const demoApi = {
  agents: async () => agents,
  usage: async () => Object.fromEntries(agents.flatMap((a) => (a.usage ? [[a.id, a.usage]] : []))),
  list: async () => (demoStep === 0 ? [{ ...conversations[0], title: "New conversation" }, ...conversations.slice(1)] : conversations),
  create: async () => conversations[0],
  load: async () => state.events,
  tokenUsage: async () => ({
    conversation: {
      gpt: { input_tokens: 3520, output_tokens: 690, cache_read_tokens: 0, cache_creation_tokens: 0, thinking_tokens: 0, total_tokens: 4210 },
      claude: { input_tokens: 1850, output_tokens: 420, cache_read_tokens: 1200, cache_creation_tokens: 0, thinking_tokens: 0, total_tokens: 2270 },
    },
    today: {
      gpt: { input_tokens: 12400, output_tokens: 2950, cache_read_tokens: 0, cache_creation_tokens: 0, thinking_tokens: 0, total_tokens: 15350 },
      claude: { input_tokens: 18100, output_tokens: 3800, cache_read_tokens: 7500, cache_creation_tokens: 0, thinking_tokens: 0, total_tokens: 21900 },
    },
  }),
  rename: async () => conversations[0],
  remove: async () => undefined,
  exportMarkdown: async () => "",
  setVoice: async (_id: string, on: boolean) =>
    ev("consilium", on ? "Voice chat is on." : "Voice chat is off.", 6, "system"),
  send: async () => state.events[0],
  stop: async () => true,
  running: async () => (state.thinking ? [CONV] : []),
  setup: async () => "Demo",
  setApiKey: async () => undefined,
  updateAvailable: async () => null,
  updateNow: async () => undefined,
  uiUpdateAvailable: async () => null,
  uiUpdateApply: async () => "",
  uiLoaded: async () => null,
  messagingStatus: async () => [
    { id: "signal", name: "Signal", state: "ok" as const, detail: "Connected to signal-cli 0.13.9 on this computer" },
  ],
  messagingConfig: async () => ({
    enabled: false,
    endpoint: "127.0.0.1:7583",
    account_hint: "",
    allowed_acis: [],
    auto_forward_decisions: true,
    signal_cli: null,
    can_install: true,
    install_running: false,
    install_error: null,
    config_path: "~/.config/consilium/messaging.json",
  }),
  messagingSave: async () => {
    throw new Error("Not available in the browser preview");
  },
  messagingSetup: async () => "",
  systemPressure: async () => ({
    percent: 24,
    cpu_percent: 18,
    memory_percent: 32,
    cores: 8,
    total_memory_mb: 16_384,
    label: "Low",
    hardware_profile: "performance" as const,
    adaptive_mode: "full" as const,
    recommended_parallel_agents: 3,
    agents: { gpt: 6, claude: 11 },
  }),
  setMember: async () => undefined,
  setAccess: async () => undefined,
  setWorkspace: async () => conversations[0],
  pickFolder: async () => null,
  getCouncil: async () => ({ members: ["gpt", "claude"], access: {}, default_workspace: "/projects", default_access: "edit" as const }),
  setDefaultAccess: async () => undefined,
  setDefaultWorkspace: async () => "/projects",
  getMemory: async () => "- The human prefers short, direct answers (Claude, 2026-11-14)\n- Thumbnails are immutable per image id (ChatGPT, 2026-11-14)\n",
  setMemory: async () => undefined,
  resetForecast: async () => null,
  attachFiles: async () => [],
  attachData: async (_id: string, name: string) => ({ name, path: name, kind: "image", size: 1 }),
  openAttachment: async () => undefined,
  runCommand: async () => { throw new Error("Run works in the app, not the demo"); },
  pickFiles: async () => [],
  setResetForecast: async () => undefined,
  cliStatus: async () => ({ installed: true, path: "~/.local/bin/consilium", command: "consilium chat", note: "" }),
  installCli: async () => ({ installed: true, path: "~/.local/bin/consilium", command: "consilium chat", note: "" }),
  selfCheck: async () => [
    { id: "gpt", name: "ChatGPT", ok: true, ms: 9400, detail: "OK" },
    { id: "claude", name: "Claude", ok: true, ms: 6100, detail: "OK" },
  ],
  getDailyWallpaper: async () =>
    demoWallpaper
      ? {
          title: "Pacific Coast Sunset",
          copyright: "Bing Daily Wallpaper",
          url: "https://www.bing.com",
          data_url: "data:image/svg+xml;utf8,<svg xmlns='http://www.w3.org/2000/svg' width='1920' height='1080'><defs><linearGradient id='g' x1='0' y1='0' x2='1' y2='1'><stop offset='0%' stop-color='%230f172a'/><stop offset='50%' stop-color='%231e1b4b'/><stop offset='100%' stop-color='%23020617'/></linearGradient></defs><rect width='100%' height='100%' fill='url(%23g)'/><circle cx='60%' cy='40%' r='300' fill='%23312e81' opacity='0.4'/></svg>",
        }
      : null,
  linkStatus: async () => ({
    enabled: true,
    node_id: "demo-node-mac",
    name: "MacBook-Pro",
    port: 8470,
    addrs: ["192.168.1.15:8470"],
    pairing_code_expires_ms: null,
    peers: [
      {
        node_id: "demo-node-win",
        name: "Windows-Desktop",
        connected: true,
        route: "192.168.1.42:8470",
        rtt_ms: 3,
        last_seen_ms: Date.now() - 1200,
        queued: 0,
      },
    ],
    error: null,
  }),
  linkSetEnabled: async (on: boolean) => ({
    enabled: on,
    node_id: "demo-node-mac",
    name: "MacBook-Pro",
    port: 8470,
    addrs: ["192.168.1.15:8470"],
    pairing_code_expires_ms: null,
    peers: [],
    error: null,
  }),
  linkPairHost: async () => ({ code: "847291", expires_ms: Date.now() + 5 * 60 * 1000 }),
  linkPairCancel: async () => undefined,
  linkPairJoin: async (_address: string, _code: string) => ({
    node_id: "demo-node-win",
    name: "Windows-Desktop",
    connected: true,
    route: "192.168.1.42:8470",
    rtt_ms: 3,
    last_seen_ms: Date.now(),
    queued: 0,
  }),
  linkUnpair: async () => ({
    enabled: true,
    node_id: "demo-node-mac",
    name: "MacBook-Pro",
    port: 8470,
    addrs: ["192.168.1.15:8470"],
    pairing_code_expires_ms: null,
    peers: [],
    error: null,
  }),
  linkSend: async () => "demo-pkt-1",
};

export const demoTurn: TurnState | null = state.thinking
  ? { conversation: CONV, state: "running", agent: state.thinking, round: demoStep >= 3 ? 2 : 1, rounds: 3 }
  : null;

export const demoDraft = demoStep === 0 ? "Our image service re-renders thumbnails on every request and p95 latency is 900 ms…" : "";
