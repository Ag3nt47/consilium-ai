import { invoke } from "@tauri-apps/api/core";
import { useEffect, useState } from "react";

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
        title="Versions of the projects being worked on: this PC and GitHub. Click for details."
      >
        {list.length === 1 ? (
          <>
            <span className="pv-k">PC</span> {label(first.local_version, first.local_commit)}
            <span className="pv-sep">·</span>
            <span className="pv-k">GitHub</span> {label(first.remote_version, first.remote_commit)}
          </>
        ) : (
          <>{list.length} projects · versions</>
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
