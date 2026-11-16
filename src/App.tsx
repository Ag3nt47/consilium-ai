import { TypedText, TYPED_THEMES } from "./typed";
import { fencedBlocks, shellName, needsAdmin, handledCommands, deniedText, type Fence } from "./commands";
import { dictationHint, paletteKey, paletteKeyWords } from "./platform";
import { forwardRef, memo, useCallback, useEffect, useImperativeHandle, useMemo, useRef, useState } from "react";
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
  type DailyWallpaper,
  type ResetForecast,
  type SystemPressure,
  type StagedUpdate,
  type TokenUsageReport,
  type TurnState,
  type UsageWindow,
} from "./api";
import { canListen, canSpeak, Listener, Speaker, voiceOn } from "./voice";
import { VoiceSettings } from "./voicesettings";
import { AgentVoiceMeter, VoiceVisualizer } from "./visualizer";
import { checkForUpdate, RELEASES_URL, type UpdateInfo } from "./updates";
import { installHoverTips } from "./tooltip";
import { AuditDrawer, loadPersona, PersonaChips, savePersona, withPersona } from "./qol";
import { MessagingIndicator, MessagingSettings } from "./messaging";
import { LinkIndicator, LinkSettings } from "./link";
import { ProjectVersions } from "./projects";
import { RepoDrawer } from "./repo";
import { demoAddAi, demoDraft, demoSettings, demoTheme, demoTurn } from "./demo";

