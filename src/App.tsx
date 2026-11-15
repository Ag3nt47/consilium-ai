import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { ACCESS_LEVELS, api, FORECAST_SITE, IN_APP, onEvent, onTurn, senderName, type Access, type AgentInfo, type CheckResult, type CliInstall, type Council, type ChatEvent, type Conversation, type ResetForecast, type TurnState, type UsageWindow, type Attachment } from "./api";
import { checkForUpdate, RELEASES_URL, type UpdateInfo } from "./updates";
import { demoAddAi, demoDraft, demoSettings, demoTheme, demoTurn } from "./demo";

const THEMES = [
  { id: "consilium", name: "Consilium", note: "Indigo, ivory and gold" },
  { id: "cyber", name: "Cyber", note: "From the UI Design Bundle" },
  { id: "slate", name: "Slate", note: "From the UI Design Bundle" },
  { id: "crt", name: "CRT", note: "Green phosphor terminal" },
  { id: "retro84", name: "Retro '84", note: "Neon grid, scanlines, glow" },
] as const;

type Prefs = { theme: string; effects: boolean; autoUpdate: boolean };
const PREFS_KEY = "consilium.prefs";

function loadPrefs(): Prefs {
  const defaults: Prefs = { theme: "consilium", effects: true, autoUpdate: true };
  if (!IN_APP) return { ...defaults, theme: demoTheme || defaults.theme, autoUpdate: false };
  try {
    return { ...defaults, ...JSON.parse(localStorage.getItem(PREFS_KEY) || "{}") };
  } catch {
    return defaults;
  }
}

function savePrefs(p: Prefs) {
  try {
    localStorage.setItem(PREFS_KEY, JSON.stringify(p));
  } catch {
    /* not saved; the choice still applies until the app closes */
  }
}

/** Copies text, falling back to a hidden text area where the async clipboard API is refused. */
async function copyText(text: string) {
  try {
    await navigator.clipboard.writeText(text);
    return;
  } catch {
    /* fall through to the older method */
  }
  const area = document.createElement("textarea");
  area.value = text;
  area.setAttribute("readonly", "");
  area.style.position = "fixed";
  area.style.opacity = "0";
  document.body.appendChild(area);
  area.select();
  const ok = document.execCommand("copy");
  area.remove();
  if (!ok) throw new Error("the clipboard isn't available");
}

function time(ts: number) {
  return new Date(ts).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

/** A line of text with `inline code` spans rendered as code. */
function Inline({ text }: { text: string }) {
  const parts = text.split(/(`[^`\n]+`)/g);
  return (
    <>
      {parts.map((p, i) => (p.length > 2 && p.startsWith("`") && p.endsWith("`") ? <code key={i}>{p.slice(1, -1)}</code> : p))}
    </>
  );
}

/** Message text with REQUEST: lines shown as red request callouts. */
function MessageBody({ text }: { text: string }) {
  const lines = text.split("\n");
  return (
    <div className="body">
      {lines.map((line, i) => {
        const add = /^\s*ADD_AI:\s*(.*)$/i.exec(line);
        if (add && add[1].trim()) {
          return (
            <div key={i} className="addai-line">
              <span className="addai-label">ADD AI:</span> {add[1]}
            </div>
          );
        }
        const m = /^\s*REQUEST:\s*(.*)$/i.exec(line);
        if (m && m[1].trim()) {
          return (
            <div key={i} className="request">
              <span className="request-label">REQUEST:</span> <Inline text={m[1]} />
            </div>
          );
        }
        return <div key={i} className={line.trim() ? "line" : "gap"}><Inline text={line} /></div>;
      })}
    </div>
  );
}

function agentState(a: AgentInfo): "ready" | "signin" | "missing" | "benched" {
  if (a.available && a.benched_until && a.benched_until > Date.now()) return "benched";
  return a.available ? "ready" : a.path ? "signin" : "missing";
}

function attachmentIcon(kind: string): string {
  return kind === "image" ? "🖼" : kind === "pdf" ? "📕" : kind === "text" ? "📄" : "📎";
}

function fileSize(bytes: number): string {
  return bytes >= 1048576 ? `${(bytes / 1048576).toFixed(1)} MB` : bytes >= 1024 ? `${Math.round(bytes / 1024)} KB` : `${bytes} B`;
}

/** The soonest reset among `windows` that is still ahead. */
function nextReset(windows: UsageWindow[]): number | null {
  const now = Date.now();
  const ahead = windows.map((w) => w.resets_at ?? 0).filter((t) => t > now);
  return ahead.length ? Math.min(...ahead) : null;
}

function UsageBar({ label, percent, windows, aria, note }: { label: string; percent: number; windows: UsageWindow[]; aria: string; note?: string }) {
  const remaining = Math.max(0, Math.min(100, Math.round(percent)));
  const color = `hsl(${remaining * 1.2} 72% 46%)`;
  const reset = nextReset(windows);
  const details = windows
    .map((w) => `${w.label}: ${w.remaining_percent}% left${w.resets_at ? `, resets ${resetTime(w.resets_at)}` : ""}`)
    .join("\n");
  return (
    <div className="usage-row" title={note ? `${note}\n${details}` : details}>
      <div className="usage-label">
        <span className="usage-name">{label}</span>
        <span>
          <b>{remaining}%</b>
          {reset && <small> · resets {resetTime(reset)}</small>}
        </span>
      </div>
      <div
        className="usage-track"
        role="meter"
        aria-label={aria}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={remaining}
        aria-valuetext={`${remaining}% left. ${details}`}
      >
        <span style={{ width: `${remaining}%`, backgroundColor: color }} />
      </div>
    </div>
  );
}

/** The model in use, with the AI's whole plan underneath; for ChatGPT, the reset forecast if on. */
function UsageMeter({ agent, forecast }: { agent: AgentInfo; forecast?: ResetForecast | null }) {
  const usage = agent.usage;
  if (!usage) {
    return agent.available ? (
      <span className="usage-unavailable" title={`${agent.tool} did not report plan usage`}>
        usage unavailable
      </span>
    ) : null;
  }
  const { model, overall } = usage;
  return (
    <div className="usage-meter">
      {model && (
        <UsageBar
          label={model.shared ? `${model.label} · shares overall` : model.label}
          percent={model.remaining_percent}
          windows={model.windows}
          aria={`${model.label} usage remaining`}
          note={model.shared ? `${model.label} has no separate limit; it uses ${agent.name}'s overall allowance.` : undefined}
        />
      )}
      {overall ? (
        <UsageBar label={`${agent.name} overall`} percent={overall.remaining_percent} windows={overall.windows} aria={`${agent.name} overall usage remaining`} />
      ) : (
        !model && <UsageBar label="usage left" percent={usage.remaining_percent} windows={usage.windows} aria={`${agent.name} plan usage remaining`} />
      )}
      {forecast && (
        <div className="usage-forecast" title={`Unofficial estimate${forecast.confidence ? `, ${forecast.confidence} confidence` : ""}. Not affiliated with OpenAI.`}>
          {forecast.official
            ? `Reset announced: ${forecast.official}`
            : `Global reset: ${forecast.chance_24h ?? "?"}% chance in 24 h`}
          {" · "}
          <button className="link" onClick={() => openUrl(FORECAST_SITE)}>
            data: codex-reset.com
          </button>
        </div>
      )}
    </div>
  );
}

