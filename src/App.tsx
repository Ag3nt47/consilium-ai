import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { api, onEvent, onTurn, senderName, type AgentInfo, type ChatEvent, type Conversation, type TurnState } from "./api";
import { checkForUpdate, RELEASES_URL, type UpdateInfo } from "./updates";

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

function time(ts: number) {
  return new Date(ts).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

/** Message text with REQUEST: lines shown as red request callouts. */
function MessageBody({ text }: { text: string }) {
  const lines = text.split("\n");
  return (
    <div className="body">
      {lines.map((line, i) => {
        const m = /^\s*REQUEST:\s*(.*)$/i.exec(line);
        if (m && m[1].trim()) {
          return (
            <div key={i} className="request">
              <span className="request-label">REQUEST:</span> {m[1]}
            </div>
          );
        }
        return <div key={i} className={line.trim() ? "line" : "gap"}>{line}</div>;
      })}
    </div>
  );
}

function agentState(a: AgentInfo): "ready" | "signin" | "missing" {
  return a.available ? "ready" : a.path ? "signin" : "missing";
}

function AgentDot({ agent, onSetup }: { agent: AgentInfo; onSetup: (a: AgentInfo) => void }) {
  const state = agentState(agent);
  const label = { ready: "Ready", signin: "Not signed in", missing: "Not installed" }[state];
  return (
    <div className={`agent agent-${agent.id}`} title={agent.hint || `${agent.tool} ${agent.version ?? ""}`}>
      <span className={`dot dot-${state}`} />
      <span className="agent-name">{agent.name}</span>
      {state === "ready" ? (
        <span className="agent-state">{label}</span>
      ) : (
        <button className="link setup-link" onClick={() => onSetup(agent)} title={agent.hint}>
          {state === "missing" ? "Install" : "Sign in"}
        </button>
      )}
      <span className="agent-kind">local CLI · {agent.tool}</span>
    </div>
  );
}

export default function App() {
  const [prefs, setPrefs] = useState<Prefs>(loadPrefs);
  const [agents, setAgents] = useState<AgentInfo[]>([]);
  const [conversations, setConversations] = useState<Conversation[]>([]);
  const [current, setCurrent] = useState<string | null>(null);
  const [events, setEvents] = useState<ChatEvent[]>([]);
  const [turn, setTurn] = useState<TurnState | null>(null);
  const [draft, setDraft] = useState("");
  const [targets, setTargets] = useState<string[]>(["gpt", "claude"]);
  const [discuss, setDiscuss] = useState(false);
  const [rounds, setRounds] = useState(3);
  const [error, setError] = useState<string | null>(null);
  const [showSettings, setShowSettings] = useState(false);
  const [update, setUpdate] = useState<UpdateInfo | null>(null);
  const [updateNote, setUpdateNote] = useState<string>("");
  const [toast, setToast] = useState<string | null>(null);
  const currentRef = useRef<string | null>(null);
  const endRef = useRef<HTMLDivElement>(null);

  currentRef.current = current;

  useEffect(() => {
    document.documentElement.dataset.theme = prefs.theme;
    document.documentElement.dataset.effects = prefs.effects ? "on" : "off";
    savePrefs(prefs);
  }, [prefs]);

  const refreshAgents = useCallback(() => {
    api.agents().then(setAgents).catch((e) => setError(String(e)));
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
    });
    const un2 = onTurn((t) => {
      if (t.conversation === currentRef.current) setTurn(t.state === "running" ? t : null);
    });
    return () => {
      un1.then((f) => f());
      un2.then((f) => f());
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
      if (ids.includes(current)) setTurn({ conversation: current, state: "running", agent: null, round: 0, rounds: 0 });
    });
  }, [current]);

  useEffect(() => {
    endRef.current?.scrollIntoView({ behavior: prefs.effects ? "smooth" : "auto" });
  }, [events, turn, prefs.effects]);

  const meta = conversations.find((c) => c.id === current);
  const busy = turn !== null;
  const thinking = turn?.agent ? senderName(turn.agent, agents) : null;
  const readyIds = useMemo(() => new Set(agents.filter((a) => a.available).map((a) => a.id)), [agents]);

  async function send() {
    if (!current || !draft.trim() || busy) return;
    setError(null);
    try {
      await api.send(current, draft, targets, discuss ? rounds : 1);
      setDraft("");
    } catch (e) {
      setError(String(e));
    }
  }

  async function newConversation() {
    const c = await api.create();
    await refreshList();
    setCurrent(c.id);
  }

  async function exportCurrent() {
    if (!current) return;
    const md = await api.exportMarkdown(current);
    await navigator.clipboard.writeText(md);
    setToast("Conversation copied as Markdown");
    setTimeout(() => setToast(null), 2500);
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

  const notReady = agents.filter((a) => !a.available);

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
            <button className="link" onClick={refreshAgents} title="Check again">
              refresh
            </button>
          </div>
          {agents.map((a) => (
            <AgentDot key={a.id} agent={a} onSetup={setupAgent} />
          ))}
        </div>
        <button className="btn ghost wide" onClick={() => setShowSettings(true)}>
          Settings
        </button>
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
          <h1 onDoubleClick={renameCurrent} title="Double-click to rename">
            {meta?.title ?? "Consilium"}
          </h1>
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
                Ask ChatGPT and Claude together. Pick who answers below, or switch to <b>Discuss</b> to let
                them work it out between themselves for a few rounds. Address someone directly with{" "}
                <code>@gpt</code>, <code>@claude</code> or <code>@all</code>.
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
                <MessageBody text={e.text} />
              </article>
            ) : (
              <div key={e.id} className={`note note-${e.kind}`}>
                {e.text}
              </div>
            ),
          )}
          {busy && (
            <div className="thinking">
              <span className="pulse" /> {thinking ? `${thinking} is thinking` : "Working"}
              {turn && turn.rounds > 1 ? ` · round ${turn.round} of ${turn.rounds}` : ""}
              <span className="cursor">▍</span>
            </div>
          )}
          <div ref={endRef} />
        </section>

        <footer className="composer">
          {error && (
            <div className="error" onClick={() => setError(null)}>
              {error}
            </div>
          )}
          <div className="composer-controls">
            <span className="section-label">Ask</span>
            {agents.map((a) => (
              <label key={a.id} className={`chip chip-${a.id} ${targets.includes(a.id) ? "on" : ""}`}>
                <input type="checkbox" checked={targets.includes(a.id)} onChange={() => toggleTarget(a.id)} />
                {a.name}
                {!readyIds.has(a.id) && <span className="chip-warn" title={a.hint}>!</span>}
              </label>
            ))}
            <span className="spacer" />
            <label className={`chip ${discuss ? "on" : ""}`} title="Let the AIs respond to each other for several rounds">
              <input type="checkbox" checked={discuss} onChange={(e) => setDiscuss(e.target.checked)} />
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
          <div className="composer-row">
            <textarea
              value={draft}
              placeholder="Message the council…  (Enter to send, Shift+Enter for a new line)"
              onChange={(e) => setDraft(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.shiftKey) {
                  e.preventDefault();
                  send();
                }
              }}
              rows={3}
            />
            {busy ? (
              <button className="btn stop" onClick={() => current && api.stop(current)}>
                Stop
              </button>
            ) : (
              <button className="btn primary" onClick={send} disabled={!draft.trim()}>
                Send
              </button>
            )}
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
              Your conversations stay on this computer; ChatGPT and Claude are reached through their own
              command-line tools with your existing sign-ins, and Consilium never sees a password or key.
            </p>
            <div className="row end">
              <button className="btn primary" onClick={() => setShowSettings(false)}>
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
