import { useEffect, useState } from "react";
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
  const [open, setOpen] = useState(true);
  const [title, setTitle] = useState("");
  const [error, setError] = useState("");

  useEffect(() => {
    if (!conversation) return;
    api.tasksGet(conversation).then(setBoard).catch(() => undefined);
  }, [conversation, changeKey]);

  if (!conversation) return null;

  const apply = (op: TaskOp) =>
    api
      .tasksApply(conversation, op)
      .then((b) => {
        setBoard(b);
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

  return (
    <div className="task-board">
      <div className="section-label">
        <button className="link task-board-toggle" onClick={() => setOpen(!open)} title="Show or hide the shared task checklist">
          {open ? "▾" : "▸"} Tasks
        </button>
        <span title={`${done} of ${total} tasks done`}>
          {total ? `${done}/${total} · ${pct}%` : "none yet"}
        </span>
      </div>
      {total > 0 && (
        <div className="task-progress" aria-hidden>
          <div style={{ width: `${pct}%` }} />
        </div>
      )}
      {open && (
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
      )}
    </div>
  );
}