/** "3:40 PM", or "Mon 3:40 PM" when it isn't today. */
function resetTime(ts: number): string {
  const d = new Date(ts);
  const time = d.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
  return d.toDateString() === new Date().toDateString() ? time : `${d.toLocaleDateString([], { weekday: "short" })} ${time}`;
}

function AgentDot({ agent, onSetup, onRetry, check, forecast }: { agent: AgentInfo; onSetup: (a: AgentInfo) => void; onRetry: (a: AgentInfo) => void; check?: CheckResult | "running"; forecast?: ResetForecast | null }) {
  const state = agentState(agent);
  const label = { ready: "Ready", signin: "Not signed in", missing: "Not installed", benched: "Out of usage" }[state];
  return (
    <div className={`agent agent-${agent.id}`} title={agent.hint || `${agent.tool} ${agent.version ?? ""}`}>
      <span className={`dot dot-${state}`} />
      <span className="agent-name">{agent.name}</span>
      {state === "ready" ? (
        <span className="agent-state">{label}</span>
      ) : state === "benched" ? (
        <span className="agent-state benched" title="It ran out of usage. The council carries on without it until its limit resets.">
          Out of usage · back {resetTime(agent.benched_until!)}{" "}
          <button className="link setup-link" onClick={() => onRetry(agent)} title="Ask it again now, without waiting for the reset">
            Retry
          </button>
        </span>
      ) : (
        <button className="link setup-link" onClick={() => onSetup(agent)} title={agent.hint}>
          {state === "missing" ? "Install" : "Sign in"}
        </button>
      )}
      {check && (
        <span className={`check ${check === "running" ? "check-running" : check.ok ? "check-ok" : "check-bad"}`} title={check === "running" ? "" : check.detail}>
          {check === "running"
            ? "testing…"
            : check.ok
              ? `✓ answered in ${(check.ms / 1000).toFixed(1)}s`
              : `✗ ${check.detail.split("\n")[0].slice(0, 90)}`}
        </span>
      )}
      <UsageMeter agent={agent} forecast={agent.id === "gpt" ? forecast : null} />
      <span className="agent-kind">
        {agent.tool}
        {agent.access !== "chat" && <span className="access-tag"> · {ACCESS_LEVELS.find((l) => l.id === agent.access)?.name}</span>}
      </span>
    </div>
  );
}

function folderName(path: string) {
  return path.split(/[\\/]/).filter(Boolean).pop() ?? path;
}

