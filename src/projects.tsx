import { invoke } from "@tauri-apps/api/core";
import { useEffect, useState } from "react";
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
  return p.ahead === 0 && p.behind === 0 && p.local_version === p.remote_version && !p.remote_error;
}

function syncNote(p: ProjectVersion) {
  const parts: string[] = [];
  if (p.ahead) parts.push(`${p.ahead} commit${p.ahead > 1 ? "s" : ""} not on GitHub yet`);
  if (p.behind) parts.push(`${p.behind} commit${p.behind > 1 ? "s" : ""} on GitHub not on this PC`);
  if (p.dirty) parts.push(`${p.dirty} uncommitted file${p.dirty > 1 ? "s" : ""}`);
  if (p.remote_error) parts.push(p.remote_error);
  return parts.length ? parts.join(" · ") : "In sync with GitHub";
}

/** Title-bar tab, next to the project name: each active project's version on this PC and on
 *  GitHub. It hides itself when no project in the folder has been worked on recently. */
export function ProjectVersions({ workspace }: { workspace: string | null }) {
  const [list, setList] = useState<ProjectVersion[]>([]);
  const [open, setOpen] = useState(false);

  useEffect(() => {
    let live = true;
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
  const first = list[0];
  const allMatch = list.every(matches);

  return (
    <div className="project-versions">
      <button
        className={`pv-tab ${allMatch ? "pv-ok" : "pv-diff"}`}
        onClick={() => setOpen(!open)}
        title={list.length > 1 ? `${first.name}, plus ${list.length - 1} more project${list.length > 2 ? "s" : ""} being worked on. Click for details.` : `${first.name}: version on this PC and on GitHub. Click for details.`}
      >
        <span className="pv-k">PC</span> {label(first.local_version, first.local_commit)}
        <span className="pv-sep">·</span>
        <span className="pv-k">GitHub</span> {label(first.remote_version, first.remote_commit)}
        {list.length > 1 && (
          <>
            <span className="pv-sep">·</span>+{list.length - 1}
          </>
        )}
      </button>
      {open && (
        <div className="pv-pop" onMouseLeave={() => setOpen(false)}>
          {list.map((p) => (
            <div key={p.path} className="pv-row" title={p.path}>
              <div className="pv-name">
                <span className={matches(p) ? "pv-dot ok" : "pv-dot diff"} />
                {p.name}
              </div>
              <div className="pv-cols">
                <span>
                  <span className="pv-k">PC</span> {label(p.local_version, p.local_commit)}
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
