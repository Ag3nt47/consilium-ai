import { TypedText, TYPED_THEMES } from "./typed";
import { clock, clockShort, day, num } from "./fmt";
import { commandElementId, fencedBlocks, handledCommands, needsAdmin, pendingCommands, runsOn, shellName, shellSystem, deniedText, type Fence } from "./commands";
import { OS, dictationHint, paletteKey, paletteKeyWords, terminalName } from "./platform";
import { forwardRef, lazy, memo, Suspense, useCallback, useEffect, useImperativeHandle, useLayoutEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { openUrl } from "@tauri-apps/plugin-opener";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import {
  ACCESS_LEVELS,
  api,
  FORECAST_SITE,
  CLAUDE_RESETS_SITE,
  IN_APP,
  onActivity,
  onEvent,
  onTurn,
  onQuickSummon,
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
  type ClaudeResetHistory,
  type SystemPressure,
  type StagedUpdate,
  type TokenUsageReport,
  type TurnState,
  type UpdateDownloadStatus,
  type UsageWindow,
  type ModelChoice,
} from "./api";
import { canListen, canSpeak, isEcho, Listener, parseVoiceCommand, RESUME_AFTER_MS, Speaker, startsInterrupt, voiceOn } from "./voice";
import { VoiceSettings } from "./voicesettings";
import { AgentVoiceMeter, VoiceVisualizer } from "./visualizer";
import { checkForUpdate, nextUpdateCheckDelay, RELEASES_URL, type UpdateInfo } from "./updates";
import { installHoverTips } from "./tooltip";
import { AuditDrawer, loadPersona, PersonaChips, savePersona, withPersona } from "./qol";
import { MessagingIndicator, MessagingSettings } from "./messaging";
import { LinkIndicator, LinkSettings } from "./link";
import type { BackdropMode, BackdropPrefs } from "./TelemetryBackdrop";
import { SecurityScanSettings } from "./security";
import { ToolsSettings } from "./tools";
import { CiBadge, ProjectVersions } from "./projects";
import { TaskBoardPanel } from "./tasks";
import { RepoDrawer } from "./repo";
import { BranchesDrawer } from "./branches";
import { CustomAiForm } from "./customai";
import { demoAddAi, demoDraft, demoSettings, demoTheme, demoTurn } from "./demo";

const UpgradeProgress = lazy(() => import("./UpgradeProgress"));
const OfflineSkills = lazy(() => import("./OfflineSkills"));
const EnvironmentAudit = lazy(() => import("./EnvironmentAudit"));

const THEMES = [
  { id: "glass", name: "visionOS Glass", note: "Frosted translucent glass, ambient glow" },
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

const knownTheme = (id: string | null | undefined) => THEMES.some((t) => t.id === id);
const TelemetryBackdrop = lazy(() => import("./TelemetryBackdrop"));
const BackdropSettings = lazy(() => import("./TelemetryBackdrop").then((module) => ({ default: module.BackdropSettings })));
const backdropModes = { rain: "Matrix code rain", static: "Static glass", stealth: "Stealth solid" };
const defaultBackdrop: BackdropPrefs = { enabled: false, mode: "rain", filter: "all", opacity: 22, speed: 100, fps: 30 };
const nextBackdrop = (mode: BackdropMode): BackdropMode => mode === "rain" ? "static" : mode === "static" ? "stealth" : "rain";

type Prefs = { theme: string; effects: boolean; autoUpdate: boolean; bingWallpaper: boolean; dailyTokenWarning: number; backdrop: BackdropPrefs };
const PREFS_KEY = "consilium.prefs";
const REQUEST_FLOOR_KEY = "consilium.request-floor.v2";

function loadPrefs(): Prefs {
  // New installs start in Netrunner; anyone who has already picked a theme keeps it.
  const defaults: Prefs = { theme: "netrunner", effects: true, autoUpdate: true, bingWallpaper: true, dailyTokenWarning: 0, backdrop: defaultBackdrop };
  // Demo mode (README screenshots) uses Netrunner unless ?theme= says otherwise.
  if (!IN_APP) return { ...defaults, theme: knownTheme(demoTheme) ? demoTheme! : "netrunner", autoUpdate: false };
  try {
    const stored = JSON.parse(localStorage.getItem(PREFS_KEY) || "{}");
    const saved: Prefs = { ...defaults, ...stored, backdrop: { ...defaultBackdrop, ...stored.backdrop } };
    // Themes that were removed (Consilium, Cyber, Slate, CRT) fall back to Netrunner.
    return knownTheme(saved.theme) ? saved : { ...saved, theme: defaults.theme };
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
  return clock(ts);
}

function ConversationTitle({ title, rename }: { title: string; rename: (title: string) => void }) {
  const [draft, setDraft] = useState<{ original: string; value: string } | null>(null);
  const button = useRef<HTMLButtonElement>(null);
  const returnFocus = useRef(false);
  const editing = draft !== null;

  useEffect(() => {
    if (!editing && returnFocus.current) {
      returnFocus.current = false;
      button.current?.focus();
    }
  }, [editing]);

  return draft !== null ? (
    <input
      className="title-edit"
      autoFocus
      value={draft.value}
      maxLength={60}
      aria-label="Conversation title"
      onChange={(event) => setDraft({ ...draft, value: event.target.value })}
      onFocus={(event) => event.target.select()}
      onBlur={(event) => {
        const next = event.currentTarget.value.trim();
        setDraft(null);
        if (next && next !== draft.original.trim() && next !== title) rename(next);
      }}
      onKeyDown={(event) => {
        if (event.nativeEvent.isComposing || (event.key !== "Enter" && event.key !== "Escape")) return;
        event.preventDefault();
        returnFocus.current = true;
        if (event.key === "Enter") event.currentTarget.blur();
        else setDraft(null);
      }}
    />
  ) : (
    <h1>
      <button type="button" className="title-rename" ref={button} onClick={() => setDraft({ original: title, value: title })}
        title="Rename this conversation" aria-label={`Rename conversation: ${title}`}>
        {title}<span className="title-pencil" aria-hidden>✎</span>
      </button>
    </h1>
  );
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
  return num(n);
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
/** Requests and command boxes waiting longer than this leave the request tray (they stay in the chat). */
const STALE_AFTER_MS = 24 * 60 * 60 * 1000;

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
type RequestResolution = "approved" | "denied" | "responded" | "withdrawn";

/** A request's wording, for matching a WITHDRAW line to it: case, spacing and end punctuation aside. */
function requestWords(text: string): string {
  return text.toLowerCase().replace(/\s+/g, " ").trim().replace(/[.?!:;,]+$/, "");
}

/** Whether `withdrawal` (the text after WITHDRAW:) names `request`: the same words, or the start of them. */
function withdraws(withdrawal: string, request: string): boolean {
  const [w, r] = [requestWords(withdrawal), requestWords(request)];
  if (w === "all" || w === "all requests" || w === r) return true;
  return w.length >= 12 && (r.startsWith(w) || w.startsWith(r));
}

function answeredRequests(events: ChatEvent[]): Map<string, RequestResolution> {
  const out = new Map<string, RequestResolution>();
  const waiting = new Map<string, string[]>();
  // Who asked each waiting request (sender and computer), so an AI only withdraws its own.
  const asker = new Map<string, string>();
  for (const e of events) {
    // In an auto-run chat, Consilium approves waiting requests on the human's behalf, one
    // "Approved: <request>" line each in an auto-continue note (src-tauri/src/autocontinue.rs).
    if (e.sender === "consilium" && e.text.startsWith("Auto-continue")) {
      for (const line of e.text.split("\n")) {
        const request = /^Approved:\s*(.+)$/.exec(line.trim())?.[1].trim();
        const key = request ? waiting.get(request)?.shift() : undefined;
        if (!key) continue;
        out.set(key, "approved");
        if (!waiting.get(request!)?.length) waiting.delete(request!);
      }
      continue;
    }
    if (e.kind !== "message" && e.kind !== "error") continue;
    if (e.sender !== "human") {
      const me = `${e.sender}|${e.via ?? ""}`;
      for (const [line, text] of e.text.split("\n").entries()) {
        const withdrawal = /^\s*WITHDRAW:\s*(.+)$/i.exec(text)?.[1].trim();
        if (withdrawal) {
          for (const [request, keys] of [...waiting]) {
            if (!withdraws(withdrawal, request)) continue;
            const left = keys.filter((k) => {
              if (asker.get(k) !== me) return true;
              out.set(k, "withdrawn");
              return false;
            });
            if (left.length) waiting.set(request, left);
            else waiting.delete(request);
          }
          continue;
        }
        const request = /^\s*REQUEST:\s*(.+)$/i.exec(text)?.[1].trim();
        if (!request) continue;
        const key = requestKey(e.id, line);
        asker.set(key, me);
        waiting.set(request, [...(waiting.get(request) ?? []), key]);
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

type Decide = (key: string, request: string, requester: string, approved: boolean, always?: boolean) => void;
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
                  onAlways={hasExactCommand(m[1]) ? () => decide(key, m[1], event.sender, true, true) : undefined}
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
  /** Whether "Always run" can be offered for this AI's boxes (it has Full capacity). */
  canAlwaysRun: (sender: string) => boolean;
  /** Runs this box and turns "Always run" on, so later boxes run without a click. */
  alwaysRun: (eventId: string, index: number) => void;
  chatAutoRun?: boolean;
  autoRunChat?: (eventId: string, index: number) => void;
};

/** A fenced code block. Shell blocks from an AI get Run and Deny; every block gets Copy. */
/** A small â†“ in the corner of a box whose content scrolls; jumps to its end, hides once there. */
function ScrollEndButton({ target }: { target: { current: HTMLElement | null } }) {
  const [show, setShow] = useState(false);
  useEffect(() => {
    const el = target.current;
    if (!el) return;
    const update = () => setShow(el.scrollHeight - el.scrollTop - el.clientHeight > 24);
    update();
    el.addEventListener("scroll", update, { passive: true });
    const ro = new ResizeObserver(update);
    ro.observe(el);
    return () => {
      el.removeEventListener("scroll", update);
      ro.disconnect();
    };
  }, [target]);
  if (!show) return null;
  return (
    <button
      type="button"
      className="scroll-end-btn"
      onClick={() => target.current?.scrollTo({ top: target.current.scrollHeight, behavior: "smooth" })}
      title="Jump to the end of this box"
      aria-label="Jump to the end of this box"
    >
      {"\u2193"}
    </button>
  );
}

function CommandBox({ event, fence, index, actions }: { event: ChatEvent; fence: Fence; index: number; actions?: CommandActions }) {
  const [copied, setCopied] = useState(false);
  const preRef = useRef<HTMLPreElement>(null);
  const shell = shellName(fence.lang);
  // Only a box written for this computer, by an AI on this computer, can run here; any other box
  // says where it belongs and keeps Copy.
  const elsewhere = shell ? (event.via && event.sender !== "human" ? event.via : !runsOn(fence.lang, OS) ? shellSystem(fence.lang) : null) : null;
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
    <div id={commandElementId(key)} className={`command-box${shell ? " shell" : ""}`}>
      <div className="command-head">
        <span className="command-lang">
          {shell ?? (fence.lang || "code")}
          {shell && needsAdmin(fence.code) ? " · admin" : ""}
          {elsewhere ? ` · for ${elsewhere}` : ""}
        </span>
        <span className="command-actions">
          {shell && !elsewhere && actions && (state ? (
            <span className={`request-answer ${state === "ran" ? "approved" : "denied"}`}>{state === "ran" ? "✓ Ran" : "✕ Denied"}</span>
          ) : (
            <>
              <button type="button" className="btn small-btn approve" disabled={running} onClick={() => actions.run(event.id, index)}
                title={needsAdmin(fence.code) ? "Run this now. Your computer asks for your password itself; Consilium never sees it. The output goes back to the AI that suggested it." : "Run this now in the project folder. The output goes back to the AI that suggested it."}>
                {running ? "Running…" : "Run"}
              </button>
              {actions.canAlwaysRun(event.sender) && !needsAdmin(fence.code) && (
                <button type="button" className="btn small-btn approve always" disabled={running} onClick={() => actions.alwaysRun(event.id, index)}
                  title="Run this now, and from now on run command boxes from Full-capacity AIs on this computer without asking. Admin commands and software installs still ask. Turn it off in Settings.">
                  Always run
                </button>
              )}
              {actions.autoRunChat && !actions.chatAutoRun && (
                <button type="button" className="btn small-btn autorun-chat" disabled={running} onClick={() => actions.autoRunChat?.(event.id, index)}
                  title="Run every command box in this chat without asking, after a warning. Only this chat; new chats start with it off.">
                  Auto-run this chat
                </button>
              )}
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
      <pre ref={preRef}><code>{fence.code}</code></pre>
      <ScrollEndButton target={preRef} />
    </div>
  );
}

/** True when a REQUEST names an exact admin or install command, which "Always allow" can save. */
function hasExactCommand(request: string): boolean {
  return /(ELEVATED|INSTALL)_COMMAND_JSON:\s*"/.test(request);
}

function RequestActions({ answer, pending, onDecide, onAlways, onRespond }: { answer?: RequestResolution; pending?: boolean; onDecide: (approved: boolean) => void; onAlways?: () => void; onRespond: (response: string) => Promise<void> }) {
  const [response, setResponse] = useState("");
  if (answer !== undefined) {
    const label =
      answer === "approved" ? "✓ Approved" : answer === "denied" ? "✕ Denied" : answer === "withdrawn" ? "– Withdrawn: no longer needed" : "↪ Responded";
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
      {onAlways && (
        <button title="Approve, and remember this exact command on this computer so no AI has to ask for it again. Remove it any time in Settings → Saved approvals." type="button" className="btn small-btn approve always" disabled={pending} onClick={onAlways} aria-label="Approve and always allow this exact command">
          Always allow
        </button>
      )}
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

type PendingItem =
  | { kind: "request"; key: string; event: ChatEvent; text: string }
  | { kind: "command"; key: string; event: ChatEvent; index: number; fence: Fence };

/** The waiting requests and commands, one at a time, pinned above the message box. */
function RequestTray({ items, index, agents, answered, deciding, decide, respond, commands, onFocus, onShow, autoRun }: {
  items: PendingItem[];
  index: number;
  agents: AgentInfo[];
  answered: Map<string, RequestResolution>;
  deciding: Set<string>;
  decide: Decide;
  respond: Respond;
  commands: CommandActions;
  onFocus: (index: number) => void;
  onShow: (item: PendingItem) => void;
  /** Auto-run is on (for every chat or this one): start minimized, since boxes run by themselves. */
  autoRun: boolean;
}) {
  const [collapsed, setCollapsed] = useState(autoRun);
  // Follow auto-run when it is switched on or off; the ▴/▾ button still overrides it until then.
  useEffect(() => setCollapsed(autoRun), [autoRun]);
  const item = items[index];
  if (!item) return null;
  const from = senderName(item.event.sender, agents);
  const many = items.length > 1;
  return (
    <div className={`request-tray${collapsed ? " collapsed" : ""}`} role="region" aria-label="Waiting requests">
      <div className="request-tray-head">
        <span className="request-tray-count" aria-live="polite">
          {items.length} waiting
        </span>
        <span className="request-tray-from">
          {item.kind === "request" ? "Request" : "Command"} {index + 1} of {items.length} · from {from}
        </span>
        <span className="spacer" />
        <button type="button" className="btn small-btn" disabled={!many} onClick={() => onFocus(index - 1)} title="Previous waiting request" aria-label="Previous request">‹</button>
        <button type="button" className="btn small-btn" disabled={!many} onClick={() => onFocus(index + 1)} title="Next waiting request" aria-label="Next request">›</button>
        <button type="button" className="btn small-btn" onClick={() => onShow(item)} title="Scroll the chat to this request so you can read the message around it">Show in chat</button>
        <button type="button" className="btn small-btn" onClick={() => setCollapsed((c) => !c)} title={collapsed ? "Show the request" : "Hide the request; the count stays"} aria-expanded={!collapsed}>
          {collapsed ? "▴" : "▾"}
        </button>
      </div>
      {!collapsed && item.kind === "request" && (
        <div className="request request-tray-body">
          <div className="request-tray-text"><span className="request-label">REQUEST:</span> <Inline text={item.text} /></div>
          <RequestActions
            key={item.key}
            answer={answered.get(item.key)}
            pending={deciding.has(item.key)}
            onDecide={(ok) => decide(item.key, item.text, item.event.sender, ok)}
            onAlways={hasExactCommand(item.text) ? () => decide(item.key, item.text, item.event.sender, true, true) : undefined}
            onRespond={(response) => respond(item.key, item.text, item.event.sender, response)}
          />
        </div>
      )}
      {!collapsed && item.kind === "command" && (
        <div className="request-tray-body command-tray">
          <pre><code>{item.fence.code}</code></pre>
          <span className="request-actions">
            <button type="button" className="btn small-btn approve" disabled={commands.running.has(item.key)} onClick={() => commands.run(item.event.id, item.index)}
              title={needsAdmin(item.fence.code) ? "Run this now. Your computer asks for your password itself; Consilium never sees it." : "Run this now in the project folder. The output goes back to the AI that suggested it."}>
              {commands.running.has(item.key) ? "Running…" : "Run"}
            </button>
            <button type="button" className="btn small-btn deny" disabled={commands.running.has(item.key)} onClick={() => commands.deny(item.event.id, item.index, item.event.sender)} title="Don't run it, and tell the AI that suggested it">
              Deny
            </button>
          </span>
        </div>
      )}
    </div>
  );
}

function agentState(a: AgentInfo): "ready" | "signin" | "missing" | "benched" {
  if (a.available && a.benched_until && a.benched_until > Date.now()) return "benched";
  return a.available ? "ready" : a.path ? "signin" : "missing";
}

/** Out of usage: benched by Consilium, or its whole-plan allowance reads 0%. A model at 0%
 *  whose plan still has room isn't exhausted, since the AI can switch model. */
function isExhausted(a: AgentInfo): boolean {
  if (agentState(a) === "benched") return true;
  const u = a.usage;
  if (!u) return false;
  return (u.overall?.remaining_percent ?? u.remaining_percent) <= 0;
}

function attachmentIcon(kind: string): string {
  return kind === "image" ? "🖼" : kind === "pdf" ? "📕" : kind === "text" ? "📄" : "📎";
}

function fileSize(bytes: number): string {
  return bytes >= 1048576 ? `${(bytes / 1048576).toFixed(1)} MB` : bytes >= 1024 ? `${Math.round(bytes / 1024)} KB` : `${bytes} B`;
}

/** When this bar's usage limit resets. If the bar is at 0% (out of usage), it only resets when
 * all exhausted windows (0% left) reset, so it picks the latest verified reset among the exhausted windows.
 * If usage remains (> 0%), it shows the next window that replenishes (< 100% left). */
function nextReset(windows: UsageWindow[], remainingPercent?: number): number | null {
  const now = Date.now();
  if (remainingPercent === 0) {
    const exhausted = windows.filter((w) => (w.remaining_percent ?? 0) === 0);
    const targets = exhausted.length > 0 ? exhausted : windows;
    const resets = targets.map((w) => w.resets_at ?? 0).filter((t) => t > now);
    return resets.length > 0 ? Math.max(...resets) : null;
  }
  const used = windows.filter((w) => (w.remaining_percent ?? 100) < 100);
  const targets = used.length > 0 ? used : windows;
  const ahead = targets.map((w) => w.resets_at ?? 0).filter((t) => t > now);
  return ahead.length ? Math.min(...ahead) : null;
}

function UsageBar({ label, percent, windows, aria, note }: { label: string; percent: number; windows: UsageWindow[]; aria: string; note?: string }) {
  const remaining = Math.max(0, Math.min(100, Math.round(percent)));
  const color = `hsl(${remaining * 1.2} 72% 46%)`;
  const reset = nextReset(windows, remaining);
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

function usagePeriod(window: UsageWindow): "session" | "weekly" | "other" {
  const label = window.label.toLowerCase().replace(/[-_]+/g, " ").trim();
  if (/^(?:(?:current )?session|(?:5|five) hours?|5h)(?:$|\s)/.test(label)) return "session";
  if (/^(?:weekly|(?:7|seven) days?)(?:$|\s)/.test(label)) return "weekly";
  return "other";
}

function UsagePeriod({ agent, label, windows }: { agent: AgentInfo; label: string; windows: UsageWindow[] }) {
  return windows.length ? (
    <UsageBar label={label} percent={Math.min(...windows.map((window) => window.remaining_percent))}
      windows={windows} aria={`${agent.name} ${label.toLowerCase()} usage remaining`} />
  ) : (
    <div className="usage-row" title={`${agent.tool} did not report this usage window`}>
      <div className="usage-label">
        <span className="usage-name">{label}</span>
        <span className="usage-unavailable">Unavailable</span>
      </div>
    </div>
  );
}

function UsageMeter({ agent, forecast, claudeResets }: { agent: AgentInfo; forecast?: ResetForecast | null; claudeResets?: ClaudeResetHistory | null }) {
  const usage = agent.usage;
  const model = usage?.model;
  const plan = usage?.overall?.windows ?? (model ? [] : usage?.windows ?? []);
  return (
    <div className="usage-meter" title="How much of this AI's plan allowance is left, and when each limit resets">
      <UsagePeriod agent={agent} label="Current session" windows={plan.filter((window) => usagePeriod(window) === "session")} />
      <UsagePeriod agent={agent} label="Overall weekly" windows={plan.filter((window) => usagePeriod(window) === "weekly")} />
      {plan.filter((window) => usagePeriod(window) === "other").map((window, index) => (
        <UsageBar key={`${window.label}-${index}`} label={window.label} percent={window.remaining_percent}
          windows={[window]} aria={`${agent.name} ${window.label} usage remaining`} />
      ))}
      {model && !model.shared && (
        <UsageBar
          label={`${model.label} · model limit`}
          percent={model.remaining_percent}
          windows={model.windows}
          aria={`${model.label} usage remaining`}
        />
      )}
      {forecast && (
        <div className="usage-forecast" title={`Unofficial estimate${forecast.confidence ? `, ${forecast.confidence} confidence` : ""}. Not affiliated with OpenAI.`}>
          {forecast.official
            ? `Reset announced: ${forecast.official}`
            : `Global reset: ${forecast.chance_24h ?? "?"}% chance in 24 h${forecast.chance_48h != null ? ` (${forecast.chance_48h}% in 48 h)` : ""}`}
          {" · "}
          <button title="Open codex-reset.com, the unofficial source of the reset forecast" className="link" onClick={() => openUrl(FORECAST_SITE)}>
            data: codex-reset.com
          </button>
        </div>
      )}
      {claudeResets && claudeResets.last_reset_at != null && (
        <div
          className="usage-forecast"
          title={`Claude Code resets announced by Anthropic${claudeResets.reset_count != null ? ` (${claudeResets.reset_count} so far)` : ""}. History only, not a forecast. Data: claude-resets.com, CC BY 4.0. Not affiliated with Anthropic.`}
        >
          {`Last reset ${((Date.now() - claudeResets.last_reset_at) / 86_400_000).toFixed(1)} days ago`}
          {claudeResets.mean_gap_days != null ? ` \u00b7 avg gap ${claudeResets.mean_gap_days.toFixed(1)} days` : ""}
          {" \u00b7 "}
          <button title="Open claude-resets.com, the unofficial source of the Claude reset history" className="link" onClick={() => openUrl(CLAUDE_RESETS_SITE)}>
            data: claude-resets.com
          </button>
        </div>
      )}
    </div>
  );
}

/** "3:40 PM", or "Mon 3:40 PM" when it isn't today. */
function resetTime(ts: number): string {
  const d = new Date(ts);
  const time = clockShort(d);
  return d.toDateString() === new Date().toDateString() ? time : `${day(d)} ${time}`;
}

function AgentDot({
  agent,
  onSetup,
  onRetry,
  check,
  forecast,
  claudeResets,
  isWorking,
  isFrozen,
  load,
  controls,
}: {
  agent: AgentInfo;
  onSetup: (a: AgentInfo) => void;
  onRetry: (a: AgentInfo) => void;
  check?: CheckResult | "running";
  forecast?: ResetForecast | null;
  claudeResets?: ClaudeResetHistory | null;
  isWorking?: boolean;
  isFrozen?: boolean;
  /** Share of the whole PC (0-100) this AI's processes are using right now; undefined when none run. */
  load?: number;
  /** Stop/start, model and access buttons, drawn inside the card's right edge. */
  controls?: React.ReactNode;
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
      className={`agent agent-${agent.id}${isExhausted(agent) ? " agent-exhausted" : ""}${controls ? " has-controls" : ""}`}
      title={
        isFrozen
          ? "Stalled: no provider activity for 25 seconds"
          : isWorking
              ? "Generating response…"
              : agent.hint || `${agent.tool} ${agent.version ?? ""}`
      }
    >
      <span className={`dot ${dotClass}`} />
      <span className="agent-name">
        {agent.name}
        {isExhausted(agent) && (
          <em className="agent-exhausted-tag" title="This AI has used all of its plan allowance. It sits out until its limit resets.">
            Exhausted
          </em>
        )}
        {agent.id === "gpt" && forecast && forecast.chance_24h != null && (
          <span
            className="forecast-pill"
            title={`codex-reset.com forecast: ${forecast.chance_24h}% chance in 24h, ${forecast.chance_48h ?? "?"}% in 48h (${forecast.confidence ?? "low"} confidence)`}
            onClick={(e) => {
              e.stopPropagation();
              openUrl(FORECAST_SITE);
            }}
          >
            ~{forecast.chance_24h}% reset
          </span>
        )}
      </span>
      {/* Always present, at a fixed width, so the badge appearing or its digits changing
          can't resize the name column and re-wrap the card. */}
      <span
        className="agent-load"
        style={load !== undefined ? { color: pressureColor(load) } : { visibility: "hidden" }}
        title={load !== undefined ? `${agent.name}'s processes are using ${load}% of this PC (the busier of its CPU and memory share)` : undefined}
        aria-hidden={load === undefined}
      >
        {load !== undefined ? `▣ ${load}%` : ""}
      </span>
      {isFrozen ? (
        <span className="agent-state" style={{ color: "var(--danger)" }} title="No output for a while; it may be stuck. Stop and resend if it doesn't recover.">Stalled</span>
      ) : isWorking ? (
        <span className="agent-state" style={{ color: "var(--accent)" }}>Working…</span>
      ) : state === "ready" ? (
        <span className="agent-state">{label}</span>
      ) : state === "benched" ? (
        <span className="agent-state benched" title="It ran out of usage. The council carries on without it until its limit resets.">
          {agent.reset_known && agent.benched_until
            ? <>Out of usage · back {resetTime(agent.benched_until)}</>
            : <>Out of usage</>}
          {agent.id === "gpt" && forecast && forecast.chance_24h != null && (
            <span className="forecast-chance" title={`codex-reset.com forecast: ${forecast.chance_24h}% chance in 24h, ${forecast.chance_48h ?? "?"}% in 48h`}>
              {" "}· {forecast.chance_24h}% reset chance
            </span>
          )}{" "}
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
      <UsageMeter agent={agent} forecast={agent.id === "gpt" ? forecast : null} claudeResets={agent.id === "claude" ? claudeResets : null} />
      <span className="agent-kind">
        {agent.tool}
        {agent.model && <span className="model-tag" style={{ color: "#3b82f6" }}> · {agent.model}</span>}
        {agent.access !== "chat" && <span className="access-tag"> · {ACCESS_LEVELS.find((l) => l.id === agent.access)?.name}</span>}
      </span>
      {controls && <div className="council-box-controls">{controls}</div>}
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

/** Sidebar fit: steps the sidebar through compactness levels (0 = full, 4 = council list scrolls)
 *  until PC pressure, Add AI and Settings fit on screen. Re-measures on resize and content change. */
const SIDEBAR_FIT_MAX = 4;
function useSidebarFit(ref: { current: HTMLElement | null }) {
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    let frame = 0;
    let relax = 0;
    let lastWidth = -1;
    let lastHeight = -1;
    const overflows = () => el.scrollHeight > el.clientHeight + 1;
    const level = () => Number(el.dataset.fit ?? 0) || 0;
    // Writes the level only when it changes: rewriting the same value still restyles the sidebar.
    const setLevel = (n: number) => {
      if (el.dataset.fit !== String(n)) el.dataset.fit = String(n);
    };
    // Full refit: the least compact level that fits. Used on real window resizes.
    const refit = () => {
      for (let n = 0; n <= SIDEBAR_FIT_MAX; n++) {
        setLevel(n);
        if (!overflows()) return;
      }
    };
    // Growing back, throttled: try one level less compact and keep it only if it fits. A full
    // refit here forced up to five layouts every 4 s for as long as usage meters and countdowns
    // kept ticking, which they always do.
    const loosen = () => {
      const now = level();
      if (now === 0) return;
      setLevel(now - 1);
      if (overflows()) setLevel(now);
    };
    // Content changes (usage polls, Working…, load badges) only ever make the sidebar more
    // compact straight away. Re-testing every lower level on each change made the council
    // cards flip between levels, showing and hiding meters several times a second.
    const tighten = () => {
      let n = level();
      while (overflows() && n < SIDEBAR_FIT_MAX) setLevel(++n);
    };
    // The sidebar's outer size, scrollbar included. A scrollbar appearing or going away changes
    // clientWidth but not this, so it no longer counts as a window resize that sets off a full
    // refit, which could flip the scrollbar back and keep the window busy.
    const size = () => [el.offsetWidth, el.offsetHeight] as const;
    const run = () => {
      frame = 0;
      const [w, h] = size();
      const resized = w !== lastWidth || h !== lastHeight;
      lastWidth = w;
      lastHeight = h;
      if (resized) refit(); else tighten();
    };
    const schedule = () => {
      if (!frame) frame = requestAnimationFrame(run);
      // At most one step back toward full size every 4 s, and none when already at full size.
      if (!relax && level() > 0) relax = window.setTimeout(() => { relax = 0; loosen(); }, 4000);
    };
    refit();
    [lastWidth, lastHeight] = size();
    const ro = new ResizeObserver(schedule);
    ro.observe(el);
    const mo = new MutationObserver(schedule);
    mo.observe(el, { childList: true, subtree: true, characterData: true });
    return () => {
      ro.disconnect(); mo.disconnect();
      if (frame) cancelAnimationFrame(frame);
      window.clearTimeout(relax);
    };
  }, [ref]);
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
type DraftHandle = { get(): string; set(text: string): void; focus(): void };
const DraftBox = memo(
  forwardRef<DraftHandle, { initial: string; onEmptyChange: (empty: boolean) => void; onSend: () => void; onPasteFiles: (files: File[]) => void }>(
    function DraftBox({ initial, onEmptyChange, onSend, onPasteFiles }, ref) {
      const [text, setText] = useState(initial);
      const textRef = useRef(text);
      const area = useRef<HTMLTextAreaElement>(null);
      const update = (v: string) => {
        if (!v.trim() !== !textRef.current.trim()) onEmptyChange(!v.trim());
        textRef.current = v;
        setText(v);
      };
      useImperativeHandle(ref, () => ({
        get: () => textRef.current,
        set: update,
        focus: () => {
          const el = area.current;
          if (!el) return;
          el.focus();
          // Caret at the end, ready to type under the quote.
          requestAnimationFrame(() => el.setSelectionRange(el.value.length, el.value.length));
        },
      }));
      return (
        <textarea
          ref={area}
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
            // Enter that finishes typing an accented or non-Latin character isn't a send.
            if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
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

// The AIs' last known status, shown the moment the window opens so the council doesn't start
// empty or "signed out" while the CLIs are asked again in the background.
const AGENTS_CACHE_KEY = "consilium.agents.v1";

function loadCachedAgents(): AgentInfo[] {
  try {
    const saved = JSON.parse(localStorage.getItem(AGENTS_CACHE_KEY) || "[]");
    return Array.isArray(saved) ? saved : [];
  } catch {
    return [];
  }
}

function saveCachedAgents(agents: AgentInfo[]) {
  try {
    localStorage.setItem(AGENTS_CACHE_KEY, JSON.stringify(agents));
  } catch {
    /* storage full or unavailable: the next start just checks from scratch */
  }
}

export default function App() {
  const [prefs, setPrefs] = useState<Prefs>(loadPrefs);
  const [meshPeers, setMeshPeers] = useState(0);
  const updateBackdrop = (change: Partial<BackdropPrefs>) =>
    setPrefs((current) => ({ ...current, backdrop: { ...current.backdrop, ...change } }));
  const cycleBackdrop = useCallback(() => setPrefs((current) => ({
    ...current, backdrop: { ...current.backdrop, mode: nextBackdrop(current.backdrop.mode) },
  })), []);
  useEffect(() => {
    if (!prefs.backdrop.enabled) return;
    const hotkey = (event: KeyboardEvent) => {
      if (!event.repeat && event.ctrlKey && event.shiftKey && !event.altKey && !event.metaKey && event.code === "KeyM") {
        event.preventDefault();
        cycleBackdrop();
      }
    };
    window.addEventListener("keydown", hotkey);
    return () => window.removeEventListener("keydown", hotkey);
  }, [prefs.backdrop.enabled, cycleBackdrop]);
  const [agents, setAgents] = useState<AgentInfo[]>(loadCachedAgents);
  const [conversations, setConversations] = useState<Conversation[]>([]);
  const conversationReads = useRef({ issued: 0, applied: 0, list: [] as Conversation[] });
  const [current, setCurrent] = useState<string | null>(null);
  const [events, setEvents] = useState<ChatEvent[]>([]);
  const [turn, setTurn] = useState<TurnState | null>(null);
  const [activities, setActivities] = useState<Record<string, AgentActivity>>({});
  const draftBox = useRef<DraftHandle>(null);
  const [draftEmpty, setDraftEmpty] = useState(!(IN_APP ? "" : demoDraft).trim());
  const setDraft = useCallback((text: string) => draftBox.current?.set(text), []);
  // The reply arrow on a message: addresses its sender (an AI on this computer) and quotes it in
  // the prompt box, keeping whatever was already typed below the quote.
  const replyTo = useCallback((e: ChatEvent) => {
    const box = draftBox.current;
    if (!box) return;
    const lines = e.text.trim().split(/\r?\n/).filter((l) => l.trim());
    let excerpt = lines.slice(0, 3).join("\n");
    if (excerpt.length > 240) excerpt = `${excerpt.slice(0, 240).trimEnd()}…`;
    else if (lines.length > 3) excerpt += " …";
    const quote = excerpt ? excerpt.split("\n").map((l) => `> ${l}`).join("\n") : "";
    const mention = e.sender !== "human" && !e.via ? `@${e.sender} ` : "";
    const existing = box.get().trim();
    box.set([quote, `${mention}${existing}`].filter(Boolean).join("\n"));
    box.focus();
  }, []);
  /** Files waiting to go with the next message. */
  const [pending, setPending] = useState<Attachment[]>([]);
  const [dropping, setDropping] = useState(false);
  const [targets, setTargets] = useState<string[]>(["gpt", "claude"]);
  const [discuss, setDiscuss] = useState(!IN_APP);
  const [rounds, setRounds] = useState(3);
  const [persona, setPersonaState] = useState(loadPersona);
  const [auditOpen, setAuditOpen] = useState(false);
  const [repoOpen, setRepoOpen] = useState(false);
  const [branchesOpen, setBranchesOpen] = useState(false);
  const [wallpaper, setWallpaper] = useState<DailyWallpaper | null>(null);
  const [wallpaperRefresh, setWallpaperRefresh] = useState(0);
  const setPersona = (id: string) => {
    setPersonaState(id);
    savePersona(id);
  };
  const agentNames = useMemo(() => Object.fromEntries(agents.map((a) => [a.id, a.name])), [agents]);
  const [error, setError] = useState<string | null>(null);
  const [showSettings, setShowSettings] = useState(!IN_APP && demoSettings);
  const [quickSummonAvailable, setQuickSummonAvailable] = useState(false);
  const [addPanel, setAddPanel] = useState<"choose" | "ai" | "device" | null>(!IN_APP && demoAddAi ? "ai" : null);
  const addTrigger = useRef<HTMLButtonElement>(null);
  const focusAddHeading = useCallback((heading: HTMLHeadingElement | null) => {
    heading?.focus({ preventScroll: true });
  }, []);
  function closeAddPanel() {
    setAddPanel(null);
    addTrigger.current?.focus();
  }
  const [showUsage, setShowUsage] = useState(false);
  const usageVisible = useRef(false);
  const usageRequest = useRef(0);
  const [budgetWarning, setBudgetWarning] = useState<{ id: string; text: string; message: string } | null>(null);
  const budgetCheck = useRef(false);
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
  // Voice interrupting: a reply pauses when the human starts talking over it, and carries on
  // after RESUME_AFTER_MS if nothing more is said. Spoken "Consilium, …" commands run here.
  const [interrupting, setInterrupting] = useState(false);
  const resumeTimer = useRef<number | undefined>(undefined);
  const voiceCommandRef = useRef<(text: string) => boolean>(() => false);
  const [openAppAsk, setOpenAppAsk] = useState<string | null>(null);
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
  const [claudeResets, setClaudeResets] = useState<ClaudeResetHistory | null>(null);
  const [pressure, setPressure] = useState<SystemPressure | null>(null);
  const [staged, setStaged] = useState<StagedUpdate | null>(null);
  const [downloadStatus, setDownloadStatus] = useState<UpdateDownloadStatus>({ active: false, version: "" });
  const [updating, setUpdating] = useState(false);
  const [stagedUi, setStagedUi] = useState<{ version: string } | null>(null);
  const [requestingStatus, setRequestingStatus] = useState(false);
  const [requestingUpdate, setRequestingUpdate] = useState(false);
  const [downloadError, setDownloadError] = useState("");
  const [requestingInstallers, setRequestingInstallers] = useState(false);
  const [installerRequest, setInstallerRequest] = useState<{ message: string; url?: string } | null>(null);

  async function buildInstallers() {
    setRequestingInstallers(true);
    setInstallerRequest(null);
    try {
      const url = await api.requestDraftInstallers();
      setInstallerRequest(url
        ? { message: "GitHub accepted the installer build request.", url }
        : { message: "GitHub accepted the workflow request. Open Installer builds to follow it." });
    } catch (error) {
      setInstallerRequest({ message: `Couldn't request installer builds: ${String(error)}` });
    } finally {
      setRequestingInstallers(false);
    }
  }
  const markDownloadStarting = useCallback((version: string) => {
    setDownloadError("");
    setDownloadStatus({ active: true, version: version.replace(/^v/i, "") });
  }, []);
  const markDownloadFailed = useCallback((error: unknown) => {
    setDownloadStatus({ active: false, version: "" });
    setDownloadError(`Couldn't prepare the GitHub update: ${String(error)}`);
  }, []);

  // Quick hover explanations for every control that has a title.
  useEffect(() => installHoverTips(), []);

  // Confirms this page started, so an instantly applied screen update isn't rolled back.
  useEffect(() => {
    api.uiLoaded().catch(() => undefined);
  }, []);

  // A staged build is a file check, so checking it is cheap. Check every few
  // seconds, whenever the window comes back into view, and right after any new
  // message, so the button appears as soon as an AI reports the update is ready.
  const pollStaged = useCallback(() => {
    api.updateAvailable().then(setStaged).catch(() => undefined);
    api.uiUpdateAvailable().then(setStagedUi).catch(() => undefined);
    // A linked computer is installing an update: look for the release now, so this one follows.
    api.releaseCheckRequested()
      .then((now) => (now ? checkForUpdate(true, markDownloadStarting, markDownloadFailed).then(setUpdate) : undefined))
      .catch(() => undefined);
  }, [markDownloadStarting, markDownloadFailed]);
  useEffect(() => {
    pollStaged();
    const t = setInterval(pollStaged, 4000);
    const onShow = () => {
      if (document.visibilityState === "visible") pollStaged();
    };
    window.addEventListener("focus", pollStaged);
    document.addEventListener("visibilitychange", onShow);
    return () => {
      clearInterval(t);
      window.removeEventListener("focus", pollStaged);
      document.removeEventListener("visibilitychange", onShow);
    };
  }, [pollStaged]);

  useEffect(() => {
    const poll = () => api.updateDownloadStatus().then(setDownloadStatus).catch(() => undefined);
    const t = window.setInterval(poll, downloadStatus.active ? 500 : 4000);
    return () => window.clearInterval(t);
  }, [downloadStatus.active]);

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
  // Follow new messages only while the human is at the bottom of the chat. Scrolling up to read
  // (or jumping to a request) pins the view until they scroll back down or press "New messages".
  const messagesRef = useRef<HTMLElement>(null);
  const followRef = useRef(true);
  const lastScrollTopRef = useRef(0);
  const seenCountRef = useRef(0);
  // Set when the human sends from this window, so only their own new message pulls the chat down.
  // Command output, "Approved:" lines and messages from a linked computer are also sent as the
  // human, and used to snap the view to the bottom while they were reading further up.
  const ownSendRef = useRef(false);
  const [unseen, setUnseen] = useState(0);

  currentRef.current = current;
  usageVisible.current = showUsage;

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
    if (!prefs.bingWallpaper) {
      setWallpaper(null);
      return;
    }
    let disposed = false;
    let pending = false;
    let lastAttempt = -Infinity;
    let lastDay = "";
    const refresh = () => {
      if (pending || document.visibilityState !== "visible") return;
      const now = new Date();
      const day = now.toDateString();
      const elapsed = now.getTime() - lastAttempt;
      if (day === lastDay && elapsed >= 0 && elapsed < 30 * 60_000) return;
      lastDay = day;
      lastAttempt = now.getTime();
      pending = true;
      api.getDailyWallpaper().then((next) => {
        if (!disposed && next) setWallpaper(next);
      }).catch(() => undefined).finally(() => { pending = false; });
    };
    refresh();
    const timer = window.setInterval(refresh, 60_000);
    window.addEventListener("focus", refresh);
    document.addEventListener("visibilitychange", refresh);
    return () => {
      disposed = true;
      window.clearInterval(timer);
      window.removeEventListener("focus", refresh);
      document.removeEventListener("visibilitychange", refresh);
    };
  }, [prefs.bingWallpaper, wallpaperRefresh]);

  const refreshAgents = useCallback(() => {
    // Focus, turn completion and the refresh button can all fire together. Share one request so
    // the provider CLIs are never probed more than once at a time, and update both snapshots as a
    // pair so access labels and council membership cannot briefly disagree.
    if (agentRefreshRef.current) return agentRefreshRef.current;
    setAgentsRefreshing(true);
    const request = Promise.all([api.agents(), api.getCouncil()])
      .then(([nextAgents, nextCouncil]) => {
        // Keep the last known usage on screen until the fresh reading arrives.
        setAgents((prev) => {
          const next = nextAgents.map((a) => {
            const old = prev.find((p) => p.id === a.id);
            // A check that couldn't answer (signed_in null: the CLI was slow or the keychain busy)
            // isn't a sign-out. Keep the last confirmed sign-in instead of asking to sign in again.
            const keep = a.path && a.signed_in == null && old?.signed_in === true;
            return {
              ...a,
              ...(keep ? { signed_in: true, available: true, hint: "" } : {}),
              usage: old?.usage ?? null,
            };
          });
          saveCachedAgents(next);
          return next;
        });
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
          .then(() => api.claudeResetHistory())
          .then((r) => setClaudeResets(r ?? null))
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
    const reads = conversationReads.current;
    const request = ++reads.issued;
    const list = await api.list();
    if (request < reads.applied) return reads.list;
    reads.applied = request;
    reads.list = list;
    setConversations(list);
    return list;
  }, []);

  const loadTokenUsage = useCallback(async (convId?: string | null) => {
    const id = convId ?? currentRef.current;
    const request = ++usageRequest.current;
    if (!id) {
      setTokenReport(null);
      setUsageLoading(false);
      return;
    }
    setUsageLoading(true);
    try {
      const dayStart = new Date().setHours(0, 0, 0, 0);
      const report = await api.tokenUsage(id, dayStart);
      if (request === usageRequest.current && id === currentRef.current) setTokenReport(report);
    } catch {
      if (request === usageRequest.current) setTokenReport(null);
    } finally {
      if (request === usageRequest.current) setUsageLoading(false);
    }
  }, []);

  const openUsage = useCallback(() => {
    setShowUsage(true);
  }, []);

  // Startup: agents, conversations (creating a first one), live events, update check.
  useEffect(() => {
    let disposed = false;
    let readingForEvent = false;
    let queuedRead = false;
    async function refreshAfterEvent() {
      if (disposed) return;
      queuedRead = true;
      if (readingForEvent) return;
      readingForEvent = true;
      try {
        do {
          queuedRead = false;
          await refreshList().catch(() => undefined);
        } while (queuedRead && !disposed);
      } finally {
        readingForEvent = false;
      }
    }

    refreshAgents();
    refreshList().then(async (list) => {
      const first = list[0] ?? (await api.create());
      if (!list.length) await refreshList();
      setCurrent(first.id);
    }).catch((e) => setError(`Couldn't load your conversations: ${e}. Sending a message will try again.`));
    const un1 = onEvent((e) => {
      if (e.conversation === currentRef.current) {
        setEvents((prev) => (prev.some((p) => p.id === e.id) ? prev : [...prev, e]));
        // In voice chat, AI replies are read aloud as they arrive.
        if (voiceRef.current && e.kind === "message" && e.sender !== "human") {
          speakerRef.current?.say(e.sender, senderName(e.sender, agentsRef.current), e.text);
        }
      }
      void refreshAfterEvent();
      // An AI saying an update is ready should light the button straight away.
      if (e.kind === "message" && e.sender !== "human") pollStaged();
      // A note from Consilium (an AI ran out of usage, or is back, or council on/off changed) changes the AIs' status.
      if (e.sender === "consilium") {
        refreshAgents();
        if (currentRef.current) api.switchedOff(currentRef.current).then(setOffHere).catch(() => setOffHere([]));
      }
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
        if (usageVisible.current) loadTokenUsage();
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
      disposed = true;
      un1.then((f) => f());
      un2.then((f) => f());
      un3.then((f) => f());
      window.removeEventListener("focus", onFocus);
    };
  }, [refreshAgents, refreshList]);

  // Checks GitHub at launch and every 30 minutes; a newer release is downloaded and staged, and
  // with automatic install on (Settings → Updates) Consilium installs it and restarts by itself.
  useEffect(() => {
    if (!prefs.autoUpdate) return;
    let lastCheck = 0;
    let checking = false;
    let disposed = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const check = () => {
      if (checking || disposed) return;
      clearTimeout(timer);
      checking = true;
      lastCheck = Date.now();
      void checkForUpdate(false, markDownloadStarting, markDownloadFailed)
        .then((result) => { if (!disposed) setUpdate(result); })
        .catch(() => undefined)
        .finally(() => {
          checking = false;
          if (!disposed) timer = setTimeout(check, nextUpdateCheckDelay());
        });
    };
    const onResume = () => {
      if (document.visibilityState === "visible" && Date.now() - lastCheck >= 30_000) check();
    };
    check();
    window.addEventListener("focus", onResume);
    window.addEventListener("online", onResume);
    document.addEventListener("visibilitychange", onResume);
    return () => {
      disposed = true;
      clearTimeout(timer);
      window.removeEventListener("focus", onResume);
      window.removeEventListener("online", onResume);
      document.removeEventListener("visibilitychange", onResume);
    };
  }, [prefs.autoUpdate, markDownloadStarting, markDownloadFailed]);

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
    followRef.current = true;
    seenCountRef.current = 0;
    ownSendRef.current = false;
    setUnseen(0);
    setAwayFromLatest(false);
  }, [current]);

  useEffect(() => {
    const fresh = events.slice(Math.min(seenCountRef.current, events.length));
    seenCountRef.current = events.length;
    // Read receipts aren't shown, so they neither count as new nor move the view.
    const shown = fresh.filter((e) => e.kind !== "seen").length;
    const mine = ownSendRef.current && fresh.some((e) => e.sender === "human" && e.kind === "message");
    if (mine) ownSendRef.current = false;
    if (followRef.current || mine) {
      followRef.current = true;
      setUnseen(0);
      endRef.current?.scrollIntoView({ behavior: prefs.effects ? "smooth" : "auto" });
    } else if (shown > 0) {
      setUnseen((n) => n + shown);
    }
  }, [events, prefs.effects]);

  // Shows the floating jump-to-latest button whenever the chat is scrolled off the bottom.
  const [awayFromLatest, setAwayFromLatest] = useState(false);

  // Only a scroll the reader started unpins; content shrinking, scroll anchoring and our own
  // scrolls also lower scrollTop and must not stop following.
  const userScrollIntentRef = useRef(0);
  useEffect(() => {
    const el = messagesRef.current;
    if (!el) return;
    const mark = () => { userScrollIntentRef.current = Date.now(); };
    const onWheel = (e: WheelEvent) => { if (e.deltaY < 0) mark(); };
    // Scrollbar drags hit the list itself; clicks on message controls (Collapse, Reply) hit children.
    const onPointer = (e: PointerEvent) => { if (e.target === el) mark(); };
    const onKey = (e: KeyboardEvent) => { if (["PageUp", "ArrowUp", "Home"].includes(e.key)) mark(); };
    el.addEventListener("wheel", onWheel, { passive: true });
    el.addEventListener("touchmove", mark, { passive: true });
    el.addEventListener("pointerdown", onPointer);
    el.addEventListener("keydown", onKey);
    return () => {
      el.removeEventListener("wheel", onWheel);
      el.removeEventListener("touchmove", mark);
      el.removeEventListener("pointerdown", onPointer);
      el.removeEventListener("keydown", onKey);
    };
  }, [current]);

  function onMessagesScroll() {
    const el = messagesRef.current;
    if (!el) return;
    const top = el.scrollTop;
    const nearBottom = el.scrollHeight - top - el.clientHeight <= 4;
    // The jump button shows whenever the view is off the bottom, not only when far up.
    setAwayFromLatest(!nearBottom);
    // Only an upward scroll unpins: smooth auto-scrolls and growing content never move scrollTop up.
    if (top < lastScrollTopRef.current - 2 && Date.now() - userScrollIntentRef.current < 400) {
      followRef.current = false;
    } else if (nearBottom) {
      followRef.current = true;
      setUnseen(0);
    }
    lastScrollTopRef.current = top;
  }

  // Streamed text, images loading and expanded messages make the list taller without a new
  // event; while following, keep the newest content in view.
  useEffect(() => {
    const el = messagesRef.current;
    if (!el || typeof ResizeObserver === "undefined") return;
    let frame = 0;
    const stick = () => {
      if (frame) return;
      frame = requestAnimationFrame(() => {
        frame = 0;
        if (!followRef.current) return;
        el.scrollTop = el.scrollHeight;
        lastScrollTopRef.current = el.scrollTop;
      });
    };
    const ro = new ResizeObserver(stick);
    ro.observe(el);
    Array.from(el.children).forEach((c) => ro.observe(c));
    const mo = new MutationObserver((records) => {
      for (const r of records) {
        r.addedNodes.forEach((n) => n instanceof Element && ro.observe(n));
        r.removedNodes.forEach((n) => n instanceof Element && ro.unobserve(n));
      }
      stick();
    });
    mo.observe(el, { childList: true });
    return () => {
      ro.disconnect();
      mo.disconnect();
      if (frame) cancelAnimationFrame(frame);
    };
  }, [current]);

  function scrollToLatest() {
    followRef.current = true;
    setUnseen(0);
    endRef.current?.scrollIntoView({ behavior: prefs.effects ? "smooth" : "auto" });
  }

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
  // AIs switched off in this conversation with the Start/Stop button in their council box.
  const [offHere, setOffHere] = useState<string[]>([]);
  useEffect(() => {
    if (!current) {
      setOffHere([]);
      return;
    }
    api.switchedOff(current).then(setOffHere).catch(() => setOffHere([]));
  }, [current]);
  async function toggleOn(id: string) {
    if (!current) return;
    try {
      setOffHere(await api.setAiOn(current, id, offHere.includes(id)));
    } catch (e) {
      setError(String(e));
    }
  }

  const [openModelPicker, setOpenModelPicker] = useState<string | null>(null);
  const [openAccessPicker, setOpenAccessPicker] = useState<string | null>(null);
  const [pickerAnchorRect, setPickerAnchorRect] = useState<DOMRect | null>(null);
  const [modelChoices, setModelChoices] = useState<Record<string, ModelChoice[]>>({});
  const [loadingModels, setLoadingModels] = useState<Record<string, boolean>>({});

  function toggleModelPicker(id: string, el: HTMLElement) {
    if (openModelPicker === id) {
      setOpenModelPicker(null);
      setPickerAnchorRect(null);
    } else {
      setOpenModelPicker(id);
      setOpenAccessPicker(null);
      setPickerAnchorRect(el.getBoundingClientRect());
      if (!modelChoices[id]) {
        setLoadingModels((prev) => ({ ...prev, [id]: true }));
        api.listModels(id)
          .then((choices) => setModelChoices((prev) => ({ ...prev, [id]: choices })))
          .catch((err) => setError(`Couldn't list models for ${id}: ${err}`))
          .finally(() => setLoadingModels((prev) => ({ ...prev, [id]: false })));
      }
    }
  }

  function toggleAccessPicker(id: string, el: HTMLElement) {
    if (openAccessPicker === id) {
      setOpenAccessPicker(null);
      setPickerAnchorRect(null);
    } else {
      setOpenAccessPicker(id);
      setOpenModelPicker(null);
      setPickerAnchorRect(el.getBoundingClientRect());
    }
  }

  async function handleSetModel(agentId: string, model: string | null) {
    try {
      await api.setModel(agentId, model);
      setOpenModelPicker(null);
      setPickerAnchorRect(null);
      refreshAgents();
    } catch (e) {
      setError(`Couldn't set model: ${e}`);
    }
  }

  useEffect(() => {
    if (!openModelPicker && !openAccessPicker) return;
    function handleClickOutside(e: MouseEvent) {
      const target = e.target as HTMLElement | null;
      if (target?.closest(".picker-popover") || target?.closest(".model-picker-btn") || target?.closest(".access-picker-btn")) {
        return;
      }
      setOpenModelPicker(null);
      setOpenAccessPicker(null);
      setPickerAnchorRect(null);
    }
    function handleKeyDown(e: KeyboardEvent) {
      if (e.key === "Escape") {
        setOpenModelPicker(null);
        setOpenAccessPicker(null);
        setPickerAnchorRect(null);
      }
    }
    function handleScrollOrResize() {
      setOpenModelPicker(null);
      setOpenAccessPicker(null);
      setPickerAnchorRect(null);
    }
    window.addEventListener("mousedown", handleClickOutside);
    window.addEventListener("keydown", handleKeyDown);
    window.addEventListener("resize", handleScrollOrResize);
    window.addEventListener("scroll", handleScrollOrResize, true);
    return () => {
      window.removeEventListener("mousedown", handleClickOutside);
      window.removeEventListener("keydown", handleKeyDown);
      window.removeEventListener("resize", handleScrollOrResize);
      window.removeEventListener("scroll", handleScrollOrResize, true);
    };
  }, [openModelPicker, openAccessPicker]);

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

  // An AI that runs out of usage is switched off under Ask, and switched back on when its limit
  // resets, unless the human changed that chip themselves in the meantime.
  const autoOff = useRef<Set<string>>(new Set());
  const exhaustedKey = members.filter(isExhausted).map((m) => m.id).sort().join(",");
  useEffect(() => {
    const out = new Set(exhaustedKey ? exhaustedKey.split(",") : []);
    setTargets((t) => {
      let next = t;
      for (const id of out) {
        if (next.includes(id)) {
          next = next.filter((x) => x !== id);
          autoOff.current.add(id);
        }
      }
      for (const id of [...autoOff.current]) {
        if (out.has(id)) continue;
        autoOff.current.delete(id);
        if (members.some((m) => m.id === id) && !next.includes(id)) next = [...next, id];
      }
      return next;
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [exhaustedKey]);
  const others = useMemo(() => agents.filter((a) => !a.member), [agents]);

  // Sending while the AIs are answering doesn't interrupt them: they read it and carry on. Stop interrupts.
  async function send(budgetChecked = false) {
    const text = draftBox.current?.get() ?? "";
    if (budgetCheck.current || (!budgetChecked && budgetWarning) || (!text.trim() && !pending.length)) return;
    const limit = prefs.dailyTokenWarning;
    if (!budgetChecked && current && discuss && !busy && Number.isSafeInteger(limit) && limit > 0) {
      budgetCheck.current = true;
      let message = "";
      try {
        const dayStart = new Date().setHours(0, 0, 0, 0);
        const report = await api.tokenUsage(current, dayStart);
        const total = Object.values(report.today).reduce((sum, agent) => sum + agent.total_tokens, 0);
        if (total >= limit) {
          message = `Consilium recorded ${num(total)} tokens today. Your warning threshold is ${num(limit)} tokens.`;
        }
      } catch {
        message = "Consilium couldn't check today's recorded token usage.";
      } finally {
        budgetCheck.current = false;
      }
      if (current !== currentRef.current || text !== (draftBox.current?.get() ?? "")) return;
      if (message) {
        setBudgetWarning({ id: current, text, message });
      } else {
        await sendRef.current(true);
      }
      return;
    }
    // Enter used to do nothing at all when no conversation was open (for example when the list
    // failed to load at startup). Open the newest one, or start one, and send there instead.
    let id = current;
    if (!id) {
      try {
        const list = await refreshList();
        id = (list[0] ?? (await api.create())).id;
        setCurrent(id);
      } catch (e) {
        setError(`Couldn't send: no conversation is open, and opening one failed (${e}). Your message is still in the box.`);
        return;
      }
    }
    setError(null);
    const files = pending;
    setDraft("");
    setPending([]);
    // If the app's engine doesn't answer, say so instead of leaving the box empty and silent.
    const slow = window.setTimeout(() => setError("Your message is taking unusually long to send. It will appear here once Consilium accepts it."), 15000);
    try {
      const to = targets.filter((t) => members.some((m) => m.id === t));
      ownSendRef.current = true;
      await api.send(id, withPersona(text, persona), to, discuss ? rounds : 1, false, files);
      setError((e) => (e?.startsWith("Your message is taking") ? null : e));
    } catch (e) {
      ownSendRef.current = false;
      setDraft(text);
      setPending(files);
      setError(String(e));
    } finally {
      window.clearTimeout(slow);
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
  // Every linked computer is asked to update too, first, since installing here quits the app.
  async function requestUpdate() {
    if (updating || requestingUpdate) return;
    const peers = await api.requestPeerUpdate().catch((e) => {
      setError(`Couldn't ask the linked computers to update: ${e}`);
      return 0;
    });
    if (staged) return updateNow();
    if (stagedUi) return applyUi();
    if (!current || !members.length) return;
    setRequestingUpdate(true);
    setError(null);
    try {
      await api.send(
        current,
        (peers
          ? `Update request (also sent to ${peers} linked computer${peers === 1 ? "" : "s"}, which install any newer build they have or download): `
          : "Update request: ") +
          "No update is staged. If a build is active, give its status. Otherwise use docs/UPGRADE_STATUS.md, the queue for all six upgrade documents. Check open PRs and agree on owners and file scope in one message. Prefer small app and token-efficiency tasks, and avoid duplicate builds. Record verification in the queue, integrate only after checks pass, then stage the appropriate UI or desktop update.",
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
  useEffect(() => {
    if (!IN_APP) return;
    let disposed = false;
    let unlisten: (() => void) | undefined;
    api.quickSummonAvailable().then((available) => {
      if (!disposed) setQuickSummonAvailable(available);
    }).catch(() => undefined);
    onQuickSummon(() => draftBox.current?.focus()).then((remove) => {
      if (disposed) remove();
      else unlisten = remove;
    }).catch(() => undefined);
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  const attachPastedRef = useRef(attachPasted);
  attachPastedRef.current = attachPasted;

  async function attachPasted(files: File[]) {
    if (!current) return;
    for (const f of files) {
      try {
        const data = Array.from(new Uint8Array(await f.arrayBuffer()));
        const name = f.name && f.name !== "image.png" ? f.name : `pasted-${new Date().toISOString().slice(0, 19).replace(/[:T]/g, "-")}.${f.type.split("/")[1] || "png"}`;
        if (current !== currentRef.current) return;
        const a = await api.attachData(current, name, data);
        if (current !== currentRef.current) return;
        setPending((p) => [...p, a]);
      } catch (e) {
        setError(`Couldn't attach ${f.name || "the pasted file"}: ${e}`);
      }
    }
  }

  async function attachClipboardImage() {
    if (!current) return;
    const conversation = current;
    try {
      if (IN_APP) {
        const attachment = await api.attachClipboardImage(conversation);
        if (conversation === currentRef.current) {
          setPending((previous) => [...previous, attachment]);
          setError(null);
        }
        return;
      }
      const items = await navigator.clipboard.read();
      const files: File[] = [];
      for (const item of items) {
        const format = item.types.find((type) => ["image/png", "image/jpeg", "image/webp", "image/gif"].includes(type));
        if (!format) continue;
        const blob = await item.getType(format);
        const extension = format === "image/jpeg" ? "jpg" : format.split("/")[1];
        files.push(new File([blob], `clipboard-${files.length + 1}.${extension}`, { type: format }));
      }
      if (conversation !== currentRef.current) return;
      if (files.length === 0) {
        setError("Copy a screenshot or image first, then try again.");
        return;
      }
      await attachPasted(files);
    } catch (error) {
      if (conversation === currentRef.current) {
        setError(IN_APP ? String(error) : "Couldn't read an image from the clipboard. Paste the copied image directly into the message box instead.");
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
    ownSendRef.current = true;
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

  // Listen the whole time voice chat is on, including while a reply is read aloud, so the human can
  // talk over it. Speech that matches the reply being read is the AI's own voice from the speakers
  // and is ignored. Anything else pauses the reply; if a message follows, the reading stops and the
  // message is sent, and if nothing does within five seconds, the reply carries on.
  useEffect(() => {
    if (!voice || !canListen) return;
    const sp = speakerRef.current!;
    const clearResume = () => {
      window.clearTimeout(resumeTimer.current);
      resumeTimer.current = undefined;
    };
    const armResume = () => {
      clearResume();
      resumeTimer.current = window.setTimeout(() => {
        resumeTimer.current = undefined;
        sp.resume();
        setInterrupting(false);
        setHeard("");
      }, RESUME_AFTER_MS);
    };
    const l = new Listener(
      (t) => {
        const spoken = sp.speakingText;
        if (spoken) {
          if (!sp.isPaused && !startsInterrupt(t, spoken)) return;
          if (!sp.isPaused) {
            sp.pause();
            setInterrupting(true);
          }
          armResume();
        }
        setHeard(t);
      },
      (t) => {
        const spoken = sp.speakingText;
        if (spoken) {
          if (!sp.isPaused || isEcho(t, spoken)) return;
          clearResume();
          setInterrupting(false);
        }
        setHeard("");
        if (voiceCommandRef.current(t)) return;
        // A real interruption: stop reading. The replies stay in the chat.
        if (spoken) sp.stop();
        sp.sayThinking(members[0]?.id ?? "gemini", members[0]?.name ?? "Gemini");
        sendTextRef.current(t);
      },
      (m) => setError(m),
    );
    l.start();
    return () => {
      l.stop();
      clearResume();
      sp.resume();
      setInterrupting(false);
      setHeard("");
    };
  }, [voice, current]);

  // Leaving voice chat or the conversation stops any speech.
  useEffect(() => {
    if (!voice) speakerRef.current?.stop();
  }, [voice]);
  useEffect(() => () => speakerRef.current?.stop(), [current]);

  // Approve/Deny on a REQUEST callout sends the decision as the human's message, quoting the request.
  async function decide(key: string, request: string, requester: string, approved: boolean, always = false) {
    if (!current) return;
    setError(null);
    setDecidingRequests((pending) => new Set(pending).add(key));
    try {
      if (always) setCouncil(await api.saveApproval(request));
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
  // "Auto-run this chat": on only in chats where the human accepted its warning (auto_run.rs).
  const [chatAutoRun, setChatAutoRunState] = useState(false);
  // The chat whose auto-run setting has arrived. Until it matches `current` the request tray stays
  // hidden, so switching into an auto-run chat never flashes the tray open before it minimizes.
  const [chatAutoRunFor, setChatAutoRunFor] = useState<string | null>(null);
  const [autoRunAsk, setAutoRunAsk] = useState<{ eventId: string; index: number } | null>(null);
  useEffect(() => {
    setChatAutoRunState(false);
    setChatAutoRunFor(null);
    setAutoRunAsk(null);
    if (!current) return;
    let live = true;
    api.chatAutoRun(current)
      .then((on) => { if (live) setChatAutoRunState(on); })
      .catch(() => {})
      .finally(() => { if (live) setChatAutoRunFor(current); });
    return () => { live = false; };
  }, [current]);
  const confirmAutoRun = () => {
    const ask = autoRunAsk;
    setAutoRunAsk(null);
    if (!current || !ask) return;
    api.setChatAutoRun(current, true)
      .then((on) => {
        setChatAutoRunState(on);
        commandActions.run(ask.eventId, ask.index);
      })
      .catch((err) => setError(`Couldn't turn on auto-run: ${err}`));
  };
  const stopAutoRun = () => {
    if (!current) return;
    api.setChatAutoRun(current, false)
      .then((on) => setChatAutoRunState(on))
      .catch((err) => setError(`Couldn't turn off auto-run: ${err}`));
  };
  const [runningCommands, setRunningCommands] = useState<Set<string>>(() => new Set());
  const ranCommands = useMemo(() => handledCommands(events), [events]);
  const commandActions = useMemo<CommandActions>(() => ({
    handled: ranCommands,
    running: runningCommands,
    canAlwaysRun: (sender: string) =>
      !council?.auto_run_commands && agents.some((a) => a.id === sender && a.access === "unrestricted"),
    alwaysRun: (eventId: string, index: number) => {
      api.setAutoRunCommands(true)
        .then(() => setCouncil((c) => (c ? { ...c, auto_run_commands: true } : c)))
        .catch((err) => setError(`Couldn't turn on Always run: ${err}`));
      commandActions.run(eventId, index);
    },
    chatAutoRun,
    autoRunChat: (eventId: string, index: number) => setAutoRunAsk({ eventId, index }),
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
  }), [ranCommands, runningCommands, current, members, agents, council?.auto_run_commands, chatAutoRun]);
  const answered = useMemo(() => answeredRequests(events), [events]);
  const unanswered = useMemo(() => pendingRequests(events, answered, requestFloor), [events, answered, requestFloor]);
  const unansweredCmds = useMemo(() => pendingCommands(events, ranCommands, requestFloor, OS), [events, ranCommands, requestFloor]);
  const typedActivity = TYPED_THEMES.has(prefs.theme);

  // Every request and shell command still waiting for the human, in chat order.
  const autoApproves = chatAutoRun && (council?.auto_continue ?? true);
  const pendingItems = useMemo<PendingItem[]>(() => {
    const req = new Set(unanswered);
    const cmd = new Set(unansweredCmds);
    if (!req.size && !cmd.size) return [];
    const out: PendingItem[] = [];
    // Anything older than a day has gone stale: it stays in the chat, with its buttons, but leaves
    // the tray, so the tray holds what still matters.
    const freshAfter = Date.now() - STALE_AFTER_MS;
    for (const e of events) {
      if (e.sender === "human" || (requestFloor !== null && e.seq <= requestFloor) || e.ts < freshAfter) continue;
      const found: [number, PendingItem][] = [];
      for (const [line, text] of e.text.split("\n").entries()) {
        const key = requestKey(e.id, line);
        // In an auto-run chat Consilium approves ordinary requests itself when the turn ends
        // (autocontinue.rs), so they don't wait for the human. Install and administrator
        // requests still need the human's own click, so they stay.
        if (autoApproves && !/(INSTALL|ELEVATED)_COMMAND_JSON:/.test(text)) continue;
        if (req.has(key)) found.push([line, { kind: "request", key, event: e, text: /^\s*REQUEST:\s*(.*)$/i.exec(text)?.[1] ?? text }]);
      }
      fencedBlocks(e.text).forEach((fence, index) => {
        const key = `${e.id}#${index}`;
        if (cmd.has(key)) found.push([fence.start, { kind: "command", key, event: e, index, fence }]);
      });
      found.sort((a, b) => a[0] - b[0]).forEach(([, item]) => out.push(item));
    }
    return out;
  }, [events, unanswered, unansweredCmds, requestFloor, autoApproves]);
  const totalUnanswered = pendingItems.length;

  // The tray shows one item at a time. When the shown one is answered, the next one slides in.
  const [trayFocus, setTrayFocus] = useState<{ key: string | null; index: number }>({ key: null, index: 0 });
  const trayIndex = (() => {
    const at = trayFocus.key ? pendingItems.findIndex((p) => p.key === trayFocus.key) : -1;
    return at >= 0 ? at : Math.min(trayFocus.index, Math.max(pendingItems.length - 1, 0));
  })();
  const lastJumpRef = useRef<string | null>(null);

  function focusTray(index: number) {
    const n = pendingItems.length;
    if (!n) return;
    const i = ((index % n) + n) % n;
    setTrayFocus({ key: pendingItems[i].key, index: i });
  }

  // Scroll the chat to one waiting item and flash it. The view then stays put until the human
  // scrolls back down, so a new message doesn't yank them away.
  function showInChat(item: PendingItem) {
    const el = document.getElementById(item.kind === "request" ? requestElementId(item.key) : commandElementId(item.key));
    if (!el) return;
    followRef.current = false;
    lastJumpRef.current = item.key;
    el.scrollIntoView({ behavior: prefs.effects ? "smooth" : "auto", block: "center" });
    el.classList.remove("request-flash");
    void el.offsetWidth;
    el.classList.add("request-flash");
    window.setTimeout(() => el.classList.remove("request-flash"), 1800);
  }

  // "Next request": the first press goes to the item shown in the tray, each later press to the next one.
  function jumpToNextRequest() {
    const n = pendingItems.length;
    if (!n) return;
    const i = lastJumpRef.current === pendingItems[trayIndex]?.key ? (trayIndex + 1) % n : trayIndex;
    setTrayFocus({ key: pendingItems[i].key, index: i });
    showInChat(pendingItems[i]);
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

  // Deleting asks first in an in-app dialog (confirm() isn't reliably shown in the app window).
  const [confirmDelete, setConfirmDelete] = useState(false);
  function deleteCurrent() {
    if (current && meta) setConfirmDelete(true);
  }
  async function deleteConfirmed() {
    setConfirmDelete(false);
    if (!current || !meta) return;
    try {
      await api.remove(current);
      const list = await refreshList();
      if (list.length) setCurrent(list[0].id);
      else newConversation();
    } catch (e) {
      setError(String(e));
    }
  }

  // Bulk cleanup is deliberately protected by two separate in-app prompts. The backend checks
  // every conversation before deleting the first one, so a running chat cannot leave a partial
  // cleanup behind.
  const [deleteAllStep, setDeleteAllStep] = useState<0 | 1 | 2>(0);
  const [deleteAllText, setDeleteAllText] = useState("");
  const [deletingAll, setDeletingAll] = useState(false);
  function closeDeleteAll() {
    if (deletingAll) return;
    setDeleteAllStep(0);
    setDeleteAllText("");
  }
  async function deleteAllConfirmed() {
    if (deletingAll || deleteAllText.trim().toUpperCase() !== "DELETE") return;
    setDeletingAll(true);
    try {
      const removed = await api.removeAll();
      const fresh = await api.create();
      await refreshList();
      setCurrent(fresh.id);
      setDeleteAllStep(0);
      setDeleteAllText("");
      setToast(`${removed} conversation${removed === 1 ? "" : "s"} deleted`);
      setTimeout(() => setToast(null), 2500);
    } catch (e) {
      setDeleteAllStep(0);
      setDeleteAllText("");
      setError(`Couldn't delete all conversations: ${e}`);
    } finally {
      setDeletingAll(false);
    }
  }

  // Restart dialog: confirmation prompt with computer picker and Restart All
  type RestartTarget = { id: string; name: string; connected: boolean; isSelf: boolean };
  const [showRestartModal, setShowRestartModal] = useState(false);
  const [restartComputers, setRestartComputers] = useState<RestartTarget[]>([]);
  const [selectedRestartComputers, setSelectedRestartComputers] = useState<string[]>(["this"]);
  const [restarting, setRestarting] = useState(false);

  async function openRestartModal() {
    try {
      const s = await api.linkStatus();
      const list: RestartTarget[] = [
        { id: "this", name: s?.name ? `This computer (${s.name})` : "This computer", connected: true, isSelf: true },
        ...(s?.peers || []).map((p) => ({
          id: p.name,
          name: p.name,
          connected: p.connected,
          isSelf: false,
        })),
      ];
      setRestartComputers(list);
      setSelectedRestartComputers(["this"]);
    } catch {
      setRestartComputers([{ id: "this", name: "This computer", connected: true, isSelf: true }]);
      setSelectedRestartComputers(["this"]);
    }
    setShowRestartModal(true);
  }

  async function handleRestart(targets: string[]) {
    setRestarting(true);
    try {
      await api.restartApp(targets);
      setTimeout(() => {
        setShowRestartModal(false);
        setRestarting(false);
      }, 1500);
    } catch (e) {
      setError(String(e));
      setRestarting(false);
    }
  }

  async function saveTitle(conversation: string, title: string) {
    try {
      await api.rename(conversation, title);
      refreshList();
    } catch (e) {
      setError(String(e));
    }
  }

  async function checkNow() {
    setUpdateNote("Checking…");
    try {
      const u = await checkForUpdate(true, markDownloadStarting, markDownloadFailed);
      setUpdate(u);
      setUpdateNote(
        !u ? "You're up to date."
          : u.unreleased ? `Version ${u.latest} is on GitHub but has no installer yet (you have ${u.current}). Build it from source, or ask the AIs to publish a release.`
          : `Version ${u.latest} is available (you have ${u.current}).`,
      );
    } catch (error) {
      setUpdateNote(`Couldn't check GitHub for updates: ${String(error)}`);
    }
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
      setAddPanel("ai");
      const portal = PROVIDER_PORTALS[a.id];
      if (portal) await openUrl(portal.account).catch((e) => setError(`Couldn't open ${a.company}: ${e}`));
      setToast(`Finish account, billing, and API-key setup with ${a.company}; then paste only the key in Add AI`);
      setTimeout(() => setToast(null), 8000);
      return;
    }
    if (st !== "ready") {
      try {
        const title = await api.setup(a.id, st === "missing" ? "install" : "signin");
        setAddPanel("ai");
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

  async function toggleAutoContinue(on: boolean) {
    try {
      await api.setAutoContinue(on);
      refreshAgents();
    } catch (e) {
      setError(`Couldn't change the setting: ${e}`);
    }
  }

  async function toggleForecast(on: boolean) {
    try {
      await api.setResetForecast(on);
      if (!on) { setForecast(null); setClaudeResets(null); }
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

  const toggleTarget = (id: string) => {
    autoOff.current.delete(id);
    setTargets((t) => (t.includes(id) ? t.filter((x) => x !== id) : [...t, id]));
  };

  const paletteCommands = useMemo(() => [
    { id: "new-conv", name: "New Conversation", hint: "Start a fresh topic", run: () => { newConversation(); } },
    { id: "status", name: "Request Council Status", hint: "Ask every AI for its current work and next step", run: () => { requestStatus(); } },
    { id: "branches", name: "AI Branches & Worktrees", hint: "Review, merge or discard isolated copies made by Full-capacity AIs", run: () => setBranchesOpen(true) },
    { id: "repo", name: "Inspect a GitHub repo", hint: "Sandboxed clone and report: languages, licence, build files, risks", run: () => setRepoOpen(true) },
    { id: "audit", name: "Open Audit Log", hint: "Approvals, requests, updates and errors, newest first", run: () => setAuditOpen(true) },
    { id: "usage", name: "Show Token Usage", hint: "Conversation & daily totals", run: () => openUsage() },
    { id: "self-check", name: "Run Self-Check", hint: "Probe all active council models", run: () => runSelfCheck() },
    { id: "manage-ais", name: "Add / Remove AIs", hint: "Council membership", run: () => setAddPanel("ai") },
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
    { id: "theme-retro84", name: "Theme: Retro '84", hint: "Neon grid, scanlines, glow", run: () => setPrefs((p) => ({ ...p, theme: "retro84" })) },
    { id: "theme-netrunner", name: "Theme: Netrunner", hint: "Black, neon green, cyberpunk", run: () => setPrefs((p) => ({ ...p, theme: "netrunner" })) },
  ], [current, discuss, members, openUsage, requestingStatus]);

  // Spoken commands: "Consilium, open settings", "Computer, open Safari", "Consilium, stop".
  // Returns false for ordinary speech, which goes to the AIs as a message.
  voiceCommandRef.current = (text: string) => {
    const cmd = parseVoiceCommand(text);
    if (!cmd) return false;
    const sp = speakerRef.current;
    const run = (id: string) => paletteCommands.find((c) => c.id === id)?.run();
    switch (cmd.kind) {
      case "speech":
        if (cmd.action === "skip") sp?.skip();
        else sp?.stop();
        return true;
      case "action": {
        const ids = { settings: "settings", audit: "audit", usage: "usage", status: "status", new: "new-conv", "add-ai": "manage-ais" } as const;
        if (cmd.action === "palette") {
          setPaletteQuery("");
          setPaletteIndex(0);
          setShowPalette(true);
        } else if (cmd.action === "text-chat") {
          sp?.stop();
          switchVoice(false);
          return true;
        } else run(ids[cmd.action]);
        break;
      }
      case "theme":
        run(`theme-${cmd.theme}`);
        break;
      case "open-app":
        // Opening a program needs a click on Consilium's own confirmation, never voice alone.
        setOpenAppAsk(cmd.app);
        break;
      case "computer":
        sendTextRef.current(
          `Voice command: ${cmd.instruction}. Before doing anything that changes this computer or controls another program, ask me with a REQUEST: line and wait for my approval.`,
        );
        break;
    }
    sp?.resume();
    return true;
  };

  async function confirmOpenApp() {
    const name = openAppAsk;
    setOpenAppAsk(null);
    if (!name) return;
    try {
      await api.openApp(name);
    } catch (e) {
      setError(String(e));
    }
  }

  const sidebarRef = useRef<HTMLElement | null>(null);
  useSidebarFit(sidebarRef);

  const filteredCommands = useMemo(() => {
    const q = paletteQuery.trim().toLowerCase();
    if (!q) return paletteCommands;
    return paletteCommands.filter((c) => c.name.toLowerCase().includes(q) || (c.hint && c.hint.toLowerCase().includes(q)));
  }, [paletteCommands, paletteQuery]);

  return (
    <div className="app" data-backdrop={prefs.backdrop.enabled ? prefs.backdrop.mode : undefined}>
      {prefs.backdrop.enabled && prefs.backdrop.mode !== "stealth" && (
        <Suspense fallback={null}>
          <TelemetryBackdrop prefs={prefs.backdrop} agents={agents} workers={thinkingIds} events={events} meshPeers={meshPeers}
            memory={pressure?.memory_percent} paused={!prefs.effects} eco={adaptiveMode === "eco"} />
        </Suspense>
      )}
      <div className="fx-scanlines" aria-hidden />
      <aside className="sidebar" ref={sidebarRef}>
        <div className="brand">
          <img
            src="/icon.png"
            alt=""
            className={`brand-icon${downloadStatus.active ? " update-downloading" : ""}`}
            title={downloadStatus.active ? `Downloading Consilium ${downloadStatus.version || "update"} from GitHub` : undefined}
          />
          <div>
            <div className="brand-name">Consilium</div>
            <div className="brand-tagline">A council of minds</div>
          </div>
          {downloadStatus.active && (
            <span className="visually-hidden" role="status">
              Downloading Consilium {downloadStatus.version || "update"} from GitHub
            </span>
          )}
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
        <button
          type="button"
          className="link danger-link delete-all-conversations"
          title="Delete every conversation and its attachments"
          onClick={() => setDeleteAllStep(1)}
          disabled={!conversations.length}
        >
          Delete all conversations…
        </button>
        <div className={`agents${members.length >= 5 ? " many" : ""}`}>
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
          <div className={`agent-list${members.length >= 5 ? " scrolls" : ""}`}>
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
            const off = offHere.includes(a.id);
            return (
              <div key={a.id} style={{ opacity: off ? 0.55 : 1, minWidth: 0 }}>
                <AgentDot
                  agent={a}
                  onSetup={setupAgent}
                  onRetry={retryAgent}
                  check={checks[a.id]}
                  forecast={forecast}
                  claudeResets={claudeResets}
                  isWorking={isWorking}
                  isFrozen={isFrozen}
                  load={agentLoad}
                  controls={<>
                  <button
                    type="button"
                    className="link"
                    disabled={!current}
                    onClick={() => void toggleOn(a.id)}
                    title={off ? `Start ${a.name} in this chat` : `Stop ${a.name} in this chat: it won't be asked here, even when @mentioned or by auto-continue`}
                    aria-label={off ? `Start ${a.name} in this chat` : `Stop ${a.name} in this chat`}
                    style={{ color: off ? "#2e9d4f" : "#d64545", fontSize: 11, lineHeight: 1, padding: "1px 4px", flex: "none" }}
                  >
                    {off ? "\u25B6" : "\u25A0"}
                  </button>

                  <button
                    type="button"
                    className="link model-picker-btn"
                    onClick={(e) => toggleModelPicker(a.id, e.currentTarget)}
                    title={`Model: ${a.model || "Default"} (click to choose model)`}
                    aria-label={`Model picker for ${a.name}`}
                    style={{ color: "#3b82f6", fontSize: 10, lineHeight: 1, padding: "1px 4px", flex: "none" }}
                  >
                    {"\u25C6"}
                  </button>

                  <button
                    type="button"
                    className="link access-picker-btn"
                    onClick={(e) => toggleAccessPicker(a.id, e.currentTarget)}
                    title={`Access: ${ACCESS_LEVELS.find((l) => l.id === a.access)?.name ?? a.access} (click to choose access level)`}
                    aria-label={`Access picker for ${a.name}`}
                    style={{ color: "#22c55e", fontSize: 10, lineHeight: 1, padding: "1px 4px", flex: "none" }}
                  >
                    {"\u25CF"}
                  </button>

                  {openModelPicker === a.id && pickerAnchorRect &&
                    createPortal(
                      <div
                        className="picker-popover model-popover"
                        style={{
                          position: "fixed",
                          left: pickerAnchorRect.right + 8 + 240 <= window.innerWidth
                            ? pickerAnchorRect.right + 8
                            : Math.max(10, pickerAnchorRect.left - 240),
                          top: Math.max(10, Math.min(pickerAnchorRect.top - 10, window.innerHeight - 280)),
                          zIndex: 1000,
                          background: "var(--panel, #1e1e24)",
                          border: "1px solid var(--border, #333)",
                          borderRadius: "6px",
                          boxShadow: "0 6px 20px rgba(0,0,0,0.45)",
                          padding: "8px",
                          minWidth: "180px",
                          maxWidth: "240px",
                          display: "flex",
                          flexDirection: "column",
                          gap: "4px",
                          textAlign: "left",
                        }}
                      >
                        <div style={{ fontSize: 11, fontWeight: 600, color: "var(--muted)", paddingBottom: 4, borderBottom: "1px solid var(--border)" }}>
                          Model for {a.name}
                        </div>
                        <button
                          type="button"
                          className="picker-option"
                          onClick={() => void handleSetModel(a.id, null)}
                          style={{
                            display: "flex",
                            alignItems: "center",
                            justifyContent: "space-between",
                            padding: "4px 6px",
                            borderRadius: "4px",
                            fontSize: 12,
                            background: !a.model ? "rgba(59, 130, 246, 0.2)" : "transparent",
                            color: !a.model ? "#60a5fa" : "inherit",
                            border: "none",
                            cursor: "pointer",
                            textAlign: "left",
                          }}
                        >
                          <span>Default model</span>
                          {!a.model && <span>✓</span>}
                        </button>
                        {loadingModels[a.id] ? (
                          <div style={{ fontSize: 11, color: "var(--muted)", padding: "4px 6px" }}>Loading models…</div>
                        ) : (
                          (modelChoices[a.id] ?? []).map((m) => (
                            <button
                              key={m.id}
                              type="button"
                              className="picker-option"
                              onClick={() => void handleSetModel(a.id, m.id)}
                              style={{
                                display: "flex",
                                alignItems: "center",
                                justifyContent: "space-between",
                                padding: "4px 6px",
                                borderRadius: "4px",
                                fontSize: 12,
                                background: a.model === m.id ? "rgba(59, 130, 246, 0.2)" : "transparent",
                                color: a.model === m.id ? "#60a5fa" : "inherit",
                                border: "none",
                                cursor: "pointer",
                                textAlign: "left",
                              }}
                            >
                              <span>{m.name || m.id}</span>
                              {a.model === m.id && <span>✓</span>}
                            </button>
                          ))
                        )}
                      </div>,
                      document.body
                    )}

                  {openAccessPicker === a.id && pickerAnchorRect &&
                    createPortal(
                      <div
                        className="picker-popover access-popover"
                        style={{
                          position: "fixed",
                          left: pickerAnchorRect.right + 8 + 260 <= window.innerWidth
                            ? pickerAnchorRect.right + 8
                            : Math.max(10, pickerAnchorRect.left - 260),
                          top: Math.max(10, Math.min(pickerAnchorRect.top - 20, window.innerHeight - 300)),
                          zIndex: 1000,
                          background: "var(--panel, #1e1e24)",
                          border: "1px solid var(--border, #333)",
                          borderRadius: "6px",
                          boxShadow: "0 6px 20px rgba(0,0,0,0.45)",
                          padding: "8px",
                          minWidth: "200px",
                          maxWidth: "260px",
                          display: "flex",
                          flexDirection: "column",
                          gap: "4px",
                          textAlign: "left",
                        }}
                      >
                        <div style={{ fontSize: 11, fontWeight: 600, color: "var(--muted)", paddingBottom: 4, borderBottom: "1px solid var(--border)" }}>
                          Access level for {a.name}
                        </div>
                        {ACCESS_LEVELS.map((lvl) => {
                          const active = a.access === lvl.id;
                          return (
                            <button
                              key={lvl.id}
                              type="button"
                              className="picker-option"
                              onClick={() => {
                                setOpenAccessPicker(null);
                                setPickerAnchorRect(null);
                                void changeAccess(a, lvl.id);
                              }}
                              title={lvl.note}
                              style={{
                                display: "flex",
                                flexDirection: "column",
                                alignItems: "flex-start",
                                padding: "4px 6px",
                                borderRadius: "4px",
                                background: active ? "rgba(34, 197, 94, 0.2)" : "transparent",
                                color: active ? "#4ade80" : "inherit",
                                border: "none",
                                cursor: "pointer",
                                textAlign: "left",
                              }}
                            >
                              <div style={{ display: "flex", width: "100%", justifyContent: "space-between", alignItems: "center" }}>
                                <span style={{ fontWeight: active ? 600 : 400, fontSize: 12 }}>{lvl.name}</span>
                                {active && <span style={{ fontSize: 12 }}>✓</span>}
                              </div>
                              <span style={{ fontSize: 10, color: "var(--muted)", lineHeight: 1.2 }}>{lvl.note}</span>
                            </button>
                          );
                        })}
                      </div>,
                      document.body
                    )}
                  </>}
                />
              </div>
            );
          })}
          </div>
        </div>
        <div className="sidebar-foot">
        <ResourcePressureGauge pressure={pressure} />
        {prefs.backdrop.enabled && (
          <button type="button" className="btn ghost small-btn" title="Cycle the performance backdrop (Ctrl+Shift+M)"
            onClick={cycleBackdrop}>{backdropModes[prefs.backdrop.mode]}</button>
        )}
        <div className="sidebar-actions">
          <button className="btn ghost" ref={addTrigger} onClick={() => setAddPanel("choose")} aria-haspopup="dialog" aria-label="Add an AI or device">
            Add <span aria-hidden="true">🤖 💻</span>
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
        </div>
      </aside>

      <main
        className={`main ${wallpaper && prefs.bingWallpaper && !prefs.backdrop.enabled ? "has-wallpaper" : ""}`}
        style={wallpaper && prefs.bingWallpaper ? ({ "--bing-wallpaper": `url(${wallpaper.data_url})` } as React.CSSProperties) : undefined}
      >
        {update && (
          <div className="banner">
            Consilium {update.latest} {update.unreleased ? "is on GitHub, with no installer yet" : "is available"} (you have {update.current}).
            <button title="Open the new version on GitHub" className="link" onClick={() => openUrl(update.url)}>
              {update.unreleased ? "View on GitHub" : "View release"}
            </button>
            <button title="Hide this notice until the next check" className="link" onClick={() => setUpdate(null)}>
              Later
            </button>
          </div>
        )}
        <header className="topbar">
          <div className="title-block">
            {meta
              ? <ConversationTitle key={meta.id} title={meta.title} rename={(title) => void saveTitle(meta.id, title)} />
              : <h1>Consilium</h1>}
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
              <CiBadge workspace={meta?.workspace ?? council?.default_workspace ?? null} />
            </div>
          </div>
          <div className="topbar-actions">
            <div className="topbar-status">
              <MessagingIndicator />
              <LinkIndicator onConnectedCount={prefs.backdrop.enabled && prefs.backdrop.mode === "rain" ? setMeshPeers : undefined} />
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
              title="Restart Consilium on this computer or linked computers"
              className="btn ghost compact-action restart-action"
              onClick={openRestartModal}
              aria-label="Restart Consilium"
            >
              <span className="topbar-action-icon" aria-hidden="true">⟳</span>
              <span className="topbar-action-label">Restart</span>
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
              title="Review, merge or discard isolated copies made by Full-capacity AIs"
              className={`btn ghost compact-action ${branchesOpen ? "on" : ""}`}
              onClick={() => setBranchesOpen((o) => !o)}
              aria-label="AI Branches and Worktrees"
            >
              <span className="topbar-action-icon" aria-hidden="true">⑂</span>
              <span className="topbar-action-label">Branches</span>
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
        {branchesOpen && (
          <BranchesDrawer
            conversation={current}
            agents={members}
            onClose={() => setBranchesOpen(false)}
          />
        )}
        {repoOpen && (
          <RepoDrawer
            onClose={() => setRepoOpen(false)}
            onAsk={(text) => {
              setDraft(text);
              setRepoOpen(false);
            }}
          />
        )}
        <div className="task-float-anchor">
          <TaskBoardPanel
            key={current}
            conversation={current}
            changeKey={events.length}
            people={[{ id: "human", name: "You" }, ...members.map((m) => ({ id: m.id, name: m.name }))]}
          />
        </div>
        {autoRunAsk && (
          <div className="autorun-overlay" role="dialog" aria-modal="true">
            <div className="autorun-dialog">
              <h3>Auto-run every command in this chat?</h3>
              <p>
                Every command box an AI posts in this chat will run right away, without asking. That includes
                commands that delete or overwrite files, rewrite git history, install or uninstall software,
                stop programs or need admin rights (your computer still shows its own password prompt).
              </p>
              <p>
                Anything that reaches this chat can propose a command, such as a pasted log or a web page an AI
                reads, and it will run without you seeing it first. Only this chat is affected: every new chat
                starts with auto-run off. Turn it off any time with the red Auto-run button next to Request App Update.
              </p>
              <div className="autorun-buttons">
                <button type="button" className="btn deny" onClick={() => setAutoRunAsk(null)}>Cancel</button>
                <button type="button" className="btn approve" onClick={confirmAutoRun}>Turn on and run this box</button>
              </div>
            </div>
          </div>
        )}
        <section className="messages" ref={messagesRef} onScroll={onMessagesScroll}>
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
                  <span className="who">{senderName(e.sender, agents, e.via)}</span>
                  {e.to.length > 0 && e.sender === "human" && (
                    <span className="to">→ {e.to.map((t) => senderName(t, agents)).join(", ")}</span>
                  )}
                  <span className="when">{time(e.ts)}</span>
                  <button
                    type="button"
                    className="msg-reply"
                    title={`Reply to ${senderName(e.sender, agents, e.via)}: quotes this message in the prompt box`}
                    aria-label={`Reply to ${senderName(e.sender, agents, e.via)}`}
                    onClick={() => replyTo(e)}
                  >
                    ↩
                  </button>
                  {e.usage && e.usage.total_tokens > 0 && (
                    <span
                      className="tokens-tag"
                      aria-label={e.usage.output_rate_milli !== undefined ? `Provider-reported completion rate: ${(e.usage.output_rate_milli / 1000).toFixed(1)} tokens per second` : undefined}
                      title={`Turn usage:\n• Total: ${num(e.usage.total_tokens)}\n• Input: ${num(e.usage.input_tokens)}\n• Output: ${num(e.usage.output_tokens)}${e.usage.thinking_tokens ? `\n• Thinking: ${num(e.usage.thinking_tokens)}` : ""}${e.usage.cache_read_tokens ? `\n• Cache read: ${num(e.usage.cache_read_tokens)}` : ""}`}
                    >
                      {formatTokens(e.usage.total_tokens)} tokens
                      {e.usage.output_rate_milli !== undefined && ` · ${(e.usage.output_rate_milli / 1000).toFixed(1)} tok/s`}
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
          <div ref={endRef} />
          {awayFromLatest && (
            <div className="jump-latest-wrap">
              <button type="button" className="jump-latest" onClick={scrollToLatest} title="Jump to the newest message" aria-label="Jump to the newest message">
                {"\u2193"}
                {unseen > 0 ? ` ${unseen} new` : ""}
              </button>
            </div>
          )}
        </section>

        <div className="thinking-slot">
          {busy && (
            <div className={`thinking ${turn?.agent ? `from-${turn.agent}` : ""}`}>
              <span className="pulse" aria-hidden="true" />
              <div className="thinking-text" tabIndex={0} role="region" aria-label="AI activity">
                <TypedText text={thinking ?? "Working"} enabled={typedActivity} />
                {turn && turn.rounds > 1 ? ` · round ${turn.round} of ${turn.rounds}` : ""}
                <span className="cursor" aria-hidden="true">▍</span>
              </div>
            </div>
          )}
        </div>

        <footer className={`composer ${dropping ? "dropping" : ""}`}>
          {error && (
            <div className="error" onClick={() => setError(null)}>
              {error}
            </div>
          )}
          {pendingItems.length > 0 && (council?.auto_run_commands || chatAutoRunFor === current) && (
            <RequestTray
              items={pendingItems}
              index={trayIndex}
              agents={agents}
              answered={answered}
              deciding={decidingRequests}
              decide={decide}
              respond={respondToRequest}
              commands={commandActions}
              onFocus={focusTray}
              onShow={showInChat}
              autoRun={!!council?.auto_run_commands || chatAutoRun}
            />
          )}
          <div className="composer-controls">
            <span className="section-label">Ask</span>
            {members.map((a) => (
              <label
                key={a.id}
                className={`chip chip-${a.id} ${targets.includes(a.id) ? "on" : ""}${isExhausted(a) ? " chip-exhausted" : ""}`}
                title={isExhausted(a) ? `${a.name} is out of usage, so it's switched off until its limit resets. Click to ask it anyway.` : undefined}
              >
                <input type="checkbox" checked={targets.includes(a.id)} onChange={() => toggleTarget(a.id)} />
                {a.name}
                {!readyIds.has(a.id) && <span className="chip-warn" title={a.hint}>!</span>}
              </label>
            ))}
            <span className="spacer" />
            <button
              type="button"
              className="btn small respond-request"
              disabled={totalUnanswered === 0}
              onClick={jumpToNextRequest}
              title={totalUnanswered ? `Scroll to the next request or command waiting for you in the chat (${totalUnanswered} waiting); press again for the one after` : "No unanswered requests"}
            >
              Next request{totalUnanswered > 0 ? ` · ${totalUnanswered}` : ""}
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
            {current && chatAutoRun && (
              <button
                type="button"
                className="btn small autorun-toggle"
                onClick={stopAutoRun}
                title="Every command box in this chat runs without asking, and Consilium approves ordinary requests for you. Click to turn auto-run off."
              >
                Auto-run on · stop
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
              {(!speaking || interrupting) && <VoiceVisualizer speaker="user" active={voice} />}
              {speaking && interrupting ? (
                <span className="voice-heard">
                  {heard || "Paused. Listening…"} <em>(reading carries on in 5 seconds if you say nothing)</em>
                </span>
              ) : speaking ? (
                <>
                  <span>Reading {speakingAgent ? `${senderName(speakingAgent, agents)}’s` : "the"} reply aloud…</span>
                  <button title="Stop reading this reply aloud and move on to the next" className="btn small" onClick={() => speakerRef.current?.skip()}>
                    Skip
                  </button>
                </>
              ) : canListen ? (
                <span className="voice-heard">{heard || "Listening… speak, and your message is sent when you pause. Talk over a reply to interrupt it. Commands start with “Consilium”, like “Consilium, open settings”."}</span>
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
              <button className="btn ghost" onClick={attachClipboardImage} disabled={!current}
                title="Attach a copied screenshot or image from the clipboard" aria-label="Attach clipboard image">
                📋
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
                onClick={() => void send()}
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
            <p className="theme-hint">You can create your own theme by asking the AIs.</p>
            <Suspense fallback={null}>
              <BackdropSettings prefs={prefs.backdrop} modes={backdropModes} update={updateBackdrop} />
            </Suspense>
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
                  onClick={() => setWallpaperRefresh((request) => request + 1)}
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
            <label className="toggle">
              <input
                type="checkbox"
                checked={council?.isolate_agents ?? false}
                onChange={(e) => {
                  const on = e.target.checked;
                  api.setIsolateAgents(on).then(() => setCouncil((c) => (c ? { ...c, isolate_agents: on } : c))).catch((err) => setError(`Couldn't update setting: ${err}`));
                }}
              />
              Isolated copies in Full capacity (separate git worktree on branch <code>consilium/&lt;id&gt;</code> for each AI)
            </label>
            <p className="muted small">
              When on, Full-capacity AIs make changes in their own branch. Their edits don't touch your checkout until you review and merge them in the Branches panel.
            </p>
            <label className="toggle">
              <input
                type="checkbox"
                checked={council?.auto_run_commands ?? false}
                onChange={(e) => {
                  const on = e.target.checked;
                  api.setAutoRunCommands(on).then(() => setCouncil((c) => (c ? { ...c, auto_run_commands: on } : c))).catch((err) => setError(`Couldn't update setting: ${err}`));
                }}
              />
              Always run command boxes from Full-capacity AIs on this computer
            </label>
            <p className="muted small">
              {terminalName} commands an AI posts run by themselves and the output goes back to it, with no Run click.
              Commands that need admin rights or install software, and ones Consilium refuses, still ask.
            </p>
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
            <div className="section-label">Auto-continue</div>
            <label className="toggle">
              <input type="checkbox" checked={council?.auto_continue ?? true} onChange={(e) => toggleAutoContinue(e.target.checked)} />
              Keep working through the task board without being told to continue
            </label>
            <p className="muted small">
              When a turn ends with tasks still open, the AIs start another round 20 seconds later. It pauses after 8 rounds
              on its own, after 2 rounds that change nothing on the board, while a request waits for your answer, and when
              you press Stop. Any message from you starts it again.
            </p>
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
            <div className="section-label">Saved approvals</div>
            <p className="muted small">
              Admin and install commands you chose to <b>Always allow</b>. Any AI may run these exact commands on this
              computer without asking again; anything else still asks. Your computer still shows its own password prompt.
            </p>
            {(council?.saved_grants ?? []).length === 0 ? (
              <p className="muted small">None yet.</p>
            ) : (
              <ul className="saved-grants">
                {(council?.saved_grants ?? []).map((grant) => {
                  const install = grant.startsWith("install\u0000");
                  return (
                    <li key={grant}>
                      <span className="saved-grant-kind">{install ? "Install" : "Admin"}</span>
                      <code>{install ? grant.slice("install\u0000".length) : grant}</code>
                      <button
                        title="Stop always allowing this command; the AIs will ask again"
                        className="btn ghost small-btn"
                        onClick={() => api.forgetApproval(grant).then(setCouncil).catch((e) => setError(`Couldn't remove it: ${e}`))}
                      >
                        Remove
                      </button>
                    </li>
                  );
                })}
              </ul>
            )}
            <div className="section-label">Tools</div>
            <Suspense fallback={null}>
              <OfflineSkills copyText={copyText} />
              <EnvironmentAudit key={current ?? ""} conversation={current ?? undefined} />
            </Suspense>
            <ToolsSettings
              ais={members.map((a) => ({ id: a.id, name: a.name }))}
              onSend={async (text, to) => {
                if (!current) throw new Error("open a conversation first");
                await api.send(current, text, [to], 1);
              }}
            />
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
            <div className="section-label">Security scans</div>
            <SecurityScanSettings />
            <div className="section-label">Keyboard shortcut</div>
            <p className="muted small">
              {quickSummonAvailable
                ? `Press ${OS === "mac" ? "Command+Option" : "Ctrl+Alt"}+Space to bring Consilium forward and focus the current draft.`
                : "The global quick-summon shortcut is unavailable on this computer."}
            </p>
            <div className="section-label">Updates</div>
            <label className="toggle">
              <input
                type="checkbox"
                checked={prefs.autoUpdate}
                onChange={(e) => setPrefs({ ...prefs, autoUpdate: e.target.checked })}
              />
              Check GitHub for a new version every 30 minutes
            </label>
            <label className="toggle">
              <input
                type="checkbox"
                checked={council?.auto_install_updates ?? true}
                onChange={(e) => {
                  const on = e.target.checked;
                  api
                    .setAutoInstallUpdates(on)
                    .then(() => setCouncil((c) => (c ? { ...c, auto_install_updates: on } : c)))
                    .catch((err) => setError(`Couldn't update setting: ${err}`));
                }}
              />
              Install updates and restart automatically (the AIs' work resumes after; waits for running commands)
            </label>
            <div className="row">
              <button title="Check GitHub for a newer Consilium release now" className="btn ghost" onClick={checkNow}>
                Check GitHub now
              </button>
              <button
                className="btn primary"
                disabled={!staged || updating}
                onClick={updateNow}
                title={
                  staged
                    ? `Pause the AIs, install ${staged.version} (now ${staged.current}) and restart. Their work resumes after the restart.`
                    : update
                      ? `${update.latest} is on GitHub but hasn't finished downloading yet. This button lights up when it's ready to install.`
                      : "You're on the newest version. This button lights up when a new one is ready to install."
                }
              >
                {updating ? "Updating…" : staged ? `Update Now · ${staged.version}` : "Update Now"}
              </button>
              <button title="Open the list of all Consilium releases on GitHub" className="link" onClick={() => openUrl(RELEASES_URL)}>
                All releases
              </button>
              <span className="muted">
                {downloadStatus.active ? `Downloading Consilium ${downloadStatus.version || "update"} from GitHub…` : updateNote}
              </span>
            </div>
            {downloadError && <p className="error small" role="status">{downloadError}</p>}
            <div className="row wrap">
              <button className="btn ghost" disabled={requestingInstallers} onClick={() => void buildInstallers()}>
                {requestingInstallers ? "Requesting installer builds…" : "Build draft installers"}
              </button>
              <button className="link" onClick={() => openUrl("https://github.com/Ag3nt47/consilium-ai/actions/workflows/release.yml")}>
                Installer builds
              </button>
            </div>
            {installerRequest && (
              <p className="muted small" role="status">
                {installerRequest.message}{" "}
                {installerRequest.url && (
                  <button className="link" onClick={() => openUrl(installerRequest.url!)}>Open this build</button>
                )}
              </p>
            )}
            <p className="muted small">
              Consilium checks the downloaded installer against GitHub's fingerprint.
              {" "}{council?.auto_install_updates ?? true
                ? "The app installs the update and restarts automatically after running commands finish."
                : "Click Update Now when the installer is ready."}
            </p>
            <p className="muted small">
              Conversations stay on this computer. CLI agents use their provider's existing sign-in.
              Optional API-agent keys are stored locally.
            </p>
            <Suspense fallback={null}>
              <UpgradeProgress />
            </Suspense>
            <div className="row end">
              <button title="Close Settings" className="btn primary" onClick={() => setShowSettings(false)}>
                Done
              </button>
            </div>
          </div>
        </div>
      )}
      {addPanel === "choose" && (
        <div className="modal-backdrop" onClick={closeAddPanel}>
          <div className="modal confirm-modal" role="dialog" aria-modal="true" aria-labelledby="add-title"
            onClick={(e) => e.stopPropagation()} onKeyDown={(e) => { if (e.key === "Escape") closeAddPanel(); }}>
            <h2 id="add-title">Add to Consilium</h2>
            <button autoFocus className="btn ghost add-choice" onClick={() => setAddPanel("ai")}>
              <span aria-hidden="true">🤖</span> Add an AI
              <small>Set up a provider or a local model, check it, and add it to the council.</small>
            </button>
            <button className="btn ghost add-choice" onClick={() => setAddPanel("device")}>
              <span aria-hidden="true">💻</span> Connect a device
              <small>Pair a Windows or Mac computer and test the connection.</small>
            </button>
            <div className="row end"><button className="btn ghost" onClick={closeAddPanel}>Cancel</button></div>
          </div>
        </div>
      )}
      {addPanel === "device" && (
        <div className="modal-backdrop" onClick={closeAddPanel}>
          <div className="modal" role="dialog" aria-modal="true" aria-labelledby="add-device-title"
            onClick={(e) => e.stopPropagation()} onKeyDown={(e) => { if (e.key === "Escape") closeAddPanel(); }}>
            <h2 id="add-device-title" tabIndex={-1} ref={focusAddHeading}>Connect a device</h2>
            <ol className="addai-steps">
              <li>Open Consilium on both computers, connect them to the same network, and enable sync below.</li>
              <li>Generate a pairing code on one computer. Enter its address and code on the other.</li>
              <li>Under Paired Computers, select Test Connection to verify the handshake and routes.</li>
            </ol>
            <LinkSettings />
            <div className="row end">
              <button className="btn ghost" onClick={() => setAddPanel("choose")}>Back</button>
              <button className="btn primary" onClick={closeAddPanel}>Done</button>
            </div>
          </div>
        </div>
      )}
      {addPanel === "ai" && (
        <div className="modal-backdrop" onClick={closeAddPanel}>
          <div className="modal" role="dialog" aria-modal="true" aria-labelledby="add-ai-title"
            onClick={(e) => e.stopPropagation()} onKeyDown={(e) => { if (e.key === "Escape") closeAddPanel(); }}>
            <h2 id="add-ai-title" tabIndex={-1} ref={focusAddHeading}>Add an AI</h2>
            <ol className="addai-steps">
              <li>Choose a provider below, or add a custom endpoint for a local or hosted model.</li>
              <li>Complete the steps on its card, then use Check setup to refresh its status.</li>
              <li>Add the model to the council. Test council replies sends one short prompt to each active member and may use provider allowance.</li>
            </ol>
            <p className="muted small">
              Most AIs join through their company's official command-line tool and your own account. Grok and DeepSeek
              have no such tool, so they use an API key you paste here. You can also add any other AI below: a
              service that takes your own key, or a model running on your computer: it is saved only on this computer, never in the chat.
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
                    {a.custom && (
                      <button
                        title="Delete this AI you added, and its saved key"
                        className="link danger-link"
                        onClick={() => api.removeCustomAi(a.id).then(refreshAgents).catch((e) => setError(String(e)))}
                      >
                        delete
                      </button>
                    )}
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
            <CustomAiForm
              availableGiB={pressure ? (pressure.available_memory_mb ?? pressure.total_memory_mb * (1 - pressure.memory_percent / 100)) / 1024 : undefined}
              onAdded={(id) => {
                refreshAgents();
                setToast(`Added @${id}. Press "Add" on its card to bring it into the council.`);
                setTimeout(() => setToast(null), 7000);
              }}
              onError={(e) => setError(e)}
            />
            <div className="section-label">In the council</div>
            {members.map((a) => {
              const check = checks[a.id];
              return (
                <div key={a.id} className="member-row">
                  <span className={`agent-name agent-${a.id}`}>{a.name}</span>
                  <span className="muted small">{a.company}</span>
                  {check && (
                    <small className="muted" role="status" title={check === "running" ? undefined : check.detail}>
                      {check === "running" ? "Testing…" : check.ok ? "Replied" : "Couldn't reply"}
                    </small>
                  )}
                  <span className="spacer" />
                  <button title="Take this AI out of the council (at least one must stay)" className="link danger-link" disabled={members.length < 2} onClick={() => removeMember(a)}>
                    remove
                  </button>
                </div>
              );
            })}
            <div className="row end">
              <button className="btn ghost" onClick={() => setAddPanel("choose")}>Back</button>
              <button title="Check install and sign-in status" className="btn ghost" onClick={refreshAgents} disabled={agentsRefreshing}>
                {agentsRefreshing ? "Checking…" : "Check setup"}
              </button>
              <button title="Send one short test prompt to each active council member" className="btn ghost"
                onClick={runSelfCheck} disabled={!members.length || Object.values(checks).includes("running")}>
                Test council replies
              </button>
              <button title="Close Add AI" className="btn primary" onClick={closeAddPanel}>
                Done
              </button>
            </div>
          </div>
        </div>
      )}
      {openAppAsk && (
        <div className="modal-backdrop" onClick={() => setOpenAppAsk(null)}>
          <div
            className="modal confirm-modal"
            role="alertdialog"
            aria-labelledby="confirm-open-app-title"
            onClick={(e) => e.stopPropagation()}
            onKeyDown={(e) => {
              if (e.key === "Escape") setOpenAppAsk(null);
            }}
          >
            <h2 id="confirm-open-app-title">Open {openAppAsk}?</h2>
            <p>You asked by voice to open <strong>{openAppAsk}</strong>. Voice alone can't open programs, so please confirm.</p>
            <div className="confirm-actions">
              <button className="btn" autoFocus onClick={() => setOpenAppAsk(null)} title="Don't open it">
                Cancel
              </button>
              <button className="btn primary" onClick={confirmOpenApp} title={`Open ${openAppAsk}`}>
                Open {openAppAsk}
              </button>
            </div>
          </div>
        </div>
      )}
      {confirmDelete && meta && (
        <div className="modal-backdrop" onClick={() => setConfirmDelete(false)}>
          <div
            className="modal confirm-modal"
            role="alertdialog"
            aria-labelledby="confirm-delete-title"
            onClick={(e) => e.stopPropagation()}
            onKeyDown={(e) => {
              if (e.key === "Escape") setConfirmDelete(false);
            }}
          >
            <h2 id="confirm-delete-title">Delete this conversation?</h2>
            <p>
              <strong>{meta.title}</strong> and all of its messages will be deleted permanently. This can't be undone.
            </p>
            <div className="confirm-actions">
              <button className="btn" autoFocus onClick={() => setConfirmDelete(false)} title="Keep the conversation">
                Cancel
              </button>
              <button className="btn danger" onClick={deleteConfirmed} title="Delete the conversation permanently">
                Delete permanently
              </button>
            </div>
          </div>
        </div>
      )}

      {deleteAllStep === 1 && (
        <div className="modal-backdrop" onClick={closeDeleteAll}>
          <div
            className="modal confirm-modal"
            role="alertdialog"
            aria-labelledby="confirm-delete-all-title"
            onClick={(e) => e.stopPropagation()}
            onKeyDown={(e) => {
              if (e.key === "Escape") closeDeleteAll();
            }}
          >
            <h2 id="confirm-delete-all-title">Delete all conversations?</h2>
            <p>
              This will delete all <strong>{conversations.length}</strong> conversation{conversations.length === 1 ? "" : "s"},
              including their messages, attachments, task boards and handoff notes.
            </p>
            <p className="muted small">A second confirmation is required before anything is deleted.</p>
            <div className="confirm-actions">
              <button className="btn" autoFocus onClick={closeDeleteAll} title="Keep every conversation">
                Cancel
              </button>
              <button className="btn danger" onClick={() => setDeleteAllStep(2)} title="Continue to the final confirmation">
                Continue
              </button>
            </div>
          </div>
        </div>
      )}

      {deleteAllStep === 2 && (
        <div className="modal-backdrop" onClick={closeDeleteAll}>
          <div
            className="modal confirm-modal"
            role="alertdialog"
            aria-labelledby="final-delete-all-title"
            onClick={(e) => e.stopPropagation()}
            onKeyDown={(e) => {
              if (e.key === "Escape") closeDeleteAll();
            }}
          >
            <h2 id="final-delete-all-title">Permanently delete every conversation?</h2>
            <p>
              <strong>Final confirmation:</strong> this can't be undone. Type <strong>DELETE</strong> to confirm.
              A new empty conversation will open afterwards.
            </p>
            <input
              className="delete-all-input"
              autoFocus
              value={deleteAllText}
              disabled={deletingAll}
              placeholder="DELETE"
              aria-label="Type DELETE to confirm"
              onChange={(e) => setDeleteAllText(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") deleteAllConfirmed();
              }}
            />
            <div className="confirm-actions">
              <button className="btn" disabled={deletingAll} onClick={closeDeleteAll} title="Keep every conversation">
                Cancel
              </button>
              <button
                className="btn danger"
                disabled={deletingAll || deleteAllText.trim().toUpperCase() !== "DELETE"}
                onClick={deleteAllConfirmed}
                title="Delete every conversation permanently"
              >
                {deletingAll ? "Deleting…" : "Delete all permanently"}
              </button>
            </div>
          </div>
        </div>
      )}

      {showRestartModal && (
        <div className="modal-backdrop" onClick={() => !restarting && setShowRestartModal(false)}>
          <div
            className="modal confirm-modal"
            role="alertdialog"
            aria-labelledby="confirm-restart-title"
            onClick={(e) => e.stopPropagation()}
            onKeyDown={(e) => {
              if (e.key === "Escape" && !restarting) setShowRestartModal(false);
            }}
          >
            <h2 id="confirm-restart-title">Restart Consilium</h2>
            <p className="restart-prompt-question"><strong>Are you sure?</strong></p>
            <p className="muted small">
              Work in progress will pause and resume automatically after restart.
            </p>

            <div className="restart-computer-selection">
              <div className="section-label">Choose which Computers to restart:</div>
              <div className="restart-computer-list">
                {restartComputers.map((c) => (
                  <label key={c.id} className="restart-computer-item">
                    <input
                      type="checkbox"
                      checked={selectedRestartComputers.includes(c.id)}
                      disabled={restarting || (!c.isSelf && !c.connected)}
                      onChange={(e) => {
                        if (e.target.checked) {
                          setSelectedRestartComputers((prev) => [...prev, c.id]);
                        } else {
                          setSelectedRestartComputers((prev) => prev.filter((id) => id !== c.id));
                        }
                      }}
                    />
                    <span className="restart-computer-name">
                      <strong>{c.name}</strong>
                      {!c.isSelf && (
                        <span className={`peer-badge ${c.connected ? "connected" : "offline"}`}>
                          {c.connected ? "connected" : "offline"}
                        </span>
                      )}
                    </span>
                  </label>
                ))}
              </div>
            </div>

            <div className="confirm-actions">
              <button
                className="btn"
                autoFocus
                onClick={() => setShowRestartModal(false)}
                disabled={restarting}
                title="Cancel restart"
              >
                Cancel
              </button>
              <button
                className="btn ghost"
                onClick={() => handleRestart(selectedRestartComputers)}
                disabled={restarting || selectedRestartComputers.length === 0}
                title="Restart only the selected computers"
              >
                {restarting ? "Restarting…" : "Restart selected"}
              </button>
              <button
                className="btn danger"
                onClick={() => handleRestart(["all"])}
                disabled={restarting}
                title="Restart this computer and all linked computers"
              >
                {restarting ? "Restarting…" : "Restart all"}
              </button>
            </div>
          </div>
        </div>
      )}

      {budgetWarning && (
        <div className="modal-backdrop" onClick={() => setBudgetWarning(null)}>
          <div
            className="modal"
            role="alertdialog"
            aria-modal="true"
            aria-labelledby="token-warning-title"
            aria-describedby="token-warning-detail"
            onClick={(e) => e.stopPropagation()}
            onKeyDown={(e) => { if (e.key === "Escape") setBudgetWarning(null); }}
          >
            <h2 id="token-warning-title" className="error">REQUEST — Daily token warning</h2>
            <p id="token-warning-detail">{budgetWarning.message}</p>
            <div className="row end">
              <button className="btn" autoFocus onClick={() => setBudgetWarning(null)}>Keep draft</button>
              <button
                className="btn primary"
                disabled={budgetWarning.id !== current || budgetWarning.text !== (draftBox.current?.get() ?? "")}
                onClick={() => {
                  setBudgetWarning(null);
                  void send(true);
                }}
              >
                Send anyway
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
            <label className="row">
              Daily Discuss warning
              <input
                type="number"
                min={0}
                max={Number.MAX_SAFE_INTEGER}
                step={1}
                value={prefs.dailyTokenWarning || ""}
                placeholder="Off"
                onChange={(e) => {
                  const value = e.currentTarget.valueAsNumber;
                  if (e.currentTarget.value === "" || (Number.isSafeInteger(value) && value >= 0)) {
                    setPrefs((previous) => ({ ...previous, dailyTokenWarning: value || 0 }));
                  }
                }}
              />
            </label>
            <p className="muted small">Enter a token count to warn before starting Discuss once today's recorded usage reaches it. Clear the field to turn it off.</p>
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
                            <td>{num(u.input_tokens)}</td>
                            <td>{num(u.output_tokens)}</td>
                            <td>{u.thinking_tokens ? num(u.thinking_tokens) : "—"}</td>
                            <td>{u.cache_read_tokens ? num(u.cache_read_tokens) : "—"}</td>
                            <td><b>{num(u.total_tokens)}</b></td>
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
                            <td>{num(u.input_tokens)}</td>
                            <td>{num(u.output_tokens)}</td>
                            <td>{u.thinking_tokens ? num(u.thinking_tokens) : "—"}</td>
                            <td>{u.cache_read_tokens ? num(u.cache_read_tokens) : "—"}</td>
                            <td><b>{num(u.total_tokens)}</b></td>
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
