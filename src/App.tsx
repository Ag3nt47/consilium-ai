import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import {
  ACCESS_LEVELS,
  api,
  FORECAST_SITE,
  IN_APP,
  onActivity,
  onEvent,
  onTurn,
  senderName,
  type Access,
  type AgentActivity,
  type AgentInfo,
  type Attachment,
  type CheckResult,
  type CliInstall,
  type Council,
  type ChatEvent,
  type Conversation,
  type ResetForecast,
  type SystemPressure,
  type StagedUpdate,
  type TokenUsageReport,
  type TurnState,
  type UsageWindow,
} from "./api";
import { canListen, canSpeak, Listener, Speaker, voiceOn } from "./voice";
import { checkForUpdate, RELEASES_URL, type UpdateInfo } from "./updates";
import { installHoverTips } from "./tooltip";
import { demoAddAi, demoDraft, demoSettings, demoTheme, demoTurn } from "./demo";

const THEMES = [
  { id: "consilium", name: "Consilium", note: "Indigo, ivory and gold" },
  { id: "glass", name: "visionOS Glass", note: "Frosted translucent glass, ambient glow" },
  { id: "cyber", name: "Cyber", note: "From the UI Design Bundle" },
  { id: "slate", name: "Slate", note: "From the UI Design Bundle" },
  { id: "crt", name: "CRT", note: "Green phosphor terminal" },
  { id: "retro84", name: "Retro '84", note: "Neon grid, scanlines, glow" },
] as const;

/** Official account/key and plan portals used by the approval-gated Add AI wizard. */
const PROVIDER_PORTALS: Record<string, { account: string; accountLabel: string; plan?: string }> = {
  gpt: { account: "https://chatgpt.com/", accountLabel: "Open ChatGPT", plan: "https://chatgpt.com/#pricing" },
  claude: { account: "https://claude.ai/", accountLabel: "Open Claude", plan: "https://claude.ai/upgrade" },
  gemini: {
    account: "https://antigravity.google/pricing?app=cli",
    accountLabel: "Open Antigravity plans",
    plan: "https://antigravity.google/pricing?app=cli",
  },
  grok: { account: "https://console.x.ai/", accountLabel: "Open xAI console", plan: "https://console.x.ai/" },
  deepseek: {
    account: "https://platform.deepseek.com/api_keys",
    accountLabel: "Open DeepSeek API keys",
    plan: "https://platform.deepseek.com/top_up",
  },
};

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

function formatTokens(n: number): string {
  if (n >= 1_000_000) return (n / 1_000_000).toFixed(1).replace(/\.0$/, "") + "M";
  if (n >= 1_000) return (n / 1_000).toFixed(1).replace(/\.0$/, "") + "k";
  return n.toLocaleString();
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

/** The human's answer to a request, as sent by the Approve and Deny buttons. */
function decisionText(request: string, approved: boolean): string {
  return `${approved ? "Approved" : "Denied"}: ${request.trim()}`;
}

function requestKey(eventId: string, line: number): string {
  return `${eventId}:${line}`;
}

/**
 * Match each human button answer to the request cards that existed when it was sent.
 * Keeping event-specific keys means a later request with the same wording still gets fresh buttons.
 */
function answeredRequests(events: ChatEvent[]): Map<string, boolean> {
  const out = new Map<string, boolean>();
  const waiting = new Map<string, string[]>();
  for (const e of events) {
    if (e.kind !== "message") continue;
    if (e.sender !== "human") {
      for (const [line, text] of e.text.split("\n").entries()) {
        const request = /^\s*REQUEST:\s*(.+)$/i.exec(text)?.[1].trim();
        if (!request) continue;
        waiting.set(request, [...(waiting.get(request) ?? []), requestKey(e.id, line)]);
      }
      continue;
    }
    const answer = /^(Approved|Denied):\s*([\s\S]+)$/.exec(e.text.trim());
    if (!answer) continue;
    for (const key of waiting.get(answer[2].trim()) ?? []) out.set(key, answer[1] === "Approved");
    waiting.delete(answer[2].trim());
  }
  return out;
}

type Decide = (key: string, request: string, requester: string, approved: boolean) => void;

/** Message text with REQUEST: lines shown as red request callouts, with Approve/Deny buttons when `decide` is given. */
function MessageBody({ event, answered, deciding, decide }: { event: ChatEvent; answered?: Map<string, boolean>; deciding?: Set<string>; decide?: Decide }) {
  const text = event.text;
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
          const key = requestKey(event.id, i);
          return (
            <div key={i} className="request">
              <span className="request-label">REQUEST:</span> <Inline text={m[1]} />
              {decide && (
                <RequestActions
                  answer={answered?.get(key)}
                  pending={deciding?.has(key)}
                  onDecide={(ok) => decide(key, m[1], event.sender, ok)}
                />
              )}
            </div>
          );
        }
        return <div key={i} className={line.trim() ? "line" : "gap"}><Inline text={line} /></div>;
      })}
    </div>
  );
}

