// Demo mode: the real UI with invented, deterministic data, used only when Consilium's front end
// runs in an ordinary browser (for README screenshots and UI work), never inside the app.
//
//   http://127.0.0.1:1420/?step=4&theme=retro84
//
// `step` (0-5) picks a moment in the scripted conversation; `setup=1` shows the first-run setup
// state; `settings=1` opens Settings. Nothing here touches the network or real conversations.
import type { AgentInfo, ChatEvent, Conversation, TaskBoard, TurnState } from "./api";

const params = new URLSearchParams(location.search);
// `scene=build` swaps in a six-AI council building an app together (the README's animated demo).
export const demoScene = params.get("scene") === "build" ? "build" : "default";
export const demoStep = Math.max(0, Math.min(demoScene === "build" ? 99 : 5, Number(params.get("step") ?? 99)));
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
    usage: demoAddAi ? null : { remaining_percent: 72, windows: [{ label: "Five Hour Limit", remaining_percent: 72 }, { label: "Weekly Limit", remaining_percent: 82 }] },
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

const demoBoard: TaskBoard = {
  next: 4,
  tasks: [
    { id: 1, title: "Task board in the sidebar", owner: "claude", status: "done", by: "claude", updated: 0 },
    { id: 2, title: "Signal REQUEST relay", owner: "gemini", status: "doing", by: "gemini", updated: 0 },
    { id: 3, title: "Pairing timeout and route limits", owner: "claude", status: "blocked", note: "waiting for 0.3.21", by: "claude", updated: 0 },
    { id: 4, title: "Security review of link.rs", owner: null, status: "todo", by: "human", updated: 0 },
  ],
};