const THEMES = [
  { id: "consilium", name: "Consilium", note: "Indigo, ivory and gold" },
  { id: "glass", name: "visionOS Glass", note: "Frosted translucent glass, ambient glow" },
  { id: "cyber", name: "Cyber", note: "From the UI Design Bundle" },
  { id: "slate", name: "Slate", note: "From the UI Design Bundle" },
  { id: "crt", name: "CRT", note: "Green phosphor terminal" },
  { id: "retro84", name: "Retro '84", note: "Neon grid, scanlines, glow" },
  { id: "netrunner", name: "Netrunner", note: "Black, neon green, cyberpunk (default)" },
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

type Prefs = { theme: string; effects: boolean; autoUpdate: boolean; bingWallpaper: boolean };
const PREFS_KEY = "consilium.prefs";
const REQUEST_FLOOR_KEY = "consilium.request-floor.v2";

function loadPrefs(): Prefs {
  // New installs start in Netrunner; anyone who has already picked a theme keeps it.
  const defaults: Prefs = { theme: "netrunner", effects: true, autoUpdate: true, bingWallpaper: true };
  // Demo mode (README screenshots) keeps the Consilium look unless ?theme= says otherwise.
  if (!IN_APP) return { ...defaults, theme: demoTheme || "consilium", autoUpdate: false };
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

/**
 * Ignore the historical backlog when request tracking is first introduced to an existing
 * conversation. The floor is saved per conversation; every later REQUEST remains pending across
 * reloads until an Approved, Denied, or custom Response event resolves it.
 */
function initializeRequestFloor(conversation: string, loaded: ChatEvent[]): number {
  if (!IN_APP) return 0;
  const key = `${REQUEST_FLOOR_KEY}.${conversation}`;
  try {
    const saved = Number.parseInt(localStorage.getItem(key) ?? "", 10);
    if (Number.isSafeInteger(saved) && saved >= 0) return saved;
    const floor = loaded.reduce((largest, event) => Math.max(largest, event.seq), 0);
    localStorage.setItem(key, String(floor));
    return floor;
  } catch {
    // The in-memory floor still prevents old requests from appearing during this session.
    return loaded.reduce((largest, event) => Math.max(largest, event.seq), 0);
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

type SeenReceipt = { agent: string; ts: number };

/** Read receipts are compact log events that can cover several messages at the same timestamp. */
function receiptsByMessage(events: ChatEvent[]): Map<string, SeenReceipt[]> {
  const out = new Map<string, SeenReceipt[]>();
  for (const event of events) {
    if (event.kind !== "seen") continue;
    for (const message of event.seen ?? []) {
      const readers = out.get(message) ?? [];
      if (!readers.some((reader) => reader.agent === event.sender)) {
        readers.push({ agent: event.sender, ts: event.ts });
        out.set(message, readers);
      }
    }
  }
  return out;
}

function SeenLine({ receipts, agents }: { receipts: SeenReceipt[]; agents: AgentInfo[] }) {
  if (!receipts.length) return null;
  return (
    <div className="seen-line" aria-label={`Seen by ${receipts.length} AI${receipts.length === 1 ? "" : "s"}`}>
      <em>Seen:</em>{" "}
      {receipts
        .slice()
        .sort((a, b) => a.ts - b.ts)
        .map((receipt) => `${senderName(receipt.agent, agents)} ${time(receipt.ts)}`)
        .join(" · ")}
    </div>
  );
}

type TaskClaim = {
  agent: string;
  action: string;
  task: string;
};

/** Extracts explicit or natural language task ownership from an AI's message. */
function extractTaskClaims(text: string, defaultAgent: string, agents: AgentInfo[]): TaskClaim[] {
  if (defaultAgent === "human") return [];
  const claims: TaskClaim[] = [];
  const seen = new Set<string>();

  // Check multi-agent breakdown lines: e.g. "- **@claude:** <task>" or "@claude will handle <task>"
  for (const line of text.split("\n")) {
    const multiMatch = /^\s*(?:[-*]\s+)?(?:\*\*)?@([a-z0-9_-]+)(?:\*\*)?:?(?:\*\*)?\s*(.+)$/i.exec(line);
    if (multiMatch) {
      const targetId = multiMatch[1].toLowerCase();
      const targetAgent = agents.find((a) => a.id.toLowerCase() === targetId || a.name.toLowerCase() === targetId);
      const desc = multiMatch[2].replace(/\*\*/g, "").trim();
      const assignsWork = /^(?:please\s+)?(?:take|taking|handle|own|implement|build|fix|review|work|proceed|connect|add|create|prepare|continue|finish|stage|commit|inspect|audit|monitor|capture|update|test)\b/i.test(desc);
      if (targetAgent && assignsWork) {
        const short = desc.slice(0, 160);
        const key = `${targetAgent.id}:${short}`;
        if (!seen.has(key)) {
          seen.add(key);
          claims.push({ agent: targetAgent.id, action: targetAgent.id === defaultAgent ? "is taking" : "was assigned", task: short });
        }
      }
    }
  }

  // Check explicit author claim
  const explicit = /^\s*(?:TAKING|OWNING|WORKING ON|CLAIM_TASK):\s*(.+)$/im.exec(text)?.[1];
  if (explicit) {
    const desc = explicit.trim().slice(0, 160);
    const key = `${defaultAgent}:${desc}`;
    if (!seen.has(key)) {
      seen.add(key);
      claims.push({ agent: defaultAgent, action: "claimed", task: desc });
    }
  }

  // Natural language claim from author
  const natural = /\bI(?:'ll|’ll| will| am|'m|’m)\s+(?:take|handle|own|implement|build|fix|review|work on)\s+([^.!?\n]{3,160})/i.exec(text)?.[1];
  if (natural) {
    const desc = natural.trim();
    const key = `${defaultAgent}:${desc}`;
    if (!seen.has(key)) {
      seen.add(key);
      claims.push({ agent: defaultAgent, action: "is taking", task: desc });
    }
  }

  return claims;
}

function taskColor(agent: string): string {
  return `var(--c-${agent}, var(--accent))`;
}

function ClaimedTask({ event, agents }: { event: ChatEvent; agents: AgentInfo[] }) {
  const claims = useMemo(() => extractTaskClaims(event.text, event.sender, agents), [event.text, event.sender, agents]);
  if (!claims.length) return null;
  return (
    <div className="task-claims-container">
      {claims.map((c, idx) => (
        <div key={idx} className="task-claim-italic" style={{ color: taskColor(c.agent) }}>
          <span className="task-dot" style={{ backgroundColor: taskColor(c.agent) }} />
          <em>
            ↳ <b>{senderName(c.agent, agents)}</b> {c.action}: {c.task} <small className="task-ts">({time(event.ts)})</small>
          </em>
        </div>
      ))}
    </div>
  );
}

function taskObjective(text: string): string {
  const first = text
    .split("\n")
    .map((line) => line.trim())
    .find((line) => line && !/^\[(?:Persona|Role):/i.test(line));
  if (!first) return "this request";
  return first.length > 90 ? `${first.slice(0, 87)}…` : first;
}

/**
 * Ownership is distinct from explainability: this says who took the input and why, while live
 * Reading/Editing/Running telemetry remains exclusively in the activity line at the bottom.
 */
function TaskOwnership({ ids, objective, agents }: { ids: string[]; objective: string; agents: AgentInfo[] }) {
  if (!ids.length) return null;
  return (
    <div className="active-tasks task-ownership" aria-label="AIs working on this message">
      {ids.map((id) => (
        <div key={id} className="task-pill" style={{ borderColor: taskColor(id) }} title={`${senderName(id, agents)} is working on: ${objective}`}>
          <span className="task-dot" style={{ backgroundColor: taskColor(id) }} />
          <b>{senderName(id, agents)}</b>
          <span>is working on</span>
          <small>{objective}</small>
        </div>
      ))}
    </div>
  );
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

/** A free-form answer from the response field in a REQUEST card. */
function responseText(request: string, response: string): string {
  return `Response to request: ${request.trim()}\n${response.trim()}`;
}

function requestKey(eventId: string, line: number): string {
  return `${eventId}:${line}`;
}

/**
 * Match each human button answer to the request cards that existed when it was sent.
 * Keeping event-specific keys means a later request with the same wording still gets fresh buttons.
 */
type RequestResolution = "approved" | "denied" | "responded";

function answeredRequests(events: ChatEvent[]): Map<string, RequestResolution> {
  const out = new Map<string, RequestResolution>();
  const waiting = new Map<string, string[]>();
  for (const e of events) {
    if (e.kind !== "message" && e.kind !== "error") continue;
    if (e.sender !== "human") {
      for (const [line, text] of e.text.split("\n").entries()) {
        const request = /^\s*REQUEST:\s*(.+)$/i.exec(text)?.[1].trim();
        if (!request) continue;
        waiting.set(request, [...(waiting.get(request) ?? []), requestKey(e.id, line)]);
      }
      continue;
    }
    if (e.kind !== "message") continue;
    const answer = /^(Approved|Denied|Response to request):\s*([^\n]+)(?:\n[\s\S]*)?$/i.exec(e.text.trim());
    if (!answer) continue;
    const request = answer[2].trim();
    const queued = waiting.get(request);
    const key = queued?.shift();
    if (key) {
      const kind = answer[1].toLowerCase();
      out.set(key, kind === "approved" ? "approved" : kind === "denied" ? "denied" : "responded");
      if (!queued?.length) waiting.delete(request);
    }
  }
  return out;
}

/** Requests from the AIs that haven't been approved or denied yet, oldest first. */
function pendingRequests(events: ChatEvent[], answered: Map<string, RequestResolution>, afterSeq: number | null): string[] {
  if (afterSeq === null) return [];
  const out: string[] = [];
  for (const e of events) {
    if (e.seq <= afterSeq || (e.kind !== "message" && e.kind !== "error") || e.sender === "human") continue;
    for (const [line, text] of e.text.split("\n").entries()) {
      if (!/^\s*REQUEST:\s*\S/i.test(text)) continue;
      const key = requestKey(e.id, line);
      if (!answered.has(key)) out.push(key);
    }
  }
  return out;
}

function requestElementId(key: string): string {
  return `request-${key}`;
}

type Decide = (key: string, request: string, requester: string, approved: boolean) => void;
type Respond = (key: string, request: string, requester: string, response: string) => Promise<void>;

/** Message text with REQUEST: lines shown as red request callouts, with decision controls when supplied. */
function MessageBody({ event, answered, deciding, decide, respond, commands }: { event: ChatEvent; answered?: Map<string, RequestResolution>; deciding?: Set<string>; decide?: Decide; respond?: Respond; commands?: CommandActions }) {
  const text = event.text;
  const lines = text.split("\n");
  const fences = fencedBlocks(text);
  const fenceAt = new Map(fences.map((f, index) => [f.start, index]));
  const inside = new Set(fences.flatMap((f) => Array.from({ length: f.end - f.start }, (_, k) => f.start + 1 + k)));
  return (
    <div className="body">
      {lines.map((line, i) => {
        const fi = fenceAt.get(i);
        if (fi !== undefined) return <CommandBox key={i} event={event} fence={fences[fi]} index={fi} actions={event.sender === "human" || !fences[fi].closed ? undefined : commands} />;
        if (inside.has(i)) return null;
        if (/^\s*\(command ref [^\s#)]+#\d+\)\s*$/.test(line)) return null;
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
            <div key={i} id={requestElementId(key)} className="request">
              <span className="request-label">REQUEST:</span> <Inline text={m[1]} />
              {decide && (
                <RequestActions
                  answer={answered?.get(key)}
                  pending={deciding?.has(key)}
                  onDecide={(ok) => decide(key, m[1], event.sender, ok)}
                  onRespond={(response) => respond?.(key, m[1], event.sender, response) ?? Promise.resolve()}
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

type CommandActions = {
  handled: Map<string, "ran" | "denied">;
  running: Set<string>;
  run: (eventId: string, index: number) => void;
  deny: (eventId: string, index: number, sender: string) => void;
};

/** A fenced code block. Shell blocks from an AI get Run and Deny; every block gets Copy. */
function CommandBox({ event, fence, index, actions }: { event: ChatEvent; fence: Fence; index: number; actions?: CommandActions }) {
  const [copied, setCopied] = useState(false);
  const shell = shellName(fence.lang);
  const key = `${event.id}#${index}`;
  const state = actions?.handled.get(key);
  const running = actions?.running.has(key);
  const copy = () => {
    void navigator.clipboard.writeText(fence.code).then(() => {
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    });
  };
  return (
    <div className={`command-box${shell ? " shell" : ""}`}>
      <div className="command-head">
        <span className="command-lang">{shell ?? (fence.lang || "code")}{shell && needsAdmin(fence.code) ? " · admin" : ""}</span>
        <span className="command-actions">
          {shell && actions && (state ? (
            <span className={`request-answer ${state === "ran" ? "approved" : "denied"}`}>{state === "ran" ? "✓ Ran" : "✕ Denied"}</span>
          ) : (
            <>
              <button type="button" className="btn small-btn approve" disabled={running} onClick={() => actions.run(event.id, index)}
                title={needsAdmin(fence.code) ? "Run this now. Your computer asks for your password itself; Consilium never sees it. The output goes back to the AI that suggested it." : "Run this now in the project folder. The output goes back to the AI that suggested it."}>
                {running ? "Running…" : "Run"}
              </button>
              <button type="button" className="btn small-btn deny" disabled={running} onClick={() => actions.deny(event.id, index, event.sender)} title="Don't run it, and tell the AI that suggested it">
                Deny
              </button>
            </>
          ))}
          <button type="button" className="btn small-btn copy" onClick={copy} title="Copy the command to paste into your own terminal">
            {copied ? "✓ Copied" : "Copy"}
          </button>
        </span>
      </div>
      <pre><code>{fence.code}</code></pre>
    </div>
  );
}

function RequestActions({ answer, pending, onDecide, onRespond }: { answer?: RequestResolution; pending?: boolean; onDecide: (approved: boolean) => void; onRespond: (response: string) => Promise<void> }) {
  const [response, setResponse] = useState("");
  if (answer !== undefined) {
    const label = answer === "approved" ? "✓ Approved" : answer === "denied" ? "✕ Denied" : "↪ Responded";
    return <span className={`request-answer ${answer}`} aria-live="polite">{label}</span>;
  }
  const submitResponse = async () => {
    if (!response.trim() || pending) return;
    try {
      await onRespond(response.trim());
      setResponse("");
    } catch {
      // The parent keeps the text in place and shows the send error above the composer.
    }
  };
  return (
    <span className="request-actions">
      <button title="Say yes: posts “Approved: …” to the council so the AI that asked can go ahead" type="button" className="btn small-btn approve" disabled={pending} onClick={() => onDecide(true)} aria-label="Approve this request">
        {pending ? "Sending…" : "Approve"}
      </button>
      <button title="Say no: posts “Denied: …” to the council so the AI that asked won't do it" type="button" className="btn small-btn deny" disabled={pending} onClick={() => onDecide(false)} aria-label="Deny this request">
        Deny
      </button>
      <span className="request-response">
        <input
          value={response}
          disabled={pending}
          onChange={(event) => setResponse(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Enter") {
              event.preventDefault();
              void submitResponse();
            }
          }}
          placeholder="Write a response…"
          aria-label="Write a custom response to this request"
        />
        <button title="Send this response to the AI that asked" type="button" className="btn small-btn respond" disabled={pending || !response.trim()} onClick={() => void submitResponse()}>
          Respond
        </button>
      </span>
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
      <button title="Check this API key with the provider and save it on this computer only" className="btn ghost small-btn" disabled={busy || !key.trim()} onClick={() => save(key)}>
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
  load,
}: {
  agent: AgentInfo;
  onSetup: (a: AgentInfo) => void;
  onRetry: (a: AgentInfo) => void;
  check?: CheckResult | "running";
  forecast?: ResetForecast | null;
  isWorking?: boolean;
  isFrozen?: boolean;
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
          ? "Stalled: no provider activity for 25 seconds"
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
        <span className="agent-state" style={{ color: "var(--accent)" }}>Working…</span>
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

/** Green at 0%, through yellow, to red at 100%. */
function pressureColor(percent: number): string {
  const pct = Math.min(100, Math.max(0, percent));
  return `hsl(${Math.round(120 - (pct / 100) * 120)}, 85%, 50%)`;
}

function ordinal(n: number): string {
  return n === 2 ? "2nd" : n === 3 ? "3rd" : `${n}th`;
}

function ResourcePressureGauge({ pressure }: { pressure: SystemPressure | null }) {
  if (!pressure) return null;
  const pct = Math.min(100, Math.max(0, pressure.percent));
  const color = pressureColor(pct);
  return (
    <div
      className="pc-pressure"
      title={`Host System Pressure: ${pct}%\nCPU: ${pressure.cpu_percent}% (${pressure.cores} cores)\nMemory: ${pressure.memory_percent}% of ${Math.max(1, Math.round(pressure.total_memory_mb / 1024))} GiB\nAdaptive mode: ${pressure.adaptive_mode}\nConcurrent AI limit: ${pressure.recommended_parallel_agents}\nStatus: ${pressure.label}`}
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
      <div className="pc-pressure-sub muted small" title="Consilium adjusts visual effects, polling and simultaneous AI processes for this PC">
        <span>Auto {pressure.adaptive_mode}</span>
        <span>max {pressure.recommended_parallel_agents} AI{pressure.recommended_parallel_agents === 1 ? "" : "s"}</span>
      </div>
    </div>
  );
}

// The prompt box keeps its own text so typing re-renders only this textarea, not the whole
// conversation (which made the caret lag in long chats). The app reads and sets it through a handle
// and only hears about empty <-> non-empty changes, for the Send button.
type DraftHandle = { get(): string; set(text: string): void };
const DraftBox = memo(
  forwardRef<DraftHandle, { initial: string; onEmptyChange: (empty: boolean) => void; onSend: () => void; onPasteFiles: (files: File[]) => void }>(
    function DraftBox({ initial, onEmptyChange, onSend, onPasteFiles }, ref) {
      const [text, setText] = useState(initial);
      const textRef = useRef(text);
      const update = (v: string) => {
        if (!v.trim() !== !textRef.current.trim()) onEmptyChange(!v.trim());
        textRef.current = v;
        setText(v);
      };
      useImperativeHandle(ref, () => ({ get: () => textRef.current, set: update }));
      return (
        <textarea
          value={text}
          onPaste={(e) => {
            const files = Array.from(e.clipboardData.files);
            if (files.length) {
              if (!e.clipboardData.getData("text")) e.preventDefault();
              onPasteFiles(files);
            }
          }}
          placeholder="Message the council…  (Enter to send, Shift+Enter for a new line; drop or paste files to attach)"
          onChange={(e) => update(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              onSend();
            }
          }}
          rows={3}
        />
      );
    },
  ),
);

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
  const draftBox = useRef<DraftHandle>(null);
  const [draftEmpty, setDraftEmpty] = useState(!(IN_APP ? "" : demoDraft).trim());
  const setDraft = useCallback((text: string) => draftBox.current?.set(text), []);
  /** Files waiting to go with the next message. */
  const [pending, setPending] = useState<Attachment[]>([]);
  const [dropping, setDropping] = useState(false);
  const [targets, setTargets] = useState<string[]>(["gpt", "claude"]);
  const [discuss, setDiscuss] = useState(!IN_APP);
  const [rounds, setRounds] = useState(3);
  const [persona, setPersonaState] = useState(loadPersona);
  const [auditOpen, setAuditOpen] = useState(false);
  const [repoOpen, setRepoOpen] = useState(false);
  const [wallpaper, setWallpaper] = useState<DailyWallpaper | null>(null);
  const setPersona = (id: string) => {
    setPersonaState(id);
    savePersona(id);
  };
  const agentNames = useMemo(() => Object.fromEntries(agents.map((a) => [a.id, a.name])), [agents]);
  const [error, setError] = useState<string | null>(null);
  const [showSettings, setShowSettings] = useState(!IN_APP && demoSettings);
  const [showAddAi, setShowAddAi] = useState(!IN_APP && demoAddAi);
  const [showUsage, setShowUsage] = useState(false);
  const [showPalette, setShowPalette] = useState(false);
  // Voice chat: whether replies are being read aloud right now, and what the recogniser is hearing.
  const [speakingAgent, setSpeakingAgent] = useState<string | null>(null);
  const [speakQueue, setSpeakQueue] = useState<string[]>([]);
  const speaking = speakingAgent !== null;
  const [heard, setHeard] = useState("");
  const voiceRef = useRef(false);
  const agentsRef = useRef<AgentInfo[]>([]);
  const speakerRef = useRef<Speaker | null>(null);
  if (!speakerRef.current) speakerRef.current = new Speaker((playback) => {
    setSpeakingAgent(playback.current?.agent ?? null);
    setSpeakQueue(playback.queued.map((item) => item.agent));
  });
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
  const [requestFloor, setRequestFloor] = useState<number | null>(null);
  const currentRef = useRef<string | null>(null);
  const agentRefreshRef = useRef<Promise<void> | null>(null);
  const usageRefreshRef = useRef(false);
  const [forecast, setForecast] = useState<ResetForecast | null>(null);
  const [pressure, setPressure] = useState<SystemPressure | null>(null);
  const [staged, setStaged] = useState<StagedUpdate | null>(null);
  const [updating, setUpdating] = useState(false);
  const [stagedUi, setStagedUi] = useState<{ version: string } | null>(null);
  const [requestingStatus, setRequestingStatus] = useState(false);
  const [requestingUpdate, setRequestingUpdate] = useState(false);

  // Quick hover explanations for every control that has a title.
  useEffect(() => installHoverTips(), []);

  // Confirms this page started, so an instantly applied screen update isn't rolled back.
  useEffect(() => {
    api.uiLoaded().catch(() => undefined);
  }, []);

  // A staged build is a file check, so polling it is cheap.
  useEffect(() => {
    const poll = () => {
      api.updateAvailable().then(setStaged).catch(() => undefined);
      api.uiUpdateAvailable().then(setStagedUi).catch(() => undefined);
    };
    poll();
    const t = setInterval(poll, 15000);
    return () => clearInterval(t);
  }, []);

  // Screen-only update: swap the page in place; the AIs' work carries on untouched.
  const applyUi = async () => {
    setUpdating(true);
    try {
      await api.uiUpdateApply();
      window.location.reload();
    } catch (e) {
      setUpdating(false);
      alert(`Screen update failed: ${e}`);
    }
  };

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

  const adaptiveMode = pressure?.adaptive_mode ?? "balanced";

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
    const interval = setInterval(poll, adaptiveMode === "eco" ? 5000 : adaptiveMode === "full" ? 2000 : 3000);
    return () => {
      mounted = false;
      clearInterval(interval);
    };
  }, [adaptiveMode]);

  useEffect(() => {
    document.documentElement.dataset.theme = prefs.theme;
    document.documentElement.dataset.effects = prefs.effects ? "on" : "off";
    savePrefs(prefs);
  }, [prefs]);

  useEffect(() => {
    document.documentElement.dataset.performance = adaptiveMode;
  }, [adaptiveMode]);

  useEffect(() => {
    if (prefs.bingWallpaper) {
      api.getDailyWallpaper().then(setWallpaper).catch(() => undefined);
    } else {
      setWallpaper(null);
    }
  }, [prefs.bingWallpaper]);

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
    setRequestFloor(null);
    api.load(current)
      .then((loaded) => {
        setEvents(loaded);
        setRequestFloor(initializeRequestFloor(current, loaded));
      })
      .catch((e) => setError(String(e)));
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
  const seenByMessage = useMemo(() => receiptsByMessage(events), [events]);
  const latestHumanMessage = useMemo(
    () => events.slice().reverse().find((event) => event.kind === "message" && event.sender === "human") ?? null,
    [events],
  );
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
    const text = draftBox.current?.get() ?? "";
    if (!current || (!text.trim() && !pending.length)) return;
    setError(null);
    const files = pending;
    setDraft("");
    setPending([]);
    try {
      const to = targets.filter((t) => members.some((m) => m.id === t));
      await api.send(current, withPersona(text, persona), to, discuss ? rounds : 1, false, files);
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

  // Request Update: install what's already built, or else ask the council to find, build and stage the next one.
  async function requestUpdate() {
    if (updating || requestingUpdate) return;
    if (staged) return updateNow();
    if (stagedUi) return applyUi();
    if (!current || !members.length) return;
    setRequestingUpdate(true);
    setError(null);
    try {
      await api.send(
        current,
        "Update request: nothing is staged to install. If a build is already running, say so and when it should be ready; don't start a second one. Otherwise agree in one message who takes what, then work from docs/UPGRADE_STATUS.md, the canonical backlog for every requirement in the uploaded Word documents 'Consilium Upgrade Overhaul.docx', '2nd consilium update.docx' and '3rd update.docx'. Prioritize unfinished document items marked in progress or next before later items, docs/ROADMAP.md, open bugs, or newly noticed improvements. Build and test the selected work, update the backlog in the same commit, commit it, integrate it onto main, and push origin/main before staging it. Confirm GitHub checks pass on macOS, Windows and Linux. Stage with npm run update:ui for screen-only changes, or a version bump and npm run update:stage for engine changes. Do not mark a document item complete until it is implemented and verified. Only one build at a time on this machine.",
        members.map((member) => member.id),
        1,
      );
    } catch (e) {
      setError(`Couldn't request an update: ${e}`);
    } finally {
      setRequestingUpdate(false);
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
  // Stable handles for the prompt box, so it doesn't re-render when the app does.
  const sendRef = useRef(send);
  sendRef.current = send;
  const attachPastedRef = useRef(attachPasted);
  attachPastedRef.current = attachPasted;

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

  // A custom response resolves the request just like Approve/Deny, while preserving the exact
  // question and response in the append-only conversation history.
  async function respondToRequest(key: string, request: string, requester: string, response: string) {
    if (!current || !response.trim()) return;
    setError(null);
    setDecidingRequests((pending) => new Set(pending).add(key));
    try {
      const selected = targets.filter((t) => members.some((m) => m.id === t));
      const to = members.some((m) => m.id === requester) ? [requester] : selected;
      await api.send(current, responseText(request, response), to, discuss ? rounds : 1);
    } catch (e) {
      setError(String(e));
      throw e;
    } finally {
      setDecidingRequests((pending) => {
        const next = new Set(pending);
        next.delete(key);
        return next;
      });
    }
  }
  // Run on a command box: the engine reads the command from the stored message, runs it once and
  // sends the output to the AI that suggested it. Deny tells that AI not to run it.
  const [runningCommands, setRunningCommands] = useState<Set<string>>(() => new Set());
  const ranCommands = useMemo(() => handledCommands(events), [events]);
  const commandActions = useMemo(() => ({
    handled: ranCommands,
    running: runningCommands,
    run: (eventId: string, index: number) => {
      if (!current) return;
      const key = `${eventId}#${index}`;
      setError(null);
      setRunningCommands((r) => new Set(r).add(key));
      api.runCommand(current, eventId, index)
        .catch((e) => setError(String(e)))
        .finally(() => setRunningCommands((r) => {
          const next = new Set(r);
          next.delete(key);
          return next;
        }));
    },
    deny: (eventId: string, index: number, sender: string) => {
      if (!current) return;
      setError(null);
      const to = members.some((m) => m.id === sender) ? [sender] : [];
      api.send(current, deniedText(senderName(sender, agents), eventId, index), to, 1).catch((e) => setError(String(e)));
    },
  }), [ranCommands, runningCommands, current, members, agents]);
  const answered = useMemo(() => answeredRequests(events), [events]);
  const unanswered = useMemo(() => pendingRequests(events, answered, requestFloor), [events, answered, requestFloor]);
  const typedActivity = TYPED_THEMES.has(prefs.theme);

  // Scroll to the oldest request still waiting for Approve or Deny, and flash it.
  function jumpToRequest() {
    const el = unanswered.length ? document.getElementById(requestElementId(unanswered[0])) : null;
    if (!el) return;
    el.scrollIntoView({ behavior: "smooth", block: "center" });
    el.classList.remove("request-flash");
    void el.offsetWidth;
    el.classList.add("request-flash");
    window.setTimeout(() => el.classList.remove("request-flash"), 1800);
  }

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
  // Discuss on. Native shells are available; administrator/root elevation remains per-command.
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
          "They can run native shell commands, install software and change files there without asking first. " +
          "Administrator/root elevation still requires a fresh REQUEST for one exact command and the normal OS prompt.\n\nContinue?",
      );
      if (!ok) return;
      await api.setDefaultWorkspace(folder);
      await api.setDefaultAccess("unrestricted");
      setTargets(members.map((m) => m.id));
      setDiscuss(true);
      setRounds((r) => Math.max(r, 4));
      refreshAgents();
      setToast(
        `Full capacity on: every AI is Unrestricted. This PC will run at most ${pressure?.recommended_parallel_agents ?? 2} ` +
          `AI${(pressure?.recommended_parallel_agents ?? 2) === 1 ? "" : "s"} at once.`,
      );
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
    { id: "repo", name: "Inspect a GitHub repo", hint: "Sandboxed clone and report: languages, licence, build files, risks", run: () => setRepoOpen(true) },
    { id: "audit", name: "Open Audit Log", hint: "Approvals, requests, updates and errors, newest first", run: () => setAuditOpen(true) },
    { id: "usage", name: "Show Token Usage", hint: "Conversation & daily totals", run: () => openUsage() },
    { id: "self-check", name: "Run Self-Check", hint: "Probe all active council models", run: () => runSelfCheck() },
    { id: "manage-ais", name: "Add / Remove AIs", hint: "Council membership", run: () => setShowAddAi(true) },
    { id: "link-sync", name: "Multi-Computer Sync", hint: "Pair and sync with Mac or Windows PC over Gigabit / Wi-Fi", run: () => setShowSettings(true) },
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
    { id: "theme-consilium", name: "Theme: Consilium", hint: "Indigo and ivory", run: () => setPrefs((p) => ({ ...p, theme: "consilium" })) },
    { id: "theme-cyber", name: "Theme: Cyber", hint: "Cyberpunk blue-teal", run: () => setPrefs((p) => ({ ...p, theme: "cyber" })) },
    { id: "theme-slate", name: "Theme: Slate", hint: "Clean slate dark", run: () => setPrefs((p) => ({ ...p, theme: "slate" })) },
    { id: "theme-crt", name: "Theme: CRT", hint: "Green phosphor terminal", run: () => setPrefs((p) => ({ ...p, theme: "crt" })) },
    { id: "theme-retro84", name: "Theme: Retro '84", hint: "Neon grid, scanlines, glow", run: () => setPrefs((p) => ({ ...p, theme: "retro84" })) },
    { id: "theme-netrunner", name: "Theme: Netrunner", hint: "Black, neon green, cyberpunk", run: () => setPrefs((p) => ({ ...p, theme: "netrunner" })) },
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

      <main
        className={`main ${wallpaper && prefs.bingWallpaper ? "has-wallpaper" : ""}`}
        style={wallpaper && prefs.bingWallpaper ? ({ "--bing-wallpaper": `url(${wallpaper.data_url})` } as React.CSSProperties) : undefined}
      >
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
            <div className="workspace">
              {meta?.workspace ? (
                <>
                  <span className="folder" title={meta.workspace}>
                    <b>{folderName(meta.workspace)}</b>
                  </span>
                  <button title="Pick a different project folder for this conversation" className="link" onClick={chooseFolder}>change</button>
                  <button title="Stop using a project folder; the AIs only chat" className="link" onClick={clearFolder}>remove</button>
                </>
              ) : council?.default_workspace ? (
                <>
                  <span className="folder" title={council.default_workspace}>
                    <b>{folderName(council.default_workspace)}</b> (default)
                  </span>
                  <button title="Pick a different project folder for this conversation" className="link" onClick={chooseFolder}>use another folder</button>
                </>
              ) : (
                <button className="link" onClick={chooseFolder} title="Let the AIs work in a folder, according to each one's access">
                  + Add project folder
                </button>
              )}
              <ProjectVersions workspace={meta?.workspace ?? council?.default_workspace ?? null} />
            </div>
          </div>
          <div className="topbar-actions">
            <div className="topbar-status">
              <MessagingIndicator />
              <LinkIndicator />
              {wallpaper && prefs.bingWallpaper && wallpaper.title && (
                <span
                  className="wallpaper-pill"
                  title={wallpaper.copyright ? `${wallpaper.title} (${wallpaper.copyright})` : wallpaper.title}
                >
                  🖼️
                </span>
              )}
              <button
                className="link hover-faint palette-key"
                onClick={() => {
                  setPaletteQuery("");
                  setPaletteIndex(0);
                  setShowPalette(true);
                }}
                title={`Open command palette (${paletteKeyWords})`}
              >
                {paletteKey}
              </button>
            </div>
            <button
              title="Ask every AI for a concise report of completed work, current work, blockers, uncommitted changes and next step"
              className="btn ghost status-request compact-action"
              onClick={requestStatus}
              disabled={!current || !members.length || requestingStatus}
              aria-label={requestingStatus ? "Requesting status from every AI" : "Request status from every AI"}
            >
              <span className="topbar-action-icon" aria-hidden="true">{requestingStatus ? "…" : "↻"}</span>
              <span className="topbar-action-label">{requestingStatus ? "Requesting…" : "Status"}</span>
            </button>
            <button
              title="Open the audit log: every approval, request, update, council change, phone message and error, newest first"
              className={`btn ghost compact-action ${auditOpen ? "on" : ""}`}
              onClick={() => setAuditOpen((o) => !o)}
              disabled={!current}
              aria-label="Open audit log"
            >
              <span className="topbar-action-icon" aria-hidden="true">≡</span>
              <span className="topbar-action-label">Audit</span>
            </button>
            <button
              title="Inspect a GitHub repo: a sandboxed copy that runs none of its code, then a report"
              className={`btn ghost compact-action ${repoOpen ? "on" : ""}`}
              onClick={() => setRepoOpen((o) => !o)}
              aria-label="Inspect a GitHub repository"
            >
              <span className="topbar-action-icon repo-icon" aria-hidden="true">&lt;/&gt;</span>
              <span className="topbar-action-label">Repos</span>
            </button>
            <button
              title="Copy the whole conversation to the clipboard as Markdown"
              className="btn ghost compact-action"
              onClick={exportCurrent}
              disabled={!events.length}
              aria-label="Copy conversation as Markdown"
            >
              <span className="topbar-action-icon" aria-hidden="true">⧉</span>
              <span className="topbar-action-label">Copy</span>
            </button>
            <button
              title="Delete this conversation permanently"
              className="btn ghost danger compact-action"
              onClick={deleteCurrent}
              aria-label="Delete conversation permanently"
            >
              <span className="topbar-action-icon" aria-hidden="true">⌫</span>
              <span className="topbar-action-label">Delete</span>
            </button>
          </div>
        </header>

        {auditOpen && <AuditDrawer events={events} names={agentNames} onClose={() => setAuditOpen(false)} />}
        {repoOpen && (
          <RepoDrawer
            onClose={() => setRepoOpen(false)}
            onAsk={(text) => {
              setDraft(text);
              setRepoOpen(false);
            }}
          />
        )}
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
                <MessageBody event={e} answered={answered} deciding={decidingRequests} decide={e.sender === "human" ? undefined : decide} respond={e.sender === "human" ? undefined : respondToRequest} commands={commandActions} />
                {e.sender === "human" && e.id === latestHumanMessage?.id && busy && (
                  <TaskOwnership ids={thinkingIds} objective={taskObjective(e.text)} agents={agents} />
                )}
                <ClaimedTask event={e} agents={agents} />
                <SeenLine receipts={seenByMessage.get(e.id) ?? []} agents={agents} />
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
            ) : e.kind === "seen" ? null : e.kind === "error" && e.sender !== "human" && /^\s*REQUEST:\s*\S/im.test(e.text) ? (
              // A blocked turn asking for approval (e.g. one elevated command) gets the same buttons as a message.
              <div key={e.id} className={`note note-${e.kind}`}>
                <MessageBody event={e} answered={answered} deciding={decidingRequests} decide={decide} respond={respondToRequest} commands={commandActions} />
              </div>
            ) : (
              <div key={e.id} className={`note note-${e.kind}`}>
                {e.text}
              </div>
            ),
          )}
          {busy && (
            <div className="thinking">
              <span className="pulse" /> <TypedText text={thinking ?? "Working"} enabled={typedActivity} />
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
            <button
              type="button"
              className="btn small respond-request"
              disabled={unanswered.length === 0}
              onClick={jumpToRequest}
              title={unanswered.length ? `Scroll to the oldest request still waiting for a response${unanswered.length > 1 ? ` (${unanswered.length} waiting)` : ""}` : "No unanswered requests"}
            >
              Respond to request{unanswered.length > 0 ? ` · ${unanswered.length}` : ""}
            </button>
            <button
              type="button"
              className="btn small request-update"
              disabled={updating || requestingUpdate || (!staged && !stagedUi && (!current || !members.length))}
              onClick={requestUpdate}
              title={
                staged
                  ? `Install ${staged.version} now (pauses the AIs and restarts; their work resumes after)`
                  : stagedUi
                    ? "Load the staged screen update in place, with no restart"
                    : "Nothing is staged: ask the AIs to find, build and stage the next update"
              }
            >
              {requestingUpdate ? "Requesting…" : "Request App Update"}
            </button>
            {stagedUi && !staged && (
              <button
                className="btn small update-now"
                disabled={updating}
                onClick={applyUi}
                title="Load the new screen in place, with no restart. The AIs keep working; if the new screen doesn't start, the previous one comes back by itself."
              >
                {updating ? "Applying…" : "Apply Now · instant"}
              </button>
            )}
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
              {!speaking && <VoiceVisualizer speaker="user" active={voice} />}
              {speaking ? (
                <>
                  <span>Reading {speakingAgent ? `${senderName(speakingAgent, agents)}’s` : "the"} reply aloud…</span>
                  <button title="Stop reading this reply aloud and move on to the next" className="btn small" onClick={() => speakerRef.current?.skip()}>
                    Skip
                  </button>
                </>
              ) : canListen ? (
                <span className="voice-heard">{heard || "Listening… speak, and your message is sent when you pause."}</span>
              ) : (
                <span>
                  Speech recognition isn't available in this window. {dictationHint}, then Enter to send.
                </span>
              )}
              {!canSpeak && <span className="voice-warn">Read-aloud isn't available on this system.</span>}
              <div className="voice-agents" aria-label="Each AI's voice and status" aria-live="polite">
                {members.map((a) => {
                  const isWorking = busy && thinkingIds.includes(a.id);
                  const activity = isWorking ? activities[a.id] : undefined;
                  const lastSignal = activity?.updated_at ?? turnStartTime;
                  const load = pressure?.agents?.[a.id];
                  const stalled = Boolean(isWorking && lastSignal && Date.now() - lastSignal > 25_000 && load !== undefined && load < 1);
                  const queuedAt = speakQueue.indexOf(a.id);
                  const [mode, status] =
                    speakingAgent === a.id
                      ? (["speaking", "Speaking"] as const)
                      : stalled
                      ? (["stalled", "Stalled"] as const)
                      : isWorking
                      ? (["working", activity ? activity.label : "Initializing…"] as const)
                      : queuedAt >= 0
                      ? (["queued", queuedAt === 0 ? "Up next" : `Speaks ${ordinal(queuedAt + 2)}`] as const)
                      : readyIds.has(a.id)
                      ? (["idle", "Ready"] as const)
                      : (["off", "Unavailable"] as const);
                  return (
                    <AgentVoiceMeter
                      key={a.id}
                      id={a.id}
                      name={a.name}
                      mode={mode}
                      status={status}
                      detail={activity?.detail}
                    />
                  );
                })}
              </div>
            </div>
          )}
          <PersonaChips value={persona} onChange={setPersona} />
          <div className="composer-row">
            <DraftBox
              ref={draftBox}
              initial={IN_APP ? "" : demoDraft}
              onEmptyChange={setDraftEmpty}
              onSend={() => sendRef.current()}
              onPasteFiles={(files) => attachPastedRef.current(files)}
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
                disabled={draftEmpty && !pending.length}
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
              Visual effects (scanlines, glow and animation in neon themes)
            </label>
            <label className="toggle">
              <input
                type="checkbox"
                checked={prefs.bingWallpaper}
                onChange={(e) => setPrefs({ ...prefs, bingWallpaper: e.target.checked })}
              />
              Bing daily wallpaper (with dark frosted glass overlay)
            </label>
            {prefs.bingWallpaper && (
              <div className="wallpaper-actions">
                <button
                  type="button"
                  className="btn small-btn"
                  title="Download and apply today's wallpaper from Bing automatically"
                  onClick={() => {
                    setWallpaper(null);
                    api.getDailyWallpaper().then(setWallpaper).catch(() => undefined);
                  }}
                >
                  🔄 Download & Apply Today's Wallpaper
                </button>
                {wallpaper && (
                  <span className="muted small">
                    {wallpaper.title ? `Active: ${wallpaper.title}` : `Active: ${wallpaper.date}`}
                  </span>
                )}
              </div>
            )}
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
            <div className="section-label">Messaging</div>
            <MessagingSettings />
            <div className="section-label">Voices</div>
            <VoiceSettings agents={members.length ? members : agents} />
            <div className="section-label">Multi-Computer Sync (Mac &amp; Windows)</div>
            <LinkSettings />
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