function RequestActions({ answer, pending, onDecide }: { answer?: boolean; pending?: boolean; onDecide: (approved: boolean) => void }) {
  if (answer !== undefined) {
    return <span className={`request-answer ${answer ? "approved" : "denied"}`} aria-live="polite">{answer ? "✓ Approved" : "✕ Denied"}</span>;
  }
  return (
    <span className="request-actions">
      <button title="Say yes: posts “Approved: …” to the council so the AI that asked can go ahead" type="button" className="btn small-btn approve" disabled={pending} onClick={() => onDecide(true)} aria-label="Approve this request">
        {pending ? "Sending…" : "Approve"}
      </button>
      <button title="Say no: posts “Denied: …” to the council so the AI that asked won't do it" type="button" className="btn small-btn deny" disabled={pending} onClick={() => onDecide(false)} aria-label="Deny this request">
        Deny
      </button>
    </span>
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
/** Password field for an API-key AI. The key goes straight to the backend and is never shown again. */
function ApiKeyField({ agent, onSaved, onError }: { agent: AgentInfo; onSaved: () => void; onError: (e: string) => void }) {
  const [key, setKey] = useState("");
  const [busy, setBusy] = useState(false);
  async function save(value: string) {
    setBusy(true);
    try {
      await api.setApiKey(agent.id, value);
      setKey("");
      onSaved();
    } catch (e) {
      onError(String(e));
    } finally {
      setBusy(false);
    }
  }
  return (
    <span className="apikey-field">
      <input
        type="password"
        autoComplete="off"
        spellCheck={false}
        placeholder={agent.path ? "Replace key…" : "API key"}
        value={key}
        onChange={(e) => setKey(e.target.value)}
        onKeyDown={(e) => e.key === "Enter" && key.trim() && save(key)}
      />
      <button title="Check this API key with the provider and save it on this Mac only" className="btn ghost small-btn" disabled={busy || !key.trim()} onClick={() => save(key)}>
        {busy ? "Checking…" : "Save"}
      </button>
      {agent.path && (
        <button title="Delete the saved API key for this AI" className="link danger-link" disabled={busy} onClick={() => save("")}>
          remove key
        </button>
      )}
    </span>
  );
}

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
    <div className="usage-meter" title="How much of this AI's plan allowance is left, and when each limit resets">
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
          <button title="Open codex-reset.com, the unofficial source of the reset forecast" className="link" onClick={() => openUrl(FORECAST_SITE)}>
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

function AgentDot({
  agent,
  onSetup,
  onRetry,
  check,
  forecast,
  isWorking,
  isFrozen,
  activity,
  load,
}: {
  agent: AgentInfo;
  onSetup: (a: AgentInfo) => void;
  onRetry: (a: AgentInfo) => void;
  check?: CheckResult | "running";
  forecast?: ResetForecast | null;
  isWorking?: boolean;
  isFrozen?: boolean;
  activity?: AgentActivity;
  /** Share of the whole PC (0-100) this AI's processes are using right now; undefined when none run. */
  load?: number;
}) {
  const state = agentState(agent);
  const dotClass = isFrozen
    ? "dot-frozen"
    : isWorking
    ? "dot-working"
    : `dot-${state}`;
  const label = isFrozen
    ? "Stalled / frozen"
    : isWorking
    ? "Working…"
    : { ready: "Ready", signin: "Not signed in", missing: "Not installed", benched: "Out of usage" }[state];
  return (
    <div
      className={`agent agent-${agent.id}`}
      title={
        isFrozen
          ? `Stalled: no provider activity for 25 seconds${activity?.detail ? `\nLast activity: ${activity.label} · ${activity.detail}` : ""}`
          : isWorking && activity
            ? `${activity.label}${activity.detail ? `\n${activity.detail}` : ""}\nObservable provider activity; private reasoning is not exposed.`
            : isWorking
              ? "Generating response…"
              : agent.hint || `${agent.tool} ${agent.version ?? ""}`
      }
    >
      <span className={`dot ${dotClass}`} />
      <span className="agent-name">{agent.name}</span>
      {load !== undefined && (
        <span
          className="agent-load"
          style={{ color: pressureColor(load) }}
          title={`${agent.name}'s processes are using ${load}% of this PC (the busier of its CPU and memory share)`}
        >
          ▣ {load}%
        </span>
      )}
      {isFrozen ? (
        <span className="agent-state" style={{ color: "var(--danger)" }} title="No output for a while; it may be stuck. Stop and resend if it doesn't recover.">Stalled</span>
      ) : isWorking ? (
        <span className="agent-state" style={{ color: "var(--accent)" }}>{activity?.label ?? "Working…"}</span>
      ) : state === "ready" ? (
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
      {isWorking && activity && (
        <span className={`agent-activity activity-${activity.kind}`}>
          <span className="activity-icon" aria-hidden="true">{activityIcon(activity.kind)}</span>
          <span className="activity-detail">{activity.detail || activity.label}</span>
          <span className="activity-time">{elapsedActivity(activity.started_at)}</span>
        </span>
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

function activityIcon(kind: string): string {
  return {
    preparing: "◌",
    planning: "◇",
    thinking: "✦",
    reading: "◫",
    researching: "⌕",
    coding: "⌘",
    running: "›_",
    tool: "⚙",
    delegating: "⇄",
    reporting: "✎",
  }[kind] ?? "•";
}

function elapsedActivity(startedAt: number): string {
  const seconds = Math.max(0, Math.floor((Date.now() - startedAt) / 1000));
  if (seconds < 60) return `${seconds}s`;
  return `${Math.floor(seconds / 60)}m ${seconds % 60}s`;
}

/** Green at 0%, through yellow, to red at 100%. */
function pressureColor(percent: number): string {
  const pct = Math.min(100, Math.max(0, percent));
  return `hsl(${Math.round(120 - (pct / 100) * 120)}, 85%, 50%)`;
}

function ResourcePressureGauge({ pressure }: { pressure: SystemPressure | null }) {
  if (!pressure) return null;
  const pct = Math.min(100, Math.max(0, pressure.percent));
  const color = pressureColor(pct);
  return (
    <div
      className="pc-pressure"
      title={`Host System Pressure: ${pct}%\nCPU: ${pressure.cpu_percent}% (${pressure.cores} cores)\nMemory: ${pressure.memory_percent}%\nStatus: ${pressure.label}`}
    >
      <div className="pc-pressure-header">
        <span className="pc-pressure-title">PC Pressure</span>
        <span className="pc-pressure-val" style={{ color }}>{pct}%</span>
      </div>
      <div className="pc-pressure-track">
        <div
          className="pc-pressure-bar"
          style={{
            width: `${pct}%`,
            backgroundColor: color,
            boxShadow: `0 0 8px ${color}`,
          }}
        />
      </div>
      <div className="pc-pressure-sub muted small">
        <span>CPU {pressure.cpu_percent}%</span>
        <span>RAM {pressure.memory_percent}%</span>
        <span className="pc-pressure-status" style={{ color }}>{pressure.label}</span>
      </div>
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
  const [activities, setActivities] = useState<Record<string, AgentActivity>>({});
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
  const [showUsage, setShowUsage] = useState(false);
  const [showPalette, setShowPalette] = useState(false);
  // Voice chat: whether replies are being read aloud right now, and what the recogniser is hearing.
  const [speaking, setSpeaking] = useState(false);
  const [heard, setHeard] = useState("");
  const voiceRef = useRef(false);
  const agentsRef = useRef<AgentInfo[]>([]);
  const speakerRef = useRef<Speaker | null>(null);
  if (!speakerRef.current) speakerRef.current = new Speaker(setSpeaking);
  const sendTextRef = useRef<(text: string) => void>(() => undefined);
  const [paletteQuery, setPaletteQuery] = useState("");
  const [paletteIndex, setPaletteIndex] = useState(0);
  const [turnStartTime, setTurnStartTime] = useState<number | null>(null);
  const [tokenReport, setTokenReport] = useState<TokenUsageReport | null>(null);
  const [usageLoading, setUsageLoading] = useState(false);
  const [checks, setChecks] = useState<Record<string, CheckResult | "running">>({});
  const [council, setCouncil] = useState<Council | null>(null);
  const [memoryText, setMemoryText] = useState("");
  const [memoryDirty, setMemoryDirty] = useState(false);
  const knownMembers = useRef<string[] | null>(null);
  const [update, setUpdate] = useState<UpdateInfo | null>(null);
  const [updateNote, setUpdateNote] = useState<string>("");
  const [toast, setToast] = useState<string | null>(null);
  const [agentsRefreshing, setAgentsRefreshing] = useState(false);
  const [decidingRequests, setDecidingRequests] = useState<Set<string>>(() => new Set());
  const currentRef = useRef<string | null>(null);
  const agentRefreshRef = useRef<Promise<void> | null>(null);
  const usageRefreshRef = useRef(false);
  const [forecast, setForecast] = useState<ResetForecast | null>(null);
  const [pressure, setPressure] = useState<SystemPressure | null>(null);
  const [staged, setStaged] = useState<StagedUpdate | null>(null);
  const [updating, setUpdating] = useState(false);
  const [requestingStatus, setRequestingStatus] = useState(false);

  // Quick hover explanations for every control that has a title.
  useEffect(() => installHoverTips(), []);

  // A staged build is a file check, so polling it is cheap.
  useEffect(() => {
    const poll = () => api.updateAvailable().then(setStaged).catch(() => undefined);
    poll();
    const t = setInterval(poll, 15000);
    return () => clearInterval(t);
  }, []);

  const updateNow = async () => {
    setUpdating(true);
    try {
      await api.updateNow();
    } catch (e) {
      setUpdating(false);
      alert(`Update failed: ${e}`);
    }
  };
  const [cli, setCli] = useState<CliInstall | null>(null);
  const endRef = useRef<HTMLDivElement>(null);

  currentRef.current = current;

  useEffect(() => {
    let mounted = true;
    const poll = async () => {
      try {
        const p = await api.systemPressure();
        if (mounted) setPressure(p);
      } catch {
        // ignore
      }
    };
    poll();
    const interval = setInterval(poll, 2000);
    return () => {
      mounted = false;
      clearInterval(interval);
    };
  }, []);

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

  const loadTokenUsage = useCallback(async (convId?: string | null) => {
    const id = convId ?? currentRef.current;
    if (!id) {
      setTokenReport(null);
      return;
    }
    setUsageLoading(true);
    try {
      const dayStart = new Date().setHours(0, 0, 0, 0);
      const report = await api.tokenUsage(id, dayStart);
      setTokenReport(report);
    } catch {
      /* ignore */
    } finally {
      setUsageLoading(false);
    }
  }, []);

  const openUsage = useCallback(() => {
    setShowUsage(true);
    loadTokenUsage();
  }, [loadTokenUsage]);

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
        // In voice chat, AI replies are read aloud as they arrive.
        if (voiceRef.current && e.kind === "message" && e.sender !== "human") {
          speakerRef.current?.say(e.sender, senderName(e.sender, agentsRef.current), e.text);
        }
      }
      refreshList();
      // A note from Consilium (an AI ran out of usage, or is back) changes the AIs' status.
      if (e.sender === "consilium") refreshAgents();
    });
    const un2 = onTurn((t) => {
      if (t.conversation === currentRef.current) {
        setTurn(t.state === "running" ? t : null);
        if (t.state === "running") {
          setTurnStartTime((prev) => prev ?? Date.now());
        } else {
          setTurnStartTime(null);
          setActivities({});
        }
      }
      if (t.state === "idle") {
        refreshAgents();
        loadTokenUsage();
      }
    });
    const un3 = onActivity((activity) => {
      if (activity.conversation !== currentRef.current) return;
      setActivities((previous) => ({ ...previous, [activity.agent]: activity }));
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
      un3.then((f) => f());
      window.removeEventListener("focus", onFocus);
    };
  }, [refreshAgents, refreshList]);

  useEffect(() => {
    if (prefs.autoUpdate) checkForUpdate(false).then(setUpdate).catch(() => undefined);
  }, [prefs.autoUpdate]);

  useEffect(() => {
    if (!current) return;
    setTurn(null);
    setActivities({});
    api.load(current).then(setEvents).catch((e) => setError(String(e)));
    api.running().then((ids) => {
      if (ids.includes(current)) setTurn(IN_APP ? { conversation: current, state: "running", agent: null, round: 0, rounds: 0 } : demoTurn);
    });
  }, [current]);

  useEffect(() => {
    endRef.current?.scrollIntoView({ behavior: prefs.effects ? "smooth" : "auto" });
  }, [events, turn, prefs.effects]);

  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setShowPalette((prev) => {
          if (!prev) {
            setPaletteQuery("");
            setPaletteIndex(0);
          }
          return !prev;
        });
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);

  useEffect(() => {
    if (showUsage) loadTokenUsage(current);
  }, [showUsage, current, loadTokenUsage]);

  const meta = conversations.find((c) => c.id === current);
  const busy = turn !== null;
  // Every AI answers at the same time, so several can be thinking at once.
  const thinkingIds = turn?.agents?.length ? turn.agents : turn?.agent ? [turn.agent] : [];
  const thinkingNames = thinkingIds.map((id) => senderName(id, agents));
  const nativeActivity = thinkingIds
    .map((id) => {
      const activity = activities[id];
      if (!activity) return null;
      const name = senderName(id, agents);
      return `${name}: ${activity.label}${activity.detail ? ` · ${activity.detail}` : ""}`;
    })
    .filter((line): line is string => Boolean(line));
  const thinking = nativeActivity.length
    ? nativeActivity.join("  •  ")
    : thinkingNames.length
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

  // Ask every council member, even ones not selected in the composer. During an active turn this
  // joins the work without interrupting it; while idle it starts one short parallel status turn.
  async function requestStatus() {
    if (!current || !members.length || requestingStatus) return;
    setRequestingStatus(true);
    setError(null);
    try {
      await api.send(
        current,
        "Status request: Each AI, report only your own work now. Include what is done, what is in progress, any blocker, uncommitted changes, and your next step. Be concise and do not duplicate another AI's report.",
        members.map((member) => member.id),
        1,
      );
    } catch (e) {
      setError(`Couldn't request status: ${e}`);
    } finally {
      setRequestingStatus(false);
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

  // A spoken message is sent as soon as the human pauses.
  sendTextRef.current = (text: string) => {
    if (!current) return;
    setHeard("");
    const to = targets.filter((t) => members.some((m) => m.id === t));
    api.send(current, text, to, discuss ? rounds : 1).catch((e) => setError(String(e)));
  };

  const voice = useMemo(() => voiceOn(events), [events]);
  voiceRef.current = voice;
  agentsRef.current = agents;

  async function switchVoice(on: boolean) {
    if (!current) return;
    setError(null);
    if (!on) speakerRef.current?.stop();
    try {
      await api.setVoice(current, on);
    } catch (e) {
      setError(String(e));
    }
  }

  // Listen while voice chat is on, except while a reply is being read aloud (so the AIs don't hear themselves).
  useEffect(() => {
    if (!voice || speaking || !canListen) return;
    const l = new Listener(setHeard, (t) => sendTextRef.current(t), (m) => setError(m));
    l.start();
    return () => {
      l.stop();
      setHeard("");
    };
  }, [voice, speaking, current]);

  // Leaving voice chat or the conversation stops any speech.
  useEffect(() => {
    if (!voice) speakerRef.current?.stop();
  }, [voice]);
  useEffect(() => () => speakerRef.current?.stop(), [current]);

  // Approve/Deny on a REQUEST callout sends the decision as the human's message, quoting the request.
  async function decide(key: string, request: string, requester: string, approved: boolean) {
    if (!current) return;
    setError(null);
    setDecidingRequests((pending) => new Set(pending).add(key));
    try {
      const selected = targets.filter((t) => members.some((m) => m.id === t));
      const to = members.some((m) => m.id === requester) ? [requester] : selected;
      await api.send(current, decisionText(request, approved), to, discuss ? rounds : 1);
    } catch (e) {
      setError(String(e));
    } finally {
      setDecidingRequests((pending) => {
        const next = new Set(pending);
        next.delete(key);
        return next;
      });
    }
  }
  const answered = useMemo(() => answeredRequests(events), [events]);

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
    if (a.transport === "api" && st !== "ready") {
      setShowAddAi(true);
      const portal = PROVIDER_PORTALS[a.id];
      if (portal) await openUrl(portal.account).catch((e) => setError(`Couldn't open ${a.company}: ${e}`));
      setToast(`Finish account, billing, and API-key setup with ${a.company}; then paste only the key in Add AI`);
      setTimeout(() => setToast(null), 8000);
      return;
    }
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

  const paletteCommands = useMemo(() => [
    { id: "new-conv", name: "New Conversation", hint: "Start a fresh topic", run: () => { newConversation(); } },
    { id: "status", name: "Request Council Status", hint: "Ask every AI for its current work and next step", run: () => { requestStatus(); } },
    { id: "usage", name: "Show Token Usage", hint: "Conversation & daily totals", run: () => openUsage() },
    { id: "self-check", name: "Run Self-Check", hint: "Probe all active council models", run: () => runSelfCheck() },
    { id: "manage-ais", name: "Add / Remove AIs", hint: "Council membership", run: () => setShowAddAi(true) },
    { id: "settings", name: "Open Settings", hint: "Theme & access preferences", run: () => {
        setShowSettings(true);
        api.getMemory().then((m) => {
          setMemoryText(m);
          setMemoryDirty(false);
        });
      }
    },
    { id: "full-capacity", name: "Full Capacity Mode", hint: "Enable unrestricted file & shell access", run: () => fullCapacity() },
    { id: "discuss", name: discuss ? "Disable Discuss Mode" : "Enable Discuss Mode", hint: "Multi-round debate", run: () => toggleDiscuss(!discuss) },
    { id: "theme-glass", name: "Theme: visionOS Glass", hint: "Frosted translucent glass", run: () => setPrefs((p) => ({ ...p, theme: "glass" })) },
    { id: "theme-consilium", name: "Theme: Consilium", hint: "Default indigo and ivory", run: () => setPrefs((p) => ({ ...p, theme: "consilium" })) },
    { id: "theme-cyber", name: "Theme: Cyber", hint: "Cyberpunk blue-teal", run: () => setPrefs((p) => ({ ...p, theme: "cyber" })) },
    { id: "theme-slate", name: "Theme: Slate", hint: "Clean slate dark", run: () => setPrefs((p) => ({ ...p, theme: "slate" })) },
    { id: "theme-crt", name: "Theme: CRT", hint: "Green phosphor terminal", run: () => setPrefs((p) => ({ ...p, theme: "crt" })) },
    { id: "theme-retro84", name: "Theme: Retro '84", hint: "Neon grid, scanlines, glow", run: () => setPrefs((p) => ({ ...p, theme: "retro84" })) },
  ], [current, discuss, members, openUsage, requestingStatus]);

  const filteredCommands = useMemo(() => {
    const q = paletteQuery.trim().toLowerCase();
    if (!q) return paletteCommands;
    return paletteCommands.filter((c) => c.name.toLowerCase().includes(q) || (c.hint && c.hint.toLowerCase().includes(q)));
  }, [paletteCommands, paletteQuery]);

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
        <button title="Start a new, empty conversation with the council" className="btn primary wide" onClick={newConversation}>
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
                onClick={openUsage}
                title="Show token counts for this conversation and today"
              >
                usage
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
          {members.map((a) => {
            const isWorking = busy && (turn?.agents?.includes(a.id) ?? (turn?.agent === a.id));
            const activity = activities[a.id];
            const lastSignal = activity?.updated_at ?? turnStartTime;
            const agentLoad = pressure?.agents?.[a.id];
            // Silence plus no measurable process activity is a stall. Silence while CPU or memory
            // is moving is honest heavy-compute time, not labelled frozen.
            const isFrozen = Boolean(
              isWorking && lastSignal && Date.now() - lastSignal > 25_000 && agentLoad !== undefined && agentLoad < 1,
            );
            return (
              <AgentDot
                key={a.id}
                agent={a}
                onSetup={setupAgent}
                onRetry={retryAgent}
                check={checks[a.id]}
                forecast={forecast}
                isWorking={isWorking}
                isFrozen={isFrozen}
                activity={activity}
                load={agentLoad}
              />
            );
          })}
          <ResourcePressureGauge pressure={pressure} />
        </div>
        <div className="sidebar-actions">
          <button className="btn ghost" onClick={() => setShowAddAi(true)} title="Add or remove AIs">
            + Add AI
          </button>
          <button
            title="Theme, default folder, access level, shared memory, terminal command and updates"
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
            <button title="Open the release page for the new version on GitHub" className="link" onClick={() => openUrl(update.url)}>
              View release
            </button>
            <button title="Hide this notice until the next check" className="link" onClick={() => setUpdate(null)}>
              Later
            </button>
          </div>
        )}
        <header className="topbar">
          <div className="title-block">
            <h1 onDoubleClick={renameCurrent} title="Double-click to rename">
              {meta?.title ?? "Consilium"}
            </h1>
            <button
              className="link hover-faint"
              onClick={() => {
                setPaletteQuery("");
                setPaletteIndex(0);
                setShowPalette(true);
              }}
              title="Open command palette (Cmd+K)"
              style={{ fontSize: "11px", padding: "2px 7px", border: "1px solid var(--border)", borderRadius: "4px" }}
            >
              ⌘K
            </button>
            <div className="workspace">
              {meta?.workspace ? (
                <>
                  <span className="folder" title={meta.workspace}>
                    Project: <b>{folderName(meta.workspace)}</b>
                  </span>
                  <button title="Pick a different project folder for this conversation" className="link" onClick={chooseFolder}>change</button>
                  <button title="Stop using a project folder; the AIs only chat" className="link" onClick={clearFolder}>remove</button>
                </>
              ) : council?.default_workspace ? (
                <>
                  <span className="folder" title={council.default_workspace}>
                    Project: <b>{folderName(council.default_workspace)}</b> (default)
                  </span>
                  <button title="Pick a different project folder for this conversation" className="link" onClick={chooseFolder}>use another folder</button>
                </>
              ) : (
                <button className="link" onClick={chooseFolder} title="Let the AIs work in a folder, according to each one's access">
                  + Add project folder
                </button>
              )}
            </div>
          </div>
          <div className="topbar-actions">
            <button
              title="Ask every AI for a concise report of completed work, current work, blockers, uncommitted changes and next step"
              className="btn ghost status-request"
              onClick={requestStatus}
              disabled={!current || !members.length || requestingStatus}
            >
              {requestingStatus ? "Requesting…" : "Status Request"}
            </button>
            <button title="Copy the whole conversation to the clipboard as Markdown" className="btn ghost" onClick={exportCurrent} disabled={!events.length}>
              Copy as Markdown
            </button>
            <button title="Delete this conversation permanently" className="btn ghost danger" onClick={deleteCurrent}>
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
                  <button title="Install and sign in to the AIs that aren't ready yet" className="btn primary" onClick={setupAll}>
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
                  {e.usage && e.usage.total_tokens > 0 && (
                    <span
                      className="tokens-tag"
                      title={`Turn usage:\n• Total: ${e.usage.total_tokens.toLocaleString()}\n• Input: ${e.usage.input_tokens.toLocaleString()}\n• Output: ${e.usage.output_tokens.toLocaleString()}${e.usage.thinking_tokens ? `\n• Thinking: ${e.usage.thinking_tokens.toLocaleString()}` : ""}${e.usage.cache_read_tokens ? `\n• Cache read: ${e.usage.cache_read_tokens.toLocaleString()}` : ""}`}
                    >
                      {formatTokens(e.usage.total_tokens)} tokens
                    </span>
                  )}
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
                <MessageBody event={e} answered={answered} deciding={decidingRequests} decide={e.sender === "human" ? undefined : decide} />
                {e.add_ai?.filter((id) => agents.some((a) => a.id === id && !a.member)).map((id) => {
                  const a = agents.find((x) => x.id === id)!;
                  return (
                    <div key={id} className="addai-suggest">
                      {senderName(e.sender, agents)} suggests adding <b>{a.name}</b> to the council.
                      <button title="Bring this AI into the council, installing it or opening its sign-in first if needed" className="btn primary small-btn" onClick={() => bringIn(a)}>
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
            {staged && (
              <button
                className="btn small update-now"
                disabled={updating}
                onClick={updateNow}
                title={`Pause the AIs, install ${staged.version} (now ${staged.current}) and restart. Their work resumes after the restart.`}
              >
                {updating ? "Updating…" : `Update Now · ${staged.version}`}
              </button>
            )}
            <button
              className={`btn small voice-toggle ${voice ? "on" : ""}`}
              onClick={() => switchVoice(!voice)}
              disabled={!current}
              title={voice ? "Back to reading and typing; the AIs write normally again" : "Speak to the council and hear the AIs' replies read aloud; the AIs switch to short spoken answers"}
            >
              {voice ? "Switch to Text Chat" : "Switch to Voice Chat"}
            </button>
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
          {voice && (
            <div className={`voice-bar ${speaking ? "voice-speaking" : "voice-listening"}`}>
              <span className="voice-dot" />
              {speaking ? (
                <>
                  <span>Reading the replies aloud…</span>
                  <button title="Stop reading this reply aloud and move on to the next" className="btn small" onClick={() => speakerRef.current?.stop()}>
                    Skip
                  </button>
                </>
              ) : canListen ? (
                <span className="voice-heard">{heard || "Listening… speak, and your message is sent when you pause."}</span>
              ) : (
                <span>
                  Speech recognition isn't available in this window. Press the Fn key twice (macOS Dictation) in the message box to speak, then Enter to send.
                </span>
              )}
              {!canSpeak && <span className="voice-warn">Read-aloud isn't available on this system.</span>}
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
                <button
                  className="btn stop"
                  onClick={() => {
                    speakerRef.current?.stop();
                    if (current) api.stop(current);
                  }}
                  title="Stop the AIs now"
                >
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
                  title={`Use the ${t.name} theme`}
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
              <button title="Choose the folder new conversations start in" className="link" onClick={() => chooseDefaultFolder()}>
                choose
              </button>
              {council?.default_workspace && (
                <button title="New conversations start without a project folder" className="link" onClick={() => chooseDefaultFolder(true)}>
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
              <button title="Give the AIs full access to the project folder: they can edit files and run commands without asking" className="btn ghost" onClick={fullCapacity}>
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
              <button title="Save your edits to the shared memory every AI reads" className="btn ghost" onClick={saveMemory} disabled={!memoryDirty}>
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
              <button title="Open codex-reset.com, the unofficial source of the reset forecast" className="link" onClick={() => openUrl(FORECAST_SITE)}>
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
              <button title="Install the consilium command so you can talk to the council from a terminal" className="btn ghost" onClick={installCli}>
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
              <button title="Check GitHub for a newer Consilium release now" className="btn ghost" onClick={checkNow}>
                Check now
              </button>
              <button title="Open the list of all Consilium releases on GitHub" className="link" onClick={() => openUrl(RELEASES_URL)}>
                All releases
              </button>
              <span className="muted">{updateNote}</span>
            </div>
            <p className="muted small">
              Updates are never installed automatically. Consilium only tells you when a new release exists.
              Your conversations stay on this computer. CLI agents use their provider's existing sign-in;
              optional API-agent keys are saved locally and are never added to the conversation or returned to this UI.
            </p>
            <div className="row end">
              <button title="Close Settings" className="btn primary" onClick={() => setShowSettings(false)}>
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
              Most AIs join through their company's official command-line tool and your own account. Grok and DeepSeek
              have no such tool, so they use an API key you paste here: it is saved only on this computer, never in the chat.
              You decide who joins; an AI in the conversation can suggest one, but only you can add it.
            </p>
            <p className="setup-boundary">
              Consilium can open the official installer, sign-in, account, key, and plan pages. Provider pages open in a
              separate browser window; you enter any identity or payment details and confirm any purchase yourself.
            </p>
            {others.length === 0 && <p className="muted">Every available AI is already in the council.</p>}
            {others.map((a) => {
              const st = agentState(a);
              const portal = PROVIDER_PORTALS[a.id];
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
                  {a.transport === "api" ? (
                  <ol className="addai-steps">
                    <li>
                      Create or open your {a.company} account
                      {portal && (
                        <button title="Open this AI's account page in your browser" className="btn ghost small-btn" onClick={() => openUrl(portal.account).catch((e) => setError(String(e)))}>
                          {portal.accountLabel}
                        </button>
                      )}
                      {portal?.plan && portal.plan !== portal.account && (
                        <button title="Open this AI's plans and billing page in your browser; you enter any payment details yourself" className="link" onClick={() => openUrl(portal.plan!).catch((e) => setError(String(e)))}>
                          billing / plans
                        </button>
                      )}
                    </li>
                    <li className={st === "ready" ? "done" : ""}>
                      {st === "ready" ? `API key saved (model ${a.version ?? "?"})` : `Paste your ${a.company} API key`}
                      <ApiKeyField agent={a} onSaved={refreshAgents} onError={(e) => setError(e)} />
                    </li>
                    <li>
                      Add to the council
                      <button title="Add this AI to the council for this conversation" className="btn primary small-btn" disabled={st !== "ready"} onClick={() => addMember(a)}>
                        Add {a.name}
                      </button>
                    </li>
                  </ol>
                  ) : (
                  <ol className="addai-steps">
                    <li className={st !== "missing" ? "done" : ""}>
                      Install the {a.tool}
                      {st === "missing" && (
                        <button title="Install this AI's command-line tool in a terminal window" className="btn ghost small-btn" onClick={() => setupAgent(a)}>
                          Install
                        </button>
                      )}
                    </li>
                    <li className={st === "ready" ? "done" : ""}>
                      Sign in with your {a.company} account
                      {st === "signin" && (
                        <button title="Open this AI's sign-in in a terminal window" className="btn ghost small-btn" onClick={() => setupAgent(a)}>
                          Sign in
                        </button>
                      )}
                    </li>
                    <li>
                      Add to the council
                      <button title="Add this AI to the council for this conversation" className="btn primary small-btn" disabled={st !== "ready"} onClick={() => addMember(a)}>
                        Add {a.name}
                      </button>
                    </li>
                    {portal?.plan && (
                      <li className="optional-step">
                        Optional: review higher provider limits
                        <button title="Open this AI's plans page in your browser; you enter any payment details yourself" className="link" onClick={() => openUrl(portal.plan!).catch((e) => setError(String(e)))}>
                          open plans
                        </button>
                      </li>
                    )}
                  </ol>
                  )}
                  {st !== "ready" && <p className="muted small">{a.hint}{a.transport === "api" ? "" : " When you're done, come back here; it checks again automatically."}</p>}
                </div>
              );
            })}
            <div className="section-label">In the council</div>
            {members.map((a) => (
              <div key={a.id} className="member-row">
                <span className={`agent-name agent-${a.id}`}>{a.name}</span>
                <span className="muted small">{a.company}</span>
                <span className="spacer" />
                <button title="Take this AI out of the council (at least one must stay)" className="link danger-link" disabled={members.length < 2} onClick={() => removeMember(a)}>
                  remove
                </button>
              </div>
            ))}
            <div className="row end">
              <button title="Check every AI's install and sign-in status again" className="btn ghost" onClick={refreshAgents} disabled={agentsRefreshing}>
                {agentsRefreshing ? "Checking…" : "Check again"}
              </button>
              <button title="Close Add AI" className="btn primary" onClick={() => setShowAddAi(false)}>
                Done
              </button>
            </div>
          </div>
        </div>
      )}
      {showUsage && (
        <div className="modal-backdrop" onClick={() => setShowUsage(false)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <h2>Token Usage</h2>
            <p className="muted small">
              Provider-reported token counts recorded from CLI outputs (Codex, Claude Code, Antigravity) for each turn.
            </p>
            {usageLoading ? (
              <div className="muted small">Loading token usage…</div>
            ) : tokenReport ? (
              <div className="usage-report">
                <div className="section-label">This conversation</div>
                {Object.keys(tokenReport.conversation).length === 0 ? (
                  <p className="muted small">No tokens recorded yet in this conversation.</p>
                ) : (
                  <table className="usage-table">
                    <thead>
                      <tr>
                        <th>AI</th>
                        <th>Input</th>
                        <th>Output</th>
                        <th>Thinking</th>
                        <th>Cache</th>
                        <th>Total</th>
                      </tr>
                    </thead>
                    <tbody>
                      {members.map((m) => {
                        const u = tokenReport.conversation[m.id];
                        if (!u) return null;
                        return (
                          <tr key={m.id} className={`agent-${m.id}`}>
                            <td><span className="agent-name">{m.name}</span></td>
                            <td>{u.input_tokens.toLocaleString()}</td>
                            <td>{u.output_tokens.toLocaleString()}</td>
                            <td>{u.thinking_tokens ? u.thinking_tokens.toLocaleString() : "—"}</td>
                            <td>{u.cache_read_tokens ? u.cache_read_tokens.toLocaleString() : "—"}</td>
                            <td><b>{u.total_tokens.toLocaleString()}</b></td>
                          </tr>
                        );
                      })}
                    </tbody>
                  </table>
                )}

                <div className="section-label" style={{ marginTop: 16 }}>Today (all conversations)</div>
                {Object.keys(tokenReport.today).length === 0 ? (
                  <p className="muted small">No tokens recorded today.</p>
                ) : (
                  <table className="usage-table">
                    <thead>
                      <tr>
                        <th>AI</th>
                        <th>Input</th>
                        <th>Output</th>
                        <th>Thinking</th>
                        <th>Cache</th>
                        <th>Total</th>
                      </tr>
                    </thead>
                    <tbody>
                      {members.map((m) => {
                        const u = tokenReport.today[m.id];
                        if (!u) return null;
                        return (
                          <tr key={m.id} className={`agent-${m.id}`}>
                            <td><span className="agent-name">{m.name}</span></td>
                            <td>{u.input_tokens.toLocaleString()}</td>
                            <td>{u.output_tokens.toLocaleString()}</td>
                            <td>{u.thinking_tokens ? u.thinking_tokens.toLocaleString() : "—"}</td>
                            <td>{u.cache_read_tokens ? u.cache_read_tokens.toLocaleString() : "—"}</td>
                            <td><b>{u.total_tokens.toLocaleString()}</b></td>
                          </tr>
                        );
                      })}
                    </tbody>
                  </table>
                )}
              </div>
            ) : null}
            <div className="row end">
              <button title="Close the Usage panel" className="btn primary" onClick={() => setShowUsage(false)}>
                Done
              </button>
            </div>
          </div>
        </div>
      )}
      {showPalette && (
        <div className="palette-backdrop" onClick={() => setShowPalette(false)}>
          <div className="palette-modal" onClick={(e) => e.stopPropagation()}>
            <div className="palette-input-wrap">
              <input
                ref={(el) => el?.focus()}
                className="palette-input"
                placeholder="Type a command or search actions… (Esc to exit)"
                value={paletteQuery}
                onChange={(e) => {
                  setPaletteQuery(e.target.value);
                  setPaletteIndex(0);
                }}
                onKeyDown={(e) => {
                  if (e.key === "Escape") {
                    setShowPalette(false);
                  } else if (e.key === "ArrowDown") {
                    e.preventDefault();
                    setPaletteIndex((i) => Math.min(filteredCommands.length - 1, i + 1));
                  } else if (e.key === "ArrowUp") {
                    e.preventDefault();
                    setPaletteIndex((i) => Math.max(0, i - 1));
                  } else if (e.key === "Enter") {
                    e.preventDefault();
                    if (filteredCommands[paletteIndex]) {
                      filteredCommands[paletteIndex].run();
                      setShowPalette(false);
                    }
                  }
                }}
              />
              <span className="palette-shortcut-badge">Esc</span>
            </div>
            <div className="palette-list">
              {filteredCommands.length === 0 ? (
                <div className="palette-empty">No matching actions found</div>
              ) : (
                filteredCommands.map((cmd, idx) => (
                  <button
                    key={cmd.id}
                    className={`palette-item ${idx === paletteIndex ? "active" : ""}`}
                    onClick={() => {
                      cmd.run();
                      setShowPalette(false);
                    }}
                    onMouseEnter={() => setPaletteIndex(idx)}
                  >
                    <span>{cmd.name}</span>
                    {cmd.hint && <small>{cmd.hint}</small>}
                  </button>
                ))
              )}
            </div>
          </div>
        </div>
      )}
      {toast && <div className="toast">{toast}</div>}
    </div>
  );
}