// ---- scene=build: six AIs planning, splitting and shipping an app together ----------------------
const BUILD = "00000000-0000-4000-8000-000000000010";
const u = (pct: number, week: number) => ({ remaining_percent: Math.min(pct, week), windows: [{ label: "5-hour limit", remaining_percent: pct }, { label: "Weekly limit", remaining_percent: week }] });
const buildAgents: AgentInfo[] = [
  { ...agents[0], usage: u(81, 90) },
  { ...agents[1], available: true, signed_in: true, hint: "", usage: u(76, 64) },
  { ...agents[2], member: true, available: true, signed_in: true, tool: "Antigravity CLI", version: "agy", path: "agy", hint: "", usage: u(69, 85) },
  { id: "grok", name: "Grok", company: "xAI", member: true, access: "edit", transport: "api", custom: false, tool: "xAI API", available: true, signed_in: true, version: "grok-4", path: null, usage: u(88, 93), hint: "" },
  { id: "deepseek", name: "DeepSeek", company: "DeepSeek", member: true, access: "edit", transport: "api", custom: false, tool: "DeepSeek API", available: true, signed_in: true, version: "deepseek-reasoner", path: null, usage: u(95, 97), hint: "" },
  { id: "llama", name: "Llama 3", company: "Meta · Ollama", member: true, access: "edit", transport: "openai-compatible", custom: true, tool: "Ollama (Computer B)", available: true, signed_in: true, version: "llama3.1:8b", path: "http://localhost:11434/v1", usage: null, hint: "" },
];
const buildConversations: Conversation[] = [
  { id: BUILD, title: "Fieldline v1: plan, build, ship", created: T0, updated: T0 + 95 * 60e3, workspace: "/projects/fieldline" },
  ...conversations.map((c) => ({ ...c, id: c.id, updated: c.updated - 864e5 })),
];
const tok = (i: number, o: number) => ({ input_tokens: i, output_tokens: o, cache_read_tokens: 0, cache_creation_tokens: 0, thinking_tokens: 0, total_tokens: i + o });
let bseq = 100;
const bev = (sender: string, text: string, minutes: number, kind: ChatEvent["kind"] = "message", to: string[] = [], seen: string[] = []): ChatEvent => ({
  id: `build-${++bseq}`,
  seq: bseq,
  ts: T0 + minutes * 60e3,
  conversation: BUILD,
  sender,
  kind,
  to,
  text,
  requests: [...text.matchAll(/^\s*REQUEST:\s*(.+)$/gim)].map((m) => m[1]),
  add_ai: [],
  seen,
  usage: kind === "message" && sender !== "human" ? tok(1800 + text.length * 3, 200 + text.length) : null,
});
const ALL = ["gpt", "claude", "gemini", "grok", "deepseek", "llama"];
const buildScript: ChatEvent[] = [
  bev("human", "Let's build Fieldline: a field-service app with a web dashboard, an iOS/Android app, a REST API and Stripe billing. Plan it, split the work between you, and ship v1 behind green CI.", 0.1, "message", ALL),
  bev("claude", "", 0.2, "seen", [], ["build-101"]),
  bev("claude", "Plan: one monorepo, six tracks, one git worktree each. Nothing merges to main without CI passing on macOS, Windows and Linux.\n\n- **API + data model** (Rust/axum, Postgres): me\n- **Web dashboard**: @gpt\n- **Mobile app** (Expo, offline-first): @gemini\n- **Billing** (Stripe): @grok\n- **Scheduling + route optimizer**: @deepseek\n- **Seed data + API docs**, local only: @llama\n\nTASK: take API and data model", 1.5),
  bev("gpt", "TASK: take Web dashboard\nI'll generate the TypeScript client from Claude's OpenAPI spec, so the dashboard and the API can't drift apart.", 2.4),
  bev("gemini", "TASK: take Mobile app\nJobs, photos and signatures queue on the device and sync when the technician gets signal back.", 3.1),
  bev("grok", "TASK: take Stripe billing\nPer-seat subscriptions plus metered SMS. Webhooks are verified with the signing secret, and no keys go in the repo.", 3.8),
  bev("deepseek", "TASK: take Route optimizer\nI'll treat each day as a vehicle-routing problem with time windows. Target: 200 jobs per day planned in under 300 ms.", 4.6),
  bev("llama", "TASK: take Seed data and API docs\nI run on Computer B through Ollama, so the sample customers I generate never leave this network.", 5.2),
  bev("claude", "API is on main: 42 endpoints, 186 tests passing. @gpt the spec is at `/openapi.json`; @deepseek the job-queue contract is in `docs/jobs.md`.", 38),
  bev("gpt", "Reviewing @grok's billing branch: the webhook handler has no idempotency key, so a retried `invoice.paid` would credit the account twice. Please key on `event.id`.", 52),
  bev("grok", "Good catch. Fixed in `3f1c2ab`: events are stored by id and replays are ignored, with a test that sends the same webhook twice. Merged.", 58),
  bev("deepseek", "Optimizer merged: 7.4% less drive time than nearest-neighbour on the sample week, 212 ms for 200 jobs.", 71),
  bev("consilium", "GitHub checks passed for fieldline main @9c41e07 (macOS, Windows, Linux).", 88, "system"),
  bev("claude", "All six tracks are merged and CI is green on all three systems. v1 is ready.\n\nREQUEST: Tag v1.0.0 and deploy it to staging?", 95),
];
// Each step reveals the next message; the AI that speaks after it shows as "thinking".
const buildMsgIdx = buildScript.flatMap((e, i) => (e.kind === "seen" ? [] : [i]));
const buildMaxStep = buildMsgIdx.length;
function buildState(step: number): { events: ChatEvent[]; thinking: string | null } {
  const n = Math.min(step, buildMaxStep);
  const end = n === 0 ? 0 : buildMsgIdx[n - 1] + 1;
  const events = buildScript.slice(0, end + (buildScript[end]?.kind === "seen" ? 1 : 0));
  const next = buildScript[buildMsgIdx[n]];
  return { events, thinking: n > 0 && next && next.sender !== "consilium" ? next.sender : null };
}
// Task board follows the conversation: claimed when its owner says so, done when merged.
const buildTasks: [string, string, number, number][] = [
  ["API and data model", "claude", 2, 8],
  ["Web dashboard", "gpt", 3, 12],
  ["Mobile app", "gemini", 4, 12],
  ["Stripe billing", "grok", 5, 10],
  ["Route optimizer", "deepseek", 6, 11],
  ["Seed data and API docs", "llama", 7, 12],
];
function buildBoard(step: number): TaskBoard {
  const shown = step >= 2 ? buildTasks : [];
  return {
    next: shown.length + 1,
    tasks: shown.map(([title, owner, take, done], i) => ({
      id: i + 1,
      title,
      owner: step >= take ? owner : null,
      status: step >= done ? "done" : step >= take ? "doing" : "todo",
      by: step >= take ? owner : "claude",
      updated: 0,
    })),
  };
}
const isBuild = demoScene === "build";
const demoTitles = new Map<string, string>();
const buildNow = isBuild ? buildState(demoStep) : null;
const activeState = buildNow ?? state;
const activeAgents = isBuild ? buildAgents : agents;
const activeBoard: TaskBoard = isBuild ? buildBoard(demoStep) : demoBoard;
const ACTIVE_CONV = isBuild ? BUILD : CONV;

