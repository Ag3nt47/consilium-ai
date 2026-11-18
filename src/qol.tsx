// Persona chips above the prompt box, and the audit log drawer.
// Both are screen-only, so they can arrive through Apply Now without a restart.
import { clock, dateAndTime } from "./fmt";
import { useMemo, useState } from "react";
import type { ChatEvent } from "./api";

export interface Persona {
  id: string;
  label: string;
  hint: string;
  /** Prepended to the human's message so every AI answers in this role. */
  brief: string;
}

export const PERSONAS: Persona[] = [
  {
    id: "security",
    label: "Security Auditor",
    hint: "Answer as security reviewers: threats, secrets, permissions, unsafe input, with severity",
    brief:
      "Answer as a security auditor: look for vulnerabilities, leaked secrets, unsafe input handling, excess permissions and supply-chain risk. Rank findings by severity and give a concrete fix for each.",
  },
  {
    id: "refactor",
    label: "Code Refactor",
    hint: "Answer as refactoring reviewers: simpler, smaller, behaviour unchanged, with tests",
    brief:
      "Answer as a refactoring reviewer: make the code simpler and smaller without changing behaviour, remove duplication, keep public interfaces stable, and say how the change is tested.",
  },
  {
    id: "debug",
    label: "Debugger",
    hint: "Answer as debuggers: reproduce, find the root cause, then the smallest fix",
    brief:
      "Answer as a debugger: reproduce the problem first, find the root cause rather than the symptom, then propose the smallest fix and a test that would have caught it.",
  },
  {
    id: "explain",
    label: "Explain Simply",
    hint: "Answer in plain language for a non-expert, short and without jargon",
    brief: "Answer in plain language for a non-expert: short sentences, no jargon, and an everyday example where it helps.",
  },
  {
    id: "general",
    label: "General CLI",
    hint: "Answer as hands-on command-line helpers: exact commands to run, and what each does",
    brief: "Answer as a hands-on command-line helper: give the exact commands to run, in order, and say what each one does.",
  },
];

const PERSONA_KEY = "consilium.persona";

export function loadPersona(): string {
  try {
    const id = localStorage.getItem(PERSONA_KEY) || "";
    return PERSONAS.some((p) => p.id === id) ? id : "";
  } catch {
    return "";
  }
}

export function savePersona(id: string) {
  try {
    if (id) localStorage.setItem(PERSONA_KEY, id);
    else localStorage.removeItem(PERSONA_KEY);
  } catch {
    /* the choice still applies until the window reloads */
  }
}

/** The message as sent: the persona's brief first, then what the human typed. */
export function withPersona(text: string, personaId: string): string {
  const p = PERSONAS.find((x) => x.id === personaId);
  if (!p || !text.trim()) return text;
  return `(Persona: ${p.label}. ${p.brief})\n\n${text}`;
}

export function PersonaChips({ value, onChange }: { value: string; onChange: (id: string) => void }) {
  return (
    <div className="persona-chips" role="radiogroup" aria-label="Persona">
      <span className="section-label">Persona</span>
      {PERSONAS.map((p) => (
        <button
          key={p.id}
          type="button"
          role="radio"
          aria-checked={value === p.id}
          className={`chip persona ${value === p.id ? "on" : ""}`}
          title={`${p.hint}. Click again to turn it off.`}
          onClick={() => onChange(value === p.id ? "" : p.id)}
        >
          {p.label}
        </button>
      ))}
    </div>
  );
}

export type AuditKind = "approved" | "denied" | "request" | "update" | "council" | "phone" | "error";

export interface AuditEntry {
  id: string;
  ts: number;
  kind: AuditKind;
  who: string;
  text: string;
}

const UPDATE_RE = /\b(updated from|installing \d|Update Now|Apply Now|restarted)\b/i;
const PHONE_RE = /\b(Signal|from your phone|phone)\b/i;

/** Every approval, request, update, council change, phone message and error, newest first. */
export function auditEntries(events: ChatEvent[], names: Record<string, string>): AuditEntry[] {
  const out: AuditEntry[] = [];
  const who = (s: string, via?: string | null) => (s === "human" ? (via ? `You (${via})` : "You") : via ? `${names[s] || s} (${via})` : names[s] || s);
  for (const e of events) {
    const first = e.text.split("\n")[0].trim();
    if (e.kind === "error") {
      out.push({ id: `${e.id}-err`, ts: e.ts, kind: "error", who: who(e.sender, e.via), text: first });
      continue;
    }
    if (e.sender === "human" && /^(Approved|Denied):/.test(first)) {
      out.push({ id: e.id, ts: e.ts, kind: first.startsWith("Approved") ? "approved" : "denied", who: who("human", e.via), text: first.replace(/^(Approved|Denied):\s*/, "") });
      continue;
    }
    if (e.kind === "system") {
      const kind: AuditKind = UPDATE_RE.test(first) ? "update" : PHONE_RE.test(first) ? "phone" : "council";
      out.push({ id: e.id, ts: e.ts, kind, who: "Consilium", text: first });
      continue;
    }
    e.requests?.forEach((r, i) => out.push({ id: `${e.id}-r${i}`, ts: e.ts, kind: "request", who: who(e.sender, e.via), text: r }));
    e.add_ai?.forEach((a, i) => out.push({ id: `${e.id}-a${i}`, ts: e.ts, kind: "council", who: who(e.sender, e.via), text: `Proposed adding ${a}` }));
  }
  return out.sort((a, b) => b.ts - a.ts);
}

const LABELS: Record<AuditKind, string> = {
  approved: "Approved",
  denied: "Denied",
  request: "Request",
  update: "Update",
  council: "Council",
  phone: "Phone",
  error: "Error",
};

export function AuditDrawer({ events, names, onClose }: { events: ChatEvent[]; names: Record<string, string>; onClose: () => void }) {
  const [filter, setFilter] = useState<AuditKind | "">("");
  const all = useMemo(() => auditEntries(events, names), [events, names]);
  const shown = filter ? all.filter((e) => e.kind === filter) : all;
  return (
    <aside className="audit-drawer" aria-label="Audit log">
      <header>
        <b>Audit log</b>
        <span className="muted small">{all.length} entries, newest first</span>
        <span className="spacer" />
        <button className="btn small ghost" onClick={onClose} title="Close the audit log">
          ×
        </button>
      </header>
      <div className="audit-filters">
        <button className={`chip ${filter === "" ? "on" : ""}`} onClick={() => setFilter("")} title="Show every entry">
          All
        </button>
        {(Object.keys(LABELS) as AuditKind[]).map((k) => (
          <button key={k} className={`chip ${filter === k ? "on" : ""}`} onClick={() => setFilter(filter === k ? "" : k)} title={`Show only ${LABELS[k].toLowerCase()} entries`}>
            {LABELS[k]}
          </button>
        ))}
      </div>
      <ol className="audit-list">
        {shown.map((e) => (
          <li key={e.id} className={`audit-${e.kind}`}>
            <span className="audit-kind">{LABELS[e.kind]}</span>
            <time title={dateAndTime(e.ts)}>{clock(e.ts)}</time>
            <span className="audit-who">{e.who}</span>
            <span className="audit-text">{e.text}</span>
          </li>
        ))}
        {!shown.length && <li className="muted small">Nothing logged yet.</li>}
      </ol>
    </aside>
  );
}
