import { useEffect, useState, type CSSProperties } from "react";
import { api, type TaskBoard, type TaskItem, type TaskOp } from "./api";

const STATUS_LABEL: Record<TaskItem["status"], string> = {
  todo: "open",
  doing: "doing",
  blocked: "blocked",
  done: "done",
};

/**
 * The conversation's shared checklist. AIs change it with TASK: lines; the human changes it here.
 * Every change, from either side, is also noted in the chat by Consilium.
 */
export function TaskBoardPanel({
  conversation,
  changeKey,
  people,
}: {
  conversation: string | null;
  /** Changes whenever the conversation gets a new event, so the board is read again. */
  changeKey: number;
  people: { id: string; name: string }[];
}) {
  const [board, setBoard] = useState<TaskBoard>({ tasks: [], next: 0 });
  // Pinned open by a click; otherwise it opens while the pointer or keyboard focus is on it.
  const [pinned, setPinned] = useState(false);
  // Closed by a click: hover and focus don't reopen it until the pointer leaves, so the second
  // click really closes it instead of leaving the hover peek showing.
  const [closed, setClosed] = useState(false);
  const [title, setTitle] = useState("");
  const [error, setError] = useState("");
  const [refresh, setRefresh] = useState(0);

  useEffect(() => {
    if (!conversation) return;
    let active = true;
    api.tasksGet(conversation).then((snapshot) => {
      if (active) setBoard(snapshot);
    }).catch(() => undefined);
    return () => { active = false; };
  }, [conversation, changeKey, refresh]);

  if (!conversation) return null;

  const apply = (op: TaskOp) =>
    api
      .tasksApply(conversation, op)
      .then(() => {
        setRefresh((value) => value + 1);
        setError("");
      })
      .catch((e) => setError(String(e)));

  const nameOf = (id?: string | null) => (id ? people.find((p) => p.id === id)?.name ?? (id === "human" ? "You" : id) : "");
  const done = board.tasks.filter((t) => t.status === "done").length;
  const total = board.tasks.length;
  const pct = total ? Math.round((done * 100) / total) : 0;
  // Open work first, newest finished last.
  const order = { doing: 0, blocked: 1, todo: 2, done: 3 } as const;
  const tasks = [...board.tasks].sort((a, b) => order[a.status] - order[b.status] || a.id - b.id);
  const current = tasks.find((t) => t.status === "doing") ?? tasks.find((t) => t.status === "blocked") ?? tasks.find((t) => t.status === "todo");

  return (
    <div
      className={`task-float${pinned ? " pinned" : ""}${closed ? " closed" : ""}`}
      onMouseLeave={() => setClosed(false)}
      onKeyDown={(e) => {
        if (e.key === "Escape") {
          setPinned(false);
          setClosed(true);
          (document.activeElement as HTMLElement | null)?.blur();
        }
      }}
    >
      <button
        className="task-chip"
        onClick={() => {
          // Keyboard focus would reopen it through :focus-within once the pointer leaves.
          if (pinned) (document.activeElement as HTMLElement | null)?.blur();
          setClosed(pinned);
          setPinned(!pinned);
        }}
        aria-expanded={pinned}
        title={pinned ? "Close the task checklist" : "Shared task checklist: hover to peek, click to keep it open"}
      >
        <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
          <rect x="5" y="4" width="14" height="17" rx="2" />
          <rect x="9" y="2" width="6" height="4" rx="1" />
          <path d="M9 12l2 2 4-4" />
        </svg>
        <span className="task-chip-count">{total ? `${pct}%` : "0"}</span>
        {total > 0 && (
          <span className="task-chip-ring" aria-hidden style={{ "--pct": `${pct}%` } as CSSProperties} />
        )}
      </button>
      <div className="task-pop" role="dialog" aria-label="Shared task checklist">
        <div className="task-pop-head">
          <b>Tasks</b>
          <span title={`${done} of ${total} tasks done`}>{total ? `${done} of ${total} done · ${pct}%` : "none yet"}</span>
        </div>
        {total > 0 && (
          <div className="task-progress" aria-hidden>
            <div style={{ width: `${pct}%` }} />
          </div>
        )}
        {current && (
          <div className={`task-current ${current.status}`}>
            <span className="task-current-label">{current.status === "doing" ? "Now" : current.status === "blocked" ? "Blocked" : "Next"}</span>
            #{current.id} {current.title}
            {current.owner ? <span className="task-current-owner"> · {nameOf(current.owner)}</span> : null}
          </div>
        )}
        <>
          <ul className="task-list">
            {tasks.map((t) => (
              <li key={t.id} className={`task-item ${t.status}`} title={t.note ? `Blocked: ${t.note}` : undefined}>
                <input
                  type="checkbox"
                  checked={t.status === "done"}
                  onChange={() => apply(t.status === "done" ? { op: "reopen", task: `#${t.id}` } : { op: "done", task: `#${t.id}` })}
                  title={t.status === "done" ? "Mark as not done" : "Mark as done"}
                />
                <span className="task-text">
                  <span className="task-title">
                    #{t.id} {t.title}
                  </span>
                  <span className="task-meta">
                    {t.owner && <span className="task-dot" style={{ backgroundColor: `var(--c-${t.owner}, var(--accent))` }} />}
                    {STATUS_LABEL[t.status]}
                    {t.owner ? ` · ${nameOf(t.owner)}` : ""}
                  </span>
                </span>
                <select
                  className="task-assign"
                  value=""
                  onChange={(e) => {
                    const v = e.target.value;
                    if (v === "__drop") apply({ op: "drop", task: `#${t.id}` });
                    else if (v) apply({ op: "hand", task: `#${t.id}`, to: v });
                  }}
                  title="Give this task to someone, or remove it"
                  aria-label={`Change task ${t.id}`}
                >
                  <option value="">⋯</option>
                  {people.map((p) => (
                    <option key={p.id} value={p.id}>
                      Give to {p.name}
                    </option>
                  ))}
                  <option value="__drop">Remove</option>
                </select>
              </li>
            ))}
          </ul>
          <form
            className="task-add"
            onSubmit={(e) => {
              e.preventDefault();
              const t = title.trim();
              if (!t) return;
              apply({ op: "add", title: t });
              setTitle("");
            }}
          >
            <input
              value={title}
              onChange={(e) => setTitle(e.target.value)}
              placeholder="Add a task…"
              title="Add a task for any AI to take; they see the list every turn"
            />
          </form>
          {error && <div className="task-error">{error}</div>}
        </>
      </div>
    </div>
  );
}