const demoCouncilForApprovals = { members: isBuild ? ALL : ["gpt", "claude", "gemini"], access: {}, default_workspace: null, default_access: "chat" as const, saved_grants: [] };

function listedConversations() {
  const chats = isBuild ? buildConversations : conversations;
  return chats.map((chat, index) => ({
    ...chat,
    title: demoTitles.get(chat.id) ?? (demoStep === 0 && index === 0 ? "New conversation" : chat.title),
  }));
}

export const demoApi = {
  localToolInventory: async () => [],
  peerResources: async () => ({}),
  requestPeerResources: async () => "demo-memory-scan",
  agents: async () => activeAgents,
  usage: async () => Object.fromEntries(activeAgents.flatMap((a) => (a.usage ? [[a.id, a.usage]] : []))),
  list: async () => listedConversations(),
  create: async () => (isBuild ? buildConversations[0] : conversations[0]),
  load: async () => activeState.events,
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
  rename: async (id: string, title: string) => {
    const chat = listedConversations().find((entry) => entry.id === id);
    if (!chat) throw new Error("Conversation not found");
    demoTitles.set(id, title);
    return { ...chat, title };
  },
  remove: async () => undefined,
  exportMarkdown: async () => "",
  setVoice: async (_id: string, on: boolean) =>
    ev("consilium", on ? "Voice chat is on." : "Voice chat is off.", 6, "system"),
  send: async () => state.events[0],
  stop: async () => true,
  running: async () => (activeState.thinking ? [ACTIVE_CONV] : []),
  setup: async () => "Demo",
  setApiKey: async () => undefined,
  addCustomAi: async (name: string) => name.toLowerCase().replace(/[^a-z0-9]/g, ""),
  removeCustomAi: async () => undefined,
  updateAvailable: async () => null,
  updateDownloadStatus: async () => ({ active: false, version: "" }),
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
  signalSetupBegin: async () => ({ phase: "linking" as const, link_uri: "sgnl://linkdevice?uuid=demo&pub_key=demo", code: null, detail: "On your phone: Signal › Settings › Linked devices › Link new device, then scan this code.", device_name: "Consilium (Demo-Mac)" }),
  signalSetupState: async () => ({ phase: "idle" as const, link_uri: null, code: null, detail: "", device_name: "Consilium (Demo-Mac)" }),
  signalSetupCancel: async () => {},
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
  securityScanStatus: async () => ({
    installed: false,
    verified: false,
    tool_path: "~/Library/Application Support/Consilium/security/zero-breach/zero-breach-toolkit.py",
    enabled: true,
    interval_hours: 168,
    auto_remove_malicious_extensions: true,
    running: false,
    last_started_ms: 0,
    last_finished_ms: 0,
    last_exit_code: null,
    last_summary: "",
    last_report: null,
    paused_reason: "Waiting for the approved Zero Breach install.",
  }),
  securityScanSave: async (settings: {
    enabled: boolean;
    interval_hours: number;
    auto_remove_malicious_extensions?: boolean;
  }) => ({
    installed: false,
    verified: false,
    tool_path: "~/Library/Application Support/Consilium/security/zero-breach/zero-breach-toolkit.py",
    auto_remove_malicious_extensions: true,
    ...settings,
    running: false,
    last_started_ms: 0,
    last_finished_ms: 0,
    last_exit_code: null,
    last_summary: "",
    last_report: null,
    paused_reason: settings.enabled ? "Waiting for the approved Zero Breach install." : "Background scans are off.",
  }),
  securityScanRun: async () => { throw new Error("Zero Breach isn't installed in the demo"); },
  setMember: async () => undefined,
  setAccess: async () => undefined,
  setWorkspace: async () => conversations[0],
  pickFolder: async () => null,
  getCouncil: async () => ({ members: isBuild ? ALL : ["gpt", "claude"], access: {}, default_workspace: "/projects", default_access: "edit" as const }),
  setDefaultAccess: async () => undefined,
  setDefaultWorkspace: async () => "/projects",
  getMemory: async () => "- The human prefers short, direct answers (Claude, 2026-11-14)\n- Thumbnails are immutable per image id (ChatGPT, 2026-11-14)\n",
  setMemory: async () => undefined,
  resetForecast: async () => null,
  claudeResetHistory: async () => null,
  attachFiles: async () => [],
  attachData: async (_id: string, name: string) => ({ name, path: name, kind: "image", size: 1 }),
  openAttachment: async () => undefined,
  openApp: async () => undefined,
  runCommand: async () => { throw new Error("Run works in the app, not the demo"); },
  pickFiles: async () => [],
  setResetForecast: async () => undefined,
  setAutoContinue: async () => undefined,
  setIsolateAgents: async () => undefined,
  setAutoRunCommands: async () => undefined,
  agentBranches: async () => [],
  mergeAgentBranch: async () => "0000000",
  discardAgentBranch: async () => undefined,
  tasksGet: async () => activeBoard,
  tasksApply: async (_id: string, op: { op: string; task?: string; title?: string; to?: string }) => {
    const num = Number((op.task ?? "").replace("#", ""));
    const t = activeBoard.tasks.find((x) => x.id === num);
    if (op.op === "add" && op.title) activeBoard.tasks.push({ id: ++activeBoard.next, title: op.title, owner: null, status: "todo", by: "human", updated: Date.now() });
    if (t && op.op === "done") t.status = "done";
    if (t && op.op === "reopen") t.status = t.owner ? "doing" : "todo";
    if (t && op.op === "hand" && op.to) Object.assign(t, { owner: op.to, status: "doing" });
    if (t && op.op === "drop") activeBoard.tasks = activeBoard.tasks.filter((x) => x !== t);
    return { ...activeBoard, tasks: [...activeBoard.tasks] };
  },
  saveApproval: async () => ({ ...demoCouncilForApprovals }),
  forgetApproval: async () => ({ ...demoCouncilForApprovals }),
  toolCatalog: async () => [
    { name: "AnonRouter", source: "https://anonrouter.ai", about: "privacy router for AI model APIs", installed: null, detectable: false, install_command: "pip3 install --user anonrouter-confidential==0.1.3.post1" },
    { name: "REA (Reverse Engineer Anything)", source: "https://github.com/morluto/rea", about: "reverse-engineering MCP server", installed: "/usr/local/bin/rea", detectable: true, install_command: "npm install --global --ignore-scripts rea-agents@6.9.0" },
    { name: "Ghidra", source: "https://github.com/NationalSecurityAgency/ghidra", about: "decompiler REA uses for native programs", installed: null, detectable: true, install_command: null },
  ],
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
  linkDiscoverLan: async () => [],
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
        addrs: ["192.168.1.42:8470", "192.168.1.170:8470", "169.254.12.34:8470"],
        rtt_ms: 3,
        last_seen_ms: Date.now() - 1200,
        queued: 0,
        activity: ["Gemini in Plans"],
        git: "consilium-ai · main · clean",
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
    addrs: ["192.168.1.42:8470", "192.168.1.170:8470", "169.254.12.34:8470"],
    rtt_ms: 3,
    last_seen_ms: Date.now(),
    queued: 0,
    activity: [],
    git: null,
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
  linkProbeSources: async () => [{ address: "192.168.1.15", interface: "Wi-Fi" }],
  linkTransferTrial: async () => {
    throw new Error("Open the desktop app to send an authenticated sample to a linked computer.");
  },
  linkTestPeer: async (nodeId: string, source?: string) => ({
    node_id: nodeId,
    name: "Demo Peer",
    connected: true,
    active_route: "192.168.1.157:8470",
    handshake_ok: true,
    rtt_ms: 4,
    source: source ? { address: source, interface: "Wi-Fi" } : null,
    routes: [
      { addr: "192.168.1.157:8470", kind: "IP route", reachable: true, rtt_ms: 4, authenticated_ms: 9, status: "Authenticated probe passed in 9 ms" },
      { addr: "192.168.1.170:8470", kind: "IP route", reachable: true, rtt_ms: 6, status: "TCP connected in 6 ms · peer authentication unconfirmed" },
      { addr: "169.254.12.34:8470", kind: "Direct Cable / Bluetooth (Link-Local)", reachable: false, status: "Timed out" },
    ],
    queued_outgoing: 0,
    summary: "Authenticated session via 192.168.1.157:8470 · 4 ms round trip · 0 outgoing queued",
  }),
  linkAddPeerRoute: async (_nodeId: string, address: string) => ["192.168.1.157:8470", address],
  linkTestAiHandshake: async () => [
    { ai: "claude", side: "here" as const, handle: "@claude", reads: true, read: true },
    { ai: "gpt", side: "there" as const, handle: "@gpt-demo", reads: true, read: true },
  ],
  linkRefreshRoutes: async () => ["192.168.1.157:8470", "192.168.1.170:8470"],
  linkInbox: async () => [],
  linkSend: async () => "demo-pkt-1",
  sessionSafetyStatus: async () => null,
  sessionSafetyRestore: async () => "Demo: restored session safety snapshot",
  // Calls the app gained after demo mode was written; harmless no-ops in the browser preview.
  chatAutoRun: async () => false,
  setChatAutoRun: async (_c: string, on: boolean) => on,
  switchedOff: async () => [] as string[],
  setAiOn: async () => [] as string[],
  setModel: async () => undefined,
  listModels: async () => [],
  unbench: async () => undefined,
  probeOllama: async () => [],
  importOllamaModel: async () => "llama",
  linuxPackageKind: async () => null,
  releaseCheckRequested: async () => false,
  requestPeerUpdate: async () => 0,
  restartApp: async () => undefined,
  setAutoInstallUpdates: async () => undefined,
  stageReleaseUpdate: async () => undefined,
  stressTestLink: async () => [],
};

export const demoTurn: TurnState | null = activeState.thinking
  ? { conversation: ACTIVE_CONV, state: "running", agent: activeState.thinking, round: isBuild ? 1 : demoStep >= 3 ? 2 : 1, rounds: isBuild ? 1 : 3 }
  : null;

export const demoDraft = demoStep !== 0 ? "" : isBuild ? "Let's build Fieldline: a field-service app with a web dashboard, an iOS/Android app…" : demoStep === 0 ? "Our image service re-renders thumbnails on every request and p95 latency is 900 ms…" : "";
