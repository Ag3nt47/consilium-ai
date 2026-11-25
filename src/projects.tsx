import { invoke } from "@tauri-apps/api/core";
import { useEffect, useId, useRef, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { IN_APP } from "./api";

export interface ProjectVersion {
  name: string;
  path: string;
  local_version: string | null;
  remote_version: string | null;
  local_commit: string | null;
  remote_commit: string | null;
  ahead: number;
  behind: number;
  dirty: number;
  remote_error: string | null;
}

const label = (version: string | null, commit: string | null) =>
  version ? `v${version}` : commit ? `@${commit}` : "—";

function matches(p: ProjectVersion) {
  return p.ahead === 0 && p.behind === 0 && p.dirty === 0 &&
    p.local_version === p.remote_version && p.local_commit === p.remote_commit && !p.remote_error;
}

function syncNote(p: ProjectVersion) {
  const parts: string[] = [];
  if (p.ahead) parts.push(`${p.ahead} commit${p.ahead > 1 ? "s" : ""} not on GitHub yet`);
  if (p.behind) parts.push(`${p.behind} commit${p.behind > 1 ? "s" : ""} on GitHub not on this PC`);
  if (p.dirty) parts.push(`${p.dirty} uncommitted file${p.dirty > 1 ? "s" : ""}`);
  if (p.remote_error) parts.push(p.remote_error);
  if (!parts.length && p.local_commit !== p.remote_commit) parts.push("Local commit differs from GitHub");
  if (!parts.length && p.local_version !== p.remote_version) parts.push("Local version differs from GitHub");
  return parts.length ? parts.join(" · ") : "In sync with GitHub";
}

/** Compact source status. Repository versions stay in the details, separate from app builds. */
export function ProjectVersions({ workspace }: { workspace: string | null }) {
  const [list, setList] = useState<ProjectVersion[]>([]);
  const [open, setOpen] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const detailsId = useId();

  useEffect(() => {
    if (!open) return;
    const outside = (event: PointerEvent) => {
      if (event.target instanceof Node && !root.current?.contains(event.target)) setOpen(false);
    };
    const escape = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      setOpen(false);
      trigger.current?.focus();
    };
    document.addEventListener("pointerdown", outside);
    document.addEventListener("keydown", escape);
    return () => {
      document.removeEventListener("pointerdown", outside);
      document.removeEventListener("keydown", escape);
    };
  }, [open]);

  useEffect(() => {
    let live = true;
    setList([]);
    setOpen(false);
    const check = async () => {
      if (!workspace) return setList([]);
      if (!IN_APP) {
        if (live) {
          setList([
            {
              name: "image-service",
              path: "/projects/image-service",
              local_version: "1.4.2",
              remote_version: "1.4.2",
              local_commit: "9a2f1b0",
              remote_commit: "9a2f1b0",
              ahead: 0,
              behind: 0,
              dirty: 0,
              remote_error: null,
            },
          ]);
        }
        return;
      }
      try {
        const found = await invoke<ProjectVersion[]>("project_versions", { workspace });
        if (live) setList(found);
      } catch {
        if (live) setList([]);
      }
    };
    check();
    const t = setInterval(check, 30_000);
    window.addEventListener("focus", check);
    return () => {
      live = false;
      clearInterval(t);
      window.removeEventListener("focus", check);
    };
  }, [workspace]);

  if (!list.length) return null;
  const allMatch = list.every(matches);
  const summary = list.some((p) => p.remote_error) ? "Source unavailable" : allMatch ? "Source synced" : "Source changes";

  return (
    <div className="project-versions" ref={root}>
      <button
        type="button"
        ref={trigger}
        className={`pv-tab ${allMatch ? "pv-ok" : "pv-diff"}`}
        onClick={() => setOpen(!open)}
        title="Project source status. Click for local checkout and GitHub versions."
        aria-haspopup="dialog"
        aria-expanded={open}
        aria-controls={open ? detailsId : undefined}
      >
        <span className={allMatch ? "pv-dot ok" : "pv-dot diff"} aria-hidden="true" />
        {summary}
      </button>
      {open && (
        <div className="pv-pop" id={detailsId} role="dialog" aria-label="Project source versions">
          <div className="pv-heading">Project source</div>
          <div className="pv-note">Local checkouts compared with GitHub. App builds are shown in Settings → Multi-Computer Sync.</div>
          {list.map((p) => (
            <div key={p.path} className="pv-row" title={p.path}>
              <div className="pv-name">
                <span className={matches(p) ? "pv-dot ok" : "pv-dot diff"} />
                {p.name}
              </div>
              <div className="pv-cols">
                <span>
                  <span className="pv-k">Local</span> {label(p.local_version, p.local_commit)}
                  {p.local_version && p.local_commit ? <small> @{p.local_commit}</small> : null}
                </span>
                <span>
                  <span className="pv-k">GitHub</span> {label(p.remote_version, p.remote_commit)}
                  {p.remote_version && p.remote_commit ? <small> @{p.remote_commit}</small> : null}
                </span>
              </div>
              <div className="pv-note">{syncNote(p)}</div>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

export interface CiStatus {
  project: string;
  path: string;
  branch: string;
  commit: string | null;
  state: "pass" | "fail" | "running" | "none" | "error";
  failed: string[];
  runs: number;
  url: string | null;
  detail: string | null;
  checked_at: number;
}

const CI_WORD: Record<CiStatus["state"], string> = {
  pass: "passing",
  fail: "failing",
  running: "running",
  none: "no checks",
  error: "unknown",
};

function overall(list: CiStatus[]): CiStatus["state"] {
  for (const st of ["fail", "running", "pass", "error"] as const) if (list.some((c) => c.state === st)) return st;
  return "none";
}

function ciTitle(list: CiStatus[]) {
  return list
    .map((c) => {
      const head = `${c.project} ${c.branch}${c.commit ? ` @${c.commit}` : ""}: GitHub checks ${CI_WORD[c.state]}`;
      if (c.state === "fail" && c.failed.length) return `${head} (${c.failed.join(", ")})`;
      if (c.detail) return `${head} (${c.detail})`;
      return head;
    })
    .concat("Checked about once a minute; the AIs see this too. Click to open the run on GitHub.")
    .join("\n");
}

/** Title-bar light beside the versions tab: green when GitHub's checks pass on the newest commit,
 *  red when they fail, amber while they run. Clicking opens the run on GitHub. */
export function CiBadge({ workspace }: { workspace: string | null }) {
  const [list, setList] = useState<CiStatus[]>([]);

  useEffect(() => {
    let live = true;
    const check = async () => {
      if (!workspace) return setList([]);
      if (!IN_APP) {
        if (live)
          setList([
            {
              project: "image-service", path: "/projects/image-service", branch: "main", commit: "9a2f1b0",
              state: "pass", failed: [], runs: 2, url: null, detail: null, checked_at: Date.now(),
            },
          ]);
        return;
      }
      try {
        const found = await invoke<CiStatus[]>("ci_status", { workspace });
        if (live) setList(found);
      } catch {
        if (live) setList([]);
      }
    };
    check();
    const t = setInterval(check, 30_000);
    window.addEventListener("focus", check);
    return () => {
      live = false;
      clearInterval(t);
      window.removeEventListener("focus", check);
    };
  }, [workspace]);

  if (!list.length) return null;
  const state = overall(list);
  const target = list.find((c) => c.state === state && c.url) ?? list.find((c) => c.url);
  return (
    <button
      className={`ci-badge ci-${state}`}
      title={ciTitle(list)}
      aria-label={`GitHub checks ${CI_WORD[state]}`}
      onClick={() => target?.url && openUrl(target.url)}
    >
      <span className="ci-dot" aria-hidden="true" />
      CI {state === "pass" ? "✓" : state === "fail" ? "✗" : state === "running" ? "…" : "?"}
    </button>
  );
}