export default function App() {
  const [prefs, setPrefs] = useState<Prefs>(loadPrefs);
  const [agents, setAgents] = useState<AgentInfo[]>([]);
  const [conversations, setConversations] = useState<Conversation[]>([]);
  const [current, setCurrent] = useState<string | null>(null);
  const [events, setEvents] = useState<ChatEvent[]>([]);
  const [turn, setTurn] = useState<TurnState | null>(null);
  const [draft, setDraft] = useState(IN_APP ? "" : demoDraft);
  /** Files waiting to go with the next message. */
  const [pending, setPending] = useState<Attachment[]>([]);
  const [dropping, setDropping] = useState(false);
  const [targets, setTargets] = useState<string[]>(["gpt", "claude"]);
  const [discuss, setDiscuss] = useState(!IN_APP);
  const [rounds, setRounds] = useState(3);
  const [error, setError] = useState<string | null>(null);
  const [showSettings, setShowSettings] = useState(!IN_APP && demoSettings);
  const [showAddAi, setShowAddAi] = useState(!IN_APP && demoAddAi);
  const [checks, setChecks] = useState<Record<string, CheckResult | "running">>({});
  const [council, setCouncil] = useState<Council | null>(null);
  const [memoryText, setMemoryText] = useState("");
  const [memoryDirty, setMemoryDirty] = useState(false);
  const knownMembers = useRef<string[] | null>(null);
  const [update, setUpdate] = useState<UpdateInfo | null>(null);
  const [updateNote, setUpdateNote] = useState<string>("");
  const [toast, setToast] = useState<string | null>(null);
  const [agentsRefreshing, setAgentsRefreshing] = useState(false);
  const currentRef = useRef<string | null>(null);
  const agentRefreshRef = useRef<Promise<void> | null>(null);
  const usageRefreshRef = useRef(false);
  const [forecast, setForecast] = useState<ResetForecast | null>(null);
  const [cli, setCli] = useState<CliInstall | null>(null);
  const endRef = useRef<HTMLDivElement>(null);

  currentRef.current = current;

  useEffect(() => {
    document.documentElement.dataset.theme = prefs.theme;
    document.documentElement.dataset.effects = prefs.effects ? "on" : "off";
    savePrefs(prefs);
  }, [prefs]);

  const refreshAgents = useCallback(() => {
    // Focus, turn completion and the refresh button can all fire together. Share one request so
    // the provider CLIs are never probed more than once at a time, and update both snapshots as a
    // pair so access labels and council membership cannot briefly disagree.
    if (agentRefreshRef.current) return agentRefreshRef.current;
    setAgentsRefreshing(true);
    const request = Promise.all([api.agents(), api.getCouncil()])
      .then(([nextAgents, nextCouncil]) => {
        // Keep the last known usage on screen until the fresh reading arrives.
        setAgents((prev) =>
          nextAgents.map((a) => ({ ...a, usage: prev.find((p) => p.id === a.id)?.usage ?? null })),
        );
        setCouncil(nextCouncil);
        // Usage probes take several seconds; they update the meters when done without holding up
        // sign-in state or the refresh spinner.
        if (usageRefreshRef.current) return;
        usageRefreshRef.current = true;
        api
          .usage()
          .then((byId) => setAgents((prev) => prev.map((a) => ({ ...a, usage: byId[a.id] ?? null }))))
          .then(() => api.resetForecast())
          .then((f) => setForecast(f ?? null))
          .catch(() => undefined)
          .finally(() => {
            usageRefreshRef.current = false;
          });
      })
      .catch((e) => setError(`Couldn't refresh the council: ${e}`))
      .finally(() => {
        agentRefreshRef.current = null;
        setAgentsRefreshing(false);
      });
    agentRefreshRef.current = request;
    return request;
  }, []);

  const refreshList = useCallback(async () => {
    const list = await api.list();
    setConversations(list);
    return list;
  }, []);

  // Startup: agents, conversations (creating a first one), live events, update check.
  useEffect(() => {
    refreshAgents();
    refreshList().then(async (list) => {
      const first = list[0] ?? (await api.create());
      if (!list.length) await refreshList();
      setCurrent(first.id);
    });
    const un1 = onEvent((e) => {
      if (e.conversation === currentRef.current) {
        setEvents((prev) => (prev.some((p) => p.id === e.id) ? prev : [...prev, e]));
      }
      refreshList();
      // A note from Consilium (an AI ran out of usage, or is back) changes the AIs' status.
      if (e.sender === "consilium") refreshAgents();
    });
    const un2 = onTurn((t) => {
      if (t.conversation === currentRef.current) setTurn(t.state === "running" ? t : null);
      if (t.state === "idle") refreshAgents();
    });
    // Coming back to the window (for example after signing in from the Setup terminal) re-checks
    // the AIs, at most once every 10 seconds.
    let lastCheck = Date.now();
    const onFocus = () => {
      if (Date.now() - lastCheck > 10_000) {
        lastCheck = Date.now();
        refreshAgents();
      }
    };
    window.addEventListener("focus", onFocus);
    return () => {
      un1.then((f) => f());
      un2.then((f) => f());
      window.removeEventListener("focus", onFocus);
    };
  }, [refreshAgents, refreshList]);

  useEffect(() => {
    if (prefs.autoUpdate) checkForUpdate(false).then(setUpdate).catch(() => undefined);
  }, [prefs.autoUpdate]);

  useEffect(() => {
    if (!current) return;
    setTurn(null);
    api.load(current).then(setEvents).catch((e) => setError(String(e)));
    api.running().then((ids) => {
      if (ids.includes(current)) setTurn(IN_APP ? { conversation: current, state: "running", agent: null, round: 0, rounds: 0 } : demoTurn);
    });
  }, [current]);

  useEffect(() => {
    endRef.current?.scrollIntoView({ behavior: prefs.effects ? "smooth" : "auto" });
  }, [events, turn, prefs.effects]);

  const meta = conversations.find((c) => c.id === current);
  const busy = turn !== null;
  // Every AI answers at the same time, so several can be thinking at once.
  const thinkingIds = turn?.agents?.length ? turn.agents : turn?.agent ? [turn.agent] : [];
  const thinkingNames = thinkingIds.map((id) => senderName(id, agents));
  const thinking = thinkingNames.length
    ? thinkingNames.length === 1
      ? `${thinkingNames[0]} is thinking`
      : `${thinkingNames.slice(0, -1).join(", ")} and ${thinkingNames[thinkingNames.length - 1]} are thinking`
    : null;
  const readyIds = useMemo(() => new Set(agents.filter((a) => a.available).map((a) => a.id)), [agents]);
  const members = useMemo(() => agents.filter((a) => a.member), [agents]);

  // Everyone in the council is asked by default: after a restart, and when an AI joins.
  useEffect(() => {
    if (!agents.length) return;
    const ids = members.map((m) => m.id);
    const before = knownMembers.current;
    setTargets((t) => {
      if (before === null) return ids;
      const added = ids.filter((id) => !before.includes(id));
      return [...t.filter((id) => ids.includes(id)), ...added.filter((id) => !t.includes(id))];
    });
    knownMembers.current = ids;
  }, [agents, members]);
  const others = useMemo(() => agents.filter((a) => !a.member), [agents]);

  // Sending while the AIs are answering doesn't interrupt them: they read it and carry on. Stop interrupts.
  async function send() {
    if (!current || (!draft.trim() && !pending.length)) return;
    setError(null);
    const text = draft;
    const files = pending;
    setDraft("");
    setPending([]);
    try {
      const to = targets.filter((t) => members.some((m) => m.id === t));
      await api.send(current, text, to, discuss ? rounds : 1, false, files);
    } catch (e) {
      setDraft(text);
      setPending(files);
      setError(String(e));
    }
  }

  async function attachPaths(paths: string[]) {
    if (!current || !paths.length) return;
    try {
      const added = await api.attachFiles(current, paths);
      setPending((p) => [...p, ...added]);
    } catch (e) {
      setError(`Couldn't attach: ${e}`);
    }
  }

  async function attachPicked() {
    attachPaths(await api.pickFiles());
  }

  // Pasted files (a screenshot, say) are attached; pasted text goes into the message as usual.
  async function attachPasted(files: File[]) {
    if (!current) return;
    for (const f of files) {
      try {
        const data = Array.from(new Uint8Array(await f.arrayBuffer()));
        const name = f.name && f.name !== "image.png" ? f.name : `pasted-${new Date().toISOString().slice(0, 19).replace(/[:T]/g, "-")}.${f.type.split("/")[1] || "png"}`;
        const a = await api.attachData(current, name, data);
        setPending((p) => [...p, a]);
      } catch (e) {
        setError(`Couldn't attach ${f.name || "the pasted file"}: ${e}`);
      }
    }
  }

  // Files dragged onto the window are attached to the next message.
  const attachRef = useRef(attachPaths);
  attachRef.current = attachPaths;
  useEffect(() => {
    if (!IN_APP) return;
    let unlisten: (() => void) | undefined;
    getCurrentWebview()
      .onDragDropEvent((event) => {
        const p = event.payload;
        if (p.type === "over" || p.type === "enter") setDropping(true);
        else if (p.type === "leave") setDropping(false);
        else if (p.type === "drop") {
          setDropping(false);
          attachRef.current(p.paths);
        }
      })
      .then((u) => (unlisten = u))
      .catch(() => undefined);
    return () => unlisten?.();
  }, []);

  // Attachments belong to the conversation they were made in.
  useEffect(() => setPending([]), [current]);

  async function retryAgent(a: AgentInfo) {
    await api.unbench(a.id).catch(() => undefined);
    refreshAgents();
  }

  // Turning Discuss off during a discussion ends it now, not after the current round.
  function toggleDiscuss(on: boolean) {
    setDiscuss(on);
    if (!on && busy && current && (turn?.rounds ?? 0) > 1) api.stop(current);
  }

  async function newConversation() {
    const c = await api.create();
    await refreshList();
    setCurrent(c.id);
  }

  async function exportCurrent() {
    if (!current) return;
    try {
      const md = await api.exportMarkdown(current);
      await copyText(md);
      setToast("Conversation copied as Markdown");
      setTimeout(() => setToast(null), 2500);
    } catch (e) {
      setError(`Couldn't copy the conversation: ${e}`);
    }
  }

  async function deleteCurrent() {
    if (!current || !meta) return;
    if (!confirm(`Delete "${meta.title}"? This can't be undone.`)) return;
    try {
      await api.remove(current);
      const list = await refreshList();
      if (list.length) setCurrent(list[0].id);
      else newConversation();
    } catch (e) {
      setError(String(e));
    }
  }

  async function renameCurrent() {
    if (!current || !meta) return;
    const title = prompt("Rename conversation", meta.title);
    if (title && title.trim()) {
      await api.rename(current, title);
      refreshList();
    }
  }

  async function checkNow() {
    setUpdateNote("Checking…");
    const u = await checkForUpdate(true).catch(() => null);
    setUpdate(u);
    setUpdateNote(u ? `Version ${u.latest} is available.` : "You're up to date (or offline).");
  }

  const notReady = members.filter((a) => !a.available);

  // Adding an AI shares the conversations you address to it with that company, so it is the
  // person's decision, confirmed here; the AIs can only suggest it.
  async function addMember(a: AgentInfo) {
    if (!confirm(`Add ${a.name} to the council?\n\nMessages you send to ${a.name}, with the conversation so far, will go to ${a.company} through the ${a.tool} and your own ${a.company} account.`)) return;
    try {
      await api.setMember(a.id, true);
      setTargets((t) => (t.includes(a.id) ? t : [...t, a.id]));
      refreshAgents();
      setToast(`${a.name} joined the council`);
      setTimeout(() => setToast(null), 3000);
    } catch (e) {
      setError(String(e));
    }
  }

  // Bring an AI into the council: install or sign in first if needed, then ask the person to add it.
  async function bringIn(a: AgentInfo) {
    const st = agentState(a);
    if (st !== "ready") {
      try {
        const title = await api.setup(a.id, st === "missing" ? "install" : "signin");
        setShowAddAi(true);
        setToast(`${title}: finish in the terminal window, then approve adding ${a.name}`);
        setTimeout(() => setToast(null), 7000);
      } catch (e) {
        setError(String(e));
      }
      return;
    }
    await addMember(a);
  }

  // Full capacity: a project folder for every conversation, every AI Unrestricted, everyone asked,
  // Discuss on. The AIs can then run commands and install software in that folder.
  async function fullCapacity() {
    try {
      let folder = council?.default_workspace ?? null;
      if (!folder) {
        alert("Choose the folder the AIs will work in. Conversations without their own project folder use it.");
        folder = await api.pickFolder();
        if (!folder) return;
      }
      const ok = confirm(
        `Full capacity: every AI becomes Unrestricted in ${folder} and any conversation's own project folder. ` +
          "They can run any command, install software and change files there without asking you first.\n\nContinue?",
      );
      if (!ok) return;
      await api.setDefaultWorkspace(folder);
      await api.setDefaultAccess("unrestricted");
      setTargets(members.map((m) => m.id));
      setDiscuss(true);
      setRounds((r) => Math.max(r, 4));
      refreshAgents();
      setToast("Full capacity on: every AI is Unrestricted in the project folder.");
      setTimeout(() => setToast(null), 5000);
    } catch (e) {
      setError(String(e));
    }
  }

  async function toggleForecast(on: boolean) {
    try {
      await api.setResetForecast(on);
      if (!on) setForecast(null);
      refreshAgents();
    } catch (e) {
      setError(`Couldn't change the setting: ${e}`);
    }
  }

  async function installCli() {
    try {
      setCli(await api.installCli());
    } catch (e) {
      setError(`Couldn't install the terminal command: ${e}`);
    }
  }

  async function changeDefaultAccess(access: Access) {
    if (access !== "chat") {
      const level = ACCESS_LEVELS.find((l) => l.id === access)!;
      if (!confirm(`Make "${level.name}" the access for every AI (including ones added later)?\n\n${level.note}`)) return;
    }
    try {
      await api.setDefaultAccess(access);
      refreshAgents();
    } catch (e) {
      setError(String(e));
    }
  }

  async function chooseDefaultFolder(clear = false) {
    try {
      const folder = clear ? null : await api.pickFolder();
      if (!clear && !folder) return;
      await api.setDefaultWorkspace(folder);
      refreshAgents();
    } catch (e) {
      setError(String(e));
    }
  }

  async function saveMemory() {
    try {
      await api.setMemory(memoryText);
      setMemoryDirty(false);
      setToast("Council memory saved");
      setTimeout(() => setToast(null), 2500);
    } catch (e) {
      setError(String(e));
    }
  }

  // Self-check: one tiny real message to every council member at once.
  async function runSelfCheck() {
    setChecks(Object.fromEntries(members.map((m) => [m.id, "running" as const])));
    try {
      const results = await api.selfCheck();
      setChecks(Object.fromEntries(results.map((r) => [r.id, r])));
      const bad = results.filter((r) => !r.ok);
      setToast(bad.length ? `Self-check: ${bad.map((b) => b.name).join(", ")} didn't answer` : `Self-check: all ${results.length} AIs answered`);
      setTimeout(() => setToast(null), 5000);
    } catch (e) {
      setChecks({});
      setError(String(e));
    }
  }

  async function removeMember(a: AgentInfo) {
    if (!confirm(`Remove ${a.name} from the council? You can add it back any time.`)) return;
    try {
      await api.setMember(a.id, false);
      setTargets((t) => t.filter((x) => x !== a.id));
      refreshAgents();
    } catch (e) {
      setError(String(e));
    }
  }

  async function changeAccess(a: AgentInfo, access: Access) {
    if (access !== "chat") {
      const level = ACCESS_LEVELS.find((l) => l.id === access)!;
      const ok = confirm(
        `Give ${a.name} "${level.name}" access?\n\n${level.note}\n\nThis applies in conversations that have a project folder. ` +
          `${a.name} will be able to ${access === "read" ? "read" : "change"} the files there.`,
      );
      if (!ok) return;
    }
    try {
      await api.setAccess(a.id, access);
      refreshAgents();
    } catch (e) {
      setError(String(e));
    }
  }

  async function chooseFolder() {
    if (!current) return;
    try {
      const folder = await api.pickFolder();
      if (folder) {
        await api.setWorkspace(current, folder);
        refreshList();
      }
    } catch (e) {
      setError(String(e));
    }
  }

  async function clearFolder() {
    if (!current) return;
    await api.setWorkspace(current, null);
    refreshList();
  }

  async function setupAgent(a: AgentInfo) {
    try {
      const title = await api.setup(a.id, agentState(a) === "missing" ? "install" : "signin");
      setToast(`${title}: follow the steps in the terminal window, then press refresh`);
      setTimeout(() => setToast(null), 6000);
    } catch (e) {
      setError(String(e));
    }
  }

  async function setupAll() {
    for (const a of notReady) await setupAgent(a);
  }

  const toggleTarget = (id: string) =>
    setTargets((t) => (t.includes(id) ? t.filter((x) => x !== id) : [...t, id]));

  return (
    <div className="app">
      <div className="fx-scanlines" aria-hidden />
      <aside className="sidebar">
        <div className="brand">
          <img src="/icon.png" alt="" className="brand-icon" />
          <div>
            <div className="brand-name">Consilium</div>
            <div className="brand-tagline">A council of minds</div>
          </div>
        </div>
        <button className="btn primary wide" onClick={newConversation}>
          + New conversation
        </button>
        <nav className="conversations">
          {conversations.map((c) => (
            <button
              key={c.id}
              className={`conv ${c.id === current ? "active" : ""}`}
              onClick={() => setCurrent(c.id)}
              title={c.title}
            >
              {c.title}
            </button>
          ))}
        </nav>
        <div className="agents">
          <div className="section-label">
            Council
            <span>
              <button
                className="link"
                onClick={refreshAgents}
                disabled={agentsRefreshing}
                title="Refresh sign-in and plan usage for every AI"
              >
                {agentsRefreshing ? "refreshing…" : "refresh"}
              </button>
              <button
                className="link"
                onClick={runSelfCheck}
                disabled={Object.values(checks).includes("running")}
                title="Send every AI a one-word test message and show who answers (uses a little of each AI's allowance)"
              >
                self-check
              </button>
            </span>
          </div>
          {members.map((a) => (
            <AgentDot key={a.id} agent={a} onSetup={setupAgent} onRetry={retryAgent} check={checks[a.id]} forecast={forecast} />
          ))}
        </div>
        <div className="sidebar-actions">
          <button className="btn ghost" onClick={() => setShowAddAi(true)} title="Add or remove AIs">
            + Add AI
          </button>
          <button
            className="btn ghost"
            onClick={() => {
              setShowSettings(true);
              api.cliStatus().then(setCli).catch(() => undefined);
              api.getMemory().then((m) => {
                setMemoryText(m);
                setMemoryDirty(false);
              });
            }}
          >
            Settings
          </button>
        </div>
      </aside>

      <main className="main">
        {update && (
          <div className="banner">
            Consilium {update.latest} is available (you have {update.current}).
            <button className="link" onClick={() => openUrl(update.url)}>
              View release
            </button>
            <button className="link" onClick={() => setUpdate(null)}>
              Later
            </button>
          </div>
        )}
        <header className="topbar">
          <div className="title-block">
            <h1 onDoubleClick={renameCurrent} title="Double-click to rename">
              {meta?.title ?? "Consilium"}
            </h1>
            <div className="workspace">
              {meta?.workspace ? (
                <>
                  <span className="folder" title={meta.workspace}>
                    Project: <b>{folderName(meta.workspace)}</b>
                  </span>
                  <button className="link" onClick={chooseFolder}>change</button>
                  <button className="link" onClick={clearFolder}>remove</button>
                </>
              ) : council?.default_workspace ? (
                <>
                  <span className="folder" title={council.default_workspace}>
                    Project: <b>{folderName(council.default_workspace)}</b> (default)
                  </span>
                  <button className="link" onClick={chooseFolder}>use another folder</button>
                </>
              ) : (
                <button className="link" onClick={chooseFolder} title="Let the AIs work in a folder, according to each one's access">
                  + Add project folder
                </button>
              )}
            </div>
          </div>
          <div className="topbar-actions">
            <button className="btn ghost" onClick={exportCurrent} disabled={!events.length}>
              Copy as Markdown
            </button>
            <button className="btn ghost danger" onClick={deleteCurrent}>
              Delete
            </button>
          </div>
        </header>

        <section className="messages">
          {!events.length && (
            <div className="empty">
              <img src="/icon.png" alt="" />
              <h2>Convene the council</h2>
              <p>
                Ask {members.map((m) => m.name).join(" and ") || "the council"} together. Pick who answers below, or switch to{" "}
                <b>Discuss</b> to let them work it out between themselves for a few rounds. Address someone directly with{" "}
                {members.map((m) => (
                  <span key={m.id}>
                    <code>@{m.id}</code>{" "}
                  </span>
                ))}
                or <code>@all</code>.
              </p>
              {agents.length > 0 && notReady.length > 0 && (
                <div className="setup-card">
                  <b>{notReady.map((a) => a.name).join(" and ")} {notReady.length > 1 ? "need" : "needs"} setting up.</b>
                  <span className="muted small">
                    A terminal window opens with the official {notReady.length > 1 ? "commands" : "command"}; press Enter to
                    run, and sign in with your own account in the browser. Consilium never sees your password.
                  </span>
                  <button className="btn primary" onClick={setupAll}>
                    Set up AIs
                  </button>
                </div>
              )}
            </div>
          )}
          {events.map((e) =>
            e.kind === "message" ? (
              <article key={e.id} className={`msg from-${e.sender}`}>
                <div className="msg-head">
                  <span className="who">{senderName(e.sender, agents)}</span>
                  {e.to.length > 0 && e.sender === "human" && (
                    <span className="to">→ {e.to.map((t) => senderName(t, agents)).join(", ")}</span>
                  )}
                  <span className="when">{time(e.ts)}</span>
                </div>
                {e.attachments && e.attachments.length > 0 && (
                  <div className="attachments">
                    {e.attachments.map((a) => (
                      <button key={a.path} className="attachment" title={`Open ${a.name}`} onClick={() => current && api.openAttachment(current, a).catch((err) => setError(String(err)))}>
                        {attachmentIcon(a.kind)} {a.name} <small>{fileSize(a.size)}</small>
                      </button>
                    ))}
                  </div>
                )}
                <MessageBody text={e.text} />
                {e.add_ai?.filter((id) => agents.some((a) => a.id === id && !a.member)).map((id) => {
                  const a = agents.find((x) => x.id === id)!;
                  return (
                    <div key={id} className="addai-suggest">
                      {senderName(e.sender, agents)} suggests adding <b>{a.name}</b> to the council.
                      <button className="btn primary small-btn" onClick={() => bringIn(a)}>
                        {agentState(a) === "ready" ? `Add ${a.name}` : agentState(a) === "missing" ? `Install ${a.name}` : `Sign in to ${a.name}`}
                      </button>
                    </div>
                  );
                })}
              </article>
            ) : (
              <div key={e.id} className={`note note-${e.kind}`}>
                {e.text}
              </div>
            ),
          )}
          {busy && (
            <div className="thinking">
              <span className="pulse" /> {thinking ?? "Working"}
              {turn && turn.rounds > 1 ? ` · round ${turn.round} of ${turn.rounds}` : ""}
              <span className="cursor">▍</span>
            </div>
          )}
          <div ref={endRef} />
        </section>

        <footer className={`composer ${dropping ? "dropping" : ""}`}>
          {error && (
            <div className="error" onClick={() => setError(null)}>
              {error}
            </div>
          )}
          <div className="composer-controls">
            <span className="section-label">Ask</span>
            {members.map((a) => (
              <label key={a.id} className={`chip chip-${a.id} ${targets.includes(a.id) ? "on" : ""}`}>
                <input type="checkbox" checked={targets.includes(a.id)} onChange={() => toggleTarget(a.id)} />
                {a.name}
                {!readyIds.has(a.id) && <span className="chip-warn" title={a.hint}>!</span>}
              </label>
            ))}
            <span className="spacer" />
            <label className={`chip ${discuss ? "on" : ""}`} title="Let the AIs respond to each other for several rounds">
              <input type="checkbox" checked={discuss} onChange={(e) => toggleDiscuss(e.target.checked)} />
              Discuss
            </label>
            {discuss && (
              <label className="rounds">
                rounds
                <input
                  type="number"
                  min={2}
                  max={8}
                  value={rounds}
                  onChange={(e) => setRounds(Math.max(2, Math.min(8, Number(e.target.value) || 2)))}
                />
              </label>
            )}
          </div>
          {pending.length > 0 && (
            <div className="attachments pending">
              {pending.map((a) => (
                <span key={a.path} className="attachment" title={a.kind === "text" ? "The AIs read its text" : a.kind === "image" || a.kind === "pdf" ? "AIs that can open it look at it" : "AIs with access can open it"}>
                  {attachmentIcon(a.kind)} {a.name} <small>{fileSize(a.size)}</small>
                  <button className="link" onClick={() => setPending((p) => p.filter((x) => x.path !== a.path))} title="Remove" aria-label={`Remove ${a.name}`}>
                    ×
                  </button>
                </span>
              ))}
            </div>
          )}
          <div className="composer-row">
            <textarea
              value={draft}
              onPaste={(e) => {
                const files = Array.from(e.clipboardData.files);
                if (files.length) {
                  if (!e.clipboardData.getData("text")) e.preventDefault();
                  attachPasted(files);
                }
              }}
              placeholder="Message the council…  (Enter to send, Shift+Enter for a new line; drop or paste files to attach)"
              onChange={(e) => setDraft(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.shiftKey) {
                  e.preventDefault();
                  send();
                }
              }}
              rows={3}
            />
            <div className="send-buttons">
              <button className="btn ghost" onClick={attachPicked} title="Attach files: documents, Word files, pictures, PDFs, code" aria-label="Attach files">
                📎
              </button>
              {busy && (
                <button className="btn stop" onClick={() => current && api.stop(current)} title="Stop the AIs now">
                  Stop
                </button>
              )}
              <button
                className="btn primary"
                onClick={send}
                disabled={!draft.trim() && !pending.length}
                title={busy ? "Add this to the discussion; the AIs read it without being interrupted (Enter)" : "Send (Enter)"}
              >
                Send
              </button>
            </div>
          </div>
        </footer>
      </main>

      {showSettings && (
        <div className="modal-backdrop" onClick={() => setShowSettings(false)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <h2>Settings</h2>
            <div className="section-label">Theme</div>
            <div className="themes">
              {THEMES.map((t) => (
                <button
                  key={t.id}
                  className={`theme-card ${prefs.theme === t.id ? "on" : ""}`}
                  data-preview={t.id}
                  onClick={() => setPrefs({ ...prefs, theme: t.id })}
                >
                  <span className="swatch" />
                  <b>{t.name}</b>
                  <small>{t.note}</small>
                </button>
              ))}
            </div>
            <label className="toggle">
              <input type="checkbox" checked={prefs.effects} onChange={(e) => setPrefs({ ...prefs, effects: e.target.checked })} />
              Visual effects (scanlines, glow and animation in Retro '84 and CRT)
            </label>
            <div className="section-label">AI access</div>
            <p className="muted small">
              Applies in the conversation's project folder, or the default folder below. Without either, every AI is
              chat-only.
            </p>
            <label className="access-row">
              <span className="agent-name">Every AI</span>
              <select value={council?.default_access ?? "chat"} onChange={(e) => changeDefaultAccess(e.target.value as Access)}>
                {ACCESS_LEVELS.map((l) => (
                  <option key={l.id} value={l.id}>
                    {l.name}
                  </option>
                ))}
              </select>
              <small className="muted">The standard for all AIs, including ones added later. Below, set one AI differently.</small>
            </label>
            <div className="row">
              <span className="muted small">Default project folder:</span>
              <b className="small">{council?.default_workspace ?? "none"}</b>
              <button className="link" onClick={() => chooseDefaultFolder()}>
                choose
              </button>
              {council?.default_workspace && (
                <button className="link" onClick={() => chooseDefaultFolder(true)}>
                  clear
                </button>
              )}
            </div>
            <div className="access-list">
              {members.map((a) => (
                <label key={a.id} className={`access-row agent-${a.id}`}>
                  <span className="agent-name">{a.name}</span>
                  <select value={a.access} onChange={(e) => changeAccess(a, e.target.value as Access)}>
                    {ACCESS_LEVELS.map((l) => (
                      <option key={l.id} value={l.id}>
                        {l.name}
                      </option>
                    ))}
                  </select>
                  <small className="muted">{ACCESS_LEVELS.find((l) => l.id === a.access)?.note}</small>
                </label>
              ))}
            </div>
            <div className="row">
              <button className="btn ghost" onClick={fullCapacity}>
                Full capacity
              </button>
              <span className="muted small">Every AI becomes Unrestricted in the project folder, everyone is asked, and Discuss is on.</span>
            </div>
            <div className="section-label">Council memory</div>
            <p className="muted small">
              Notes every AI receives in every conversation. They add to it with a REMEMBER: line; you can edit or clear it
              here. A project's own shared notes go in a CONSILIUM.md file in its folder.
            </p>
            <textarea
              className="memory-editor"
              value={memoryText}
              onChange={(e) => {
                setMemoryText(e.target.value);
                setMemoryDirty(true);
              }}
              rows={6}
              placeholder="- The human prefers short answers&#10;- Builds use npm run app:build"
            />
            <div className="row">
              <button className="btn ghost" onClick={saveMemory} disabled={!memoryDirty}>
                Save memory
              </button>
            </div>
            <div className="section-label">Usage</div>
            <p className="muted small">
              Each AI's card shows its current model's allowance left, with its whole plan underneath, and when they reset.
              Every AI is told where everyone stands, so they know when the others will be back.
            </p>
            <label className="toggle">
              <input type="checkbox" checked={council?.reset_forecast ?? false} onChange={(e) => toggleForecast(e.target.checked)} />
              Forecast the next global Codex reset (unofficial, from{" "}
              <button className="link" onClick={() => openUrl(FORECAST_SITE)}>
                codex-reset.com
              </button>
              ; asks that site every 15 minutes at most)
            </label>
            <div className="section-label">Terminal</div>
            <p className="muted small">
              Talk to the council from a terminal too, in the same conversations: <code>consilium chat</code>, or{" "}
              <code>consilium ask "…"</code> for one question. The app and the terminal can be open at the same time;
              a message sent in one joins the AIs' turn in the other.
            </p>
            <div className="row">
              <button className="btn ghost" onClick={installCli}>
                {cli?.installed ? "Reinstall terminal command" : "Install terminal command"}
              </button>
              <span className="muted small">{cli ? (cli.installed ? `Installed: ${cli.path}` : "Not installed yet") : ""}</span>
            </div>
            {cli?.note && <pre className="cli-note small">{cli.note}</pre>}
            <div className="section-label">Updates</div>
            <label className="toggle">
              <input
                type="checkbox"
                checked={prefs.autoUpdate}
                onChange={(e) => setPrefs({ ...prefs, autoUpdate: e.target.checked })}
              />
              Check GitHub for a new version once a day
            </label>
            <div className="row">
              <button className="btn ghost" onClick={checkNow}>
                Check now
              </button>
              <button className="link" onClick={() => openUrl(RELEASES_URL)}>
                All releases
              </button>
              <span className="muted">{updateNote}</span>
            </div>
            <p className="muted small">
              Updates are never installed automatically. Consilium only tells you when a new release exists.
              Your conversations stay on this computer. Each AI is reached through its company's own
              command-line tool with your existing sign-in, and Consilium never sees a password or key.
            </p>
            <div className="row end">
              <button className="btn primary" onClick={() => setShowSettings(false)}>
                Done
              </button>
            </div>
          </div>
        </div>
      )}
      {showAddAi && (
        <div className="modal-backdrop" onClick={() => setShowAddAi(false)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <h2>Add AI</h2>
            <p className="muted small">
              Each AI joins through its company's official command-line tool and your own account: no API keys.
              You decide who joins; an AI in the conversation can suggest one, but only you can add it.
            </p>
            {others.length === 0 && <p className="muted">Every available AI is already in the council.</p>}
            {others.map((a) => {
              const st = agentState(a);
              return (
                <div key={a.id} className={`addai-card agent-${a.id}`}>
                  <div className="addai-head">
                    <span className={`dot dot-${st}`} />
                    <span className="agent-name">{a.name}</span>
                    <span className="muted small">
                      {a.company} · {a.tool}
                      {a.version ? ` · ${a.version}` : ""}
                    </span>
                  </div>
                  <ol className="addai-steps">
                    <li className={st !== "missing" ? "done" : ""}>
                      Install the {a.tool}
                      {st === "missing" && (
                        <button className="btn ghost small-btn" onClick={() => setupAgent(a)}>
                          Install
                        </button>
                      )}
                    </li>
                    <li className={st === "ready" ? "done" : ""}>
                      Sign in with your {a.company} account
                      {st === "signin" && (
                        <button className="btn ghost small-btn" onClick={() => setupAgent(a)}>
                          Sign in
                        </button>
                      )}
                    </li>
                    <li>
                      Add to the council
                      <button className="btn primary small-btn" disabled={st !== "ready"} onClick={() => addMember(a)}>
                        Add {a.name}
                      </button>
                    </li>
                  </ol>
                  {st !== "ready" && <p className="muted small">{a.hint} When you're done, come back here; it checks again automatically.</p>}
                </div>
              );
            })}
            <div className="section-label">In the council</div>
            {members.map((a) => (
              <div key={a.id} className="member-row">
                <span className={`agent-name agent-${a.id}`}>{a.name}</span>
                <span className="muted small">{a.company}</span>
                <span className="spacer" />
                <button className="link danger-link" disabled={members.length < 2} onClick={() => removeMember(a)}>
                  remove
                </button>
              </div>
            ))}
            <div className="row end">
              <button className="btn ghost" onClick={refreshAgents} disabled={agentsRefreshing}>
                {agentsRefreshing ? "Checking…" : "Check again"}
              </button>
              <button className="btn primary" onClick={() => setShowAddAi(false)}>
                Done
              </button>
            </div>
          </div>
        </div>
      )}
      {toast && <div className="toast">{toast}</div>}
    </div>
  );
}
