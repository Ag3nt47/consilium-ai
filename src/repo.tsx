// GitHub repo inspector: paste a link, Consilium makes a sandboxed shallow clone (running none
// of the repo's code) and shows what's inside. "Ask the council" drops a summary into the prompt.
import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { IN_APP } from "./api";

export interface RepoReport {
  slug: string;
  path: string;
  commit: string | null;
  files: number;
  bytes: number;
  truncated: boolean;
  languages: { language: string; files: number; bytes: number }[];
  license: string | null;
  build_files: string[];
  risks: string[];
}

const DEMO: RepoReport = {
  slug: "octocat/Hello-World",
  path: "~/Library/Application Support/io.github.ag3nt47.consilium/repos/octocat__Hello-World",
  commit: "7fd1a60",
  files: 42,
  bytes: 183_000,
  truncated: false,
  languages: [
    { language: "TypeScript", files: 20, bytes: 120_000 },
    { language: "CSS", files: 4, bytes: 30_000 },
  ],
  license: "MIT",
  build_files: ["package.json"],
  risks: ['package.json has an npm "postinstall" script that runs on install'],
};

function size(b: number) {
  if (b >= 1 << 20) return `${(b / (1 << 20)).toFixed(1)} MB`;
  if (b >= 1024) return `${Math.round(b / 1024)} KB`;
  return `${b} B`;
}

export function summarize(r: RepoReport) {
  const langs = r.languages.slice(0, 5).map((l) => `${l.language} (${l.files})`).join(", ") || "none detected";
  return [
    `Inspect this repo, cloned read-only at ${r.path} (commit ${r.commit ?? "?"}): https://github.com/${r.slug}`,
    `${r.files}${r.truncated ? "+" : ""} files, ${size(r.bytes)}. Languages: ${langs}. Licence: ${r.license ?? "none found"}.`,
    `Build files: ${r.build_files.join(", ") || "none"}.`,
    r.risks.length ? `Flagged: ${r.risks.join("; ")}.` : "Nothing flagged.",
    "Don't run its scripts or installers without asking me.",
  ].join("\n");
}

export function RepoDrawer({ onClose, onAsk }: { onClose: () => void; onAsk: (text: string) => void }) {
  const [url, setUrl] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [report, setReport] = useState<RepoReport | null>(IN_APP ? null : DEMO);

  async function run() {
    if (!url.trim() || busy) return;
    setBusy(true);
    setError(null);
    try {
      setReport(IN_APP ? await invoke<RepoReport>("ingest_repo", { url }) : DEMO);
    } catch (e) {
      setError(String(e));
      setReport(null);
    } finally {
      setBusy(false);
    }
  }

  return (
    <aside className="audit-drawer repo-drawer" aria-label="GitHub repo inspector">
      <div className="audit-head">
        <strong>Inspect a GitHub repo</strong>
        <span className="spacer" />
        <button className="btn small ghost" onClick={onClose} title="Close the repo inspector">
          Close
        </button>
      </div>
      <p className="muted small">
        Makes a shallow copy in Consilium's private folder. None of the repo's scripts, hooks or installers run.
      </p>
      <form
        className="repo-form"
        onSubmit={(e) => {
          e.preventDefault();
          void run();
        }}
      >
        <input
          value={url}
          onChange={(e) => setUrl(e.target.value)}
          placeholder="https://github.com/owner/repo"
          spellCheck={false}
          title="A public github.com repository link, or owner/repo"
        />
        <button className="btn small" disabled={busy || !url.trim()} title="Clone and inspect it">
          {busy ? "Cloning…" : "Inspect"}
        </button>
      </form>
      {error && <p className="repo-error">{error}</p>}
      {report && (
        <div className="repo-report">
          <h4>
            {report.slug} <span className="muted small">{report.commit}</span>
          </h4>
          <p className="small">
            {report.files}
            {report.truncated ? "+" : ""} files · {size(report.bytes)} · Licence: {report.license ?? "none found"}
          </p>
          {report.languages.length > 0 && (
            <ul className="repo-langs">
              {report.languages.slice(0, 8).map((l) => (
                <li key={l.language}>
                  {l.language} <span className="muted small">{l.files} files · {size(l.bytes)}</span>
                </li>
              ))}
            </ul>
          )}
          <p className="small">Build files: {report.build_files.join(", ") || "none"}</p>
          {report.risks.length > 0 ? (
            <ul className="repo-risks">
              {report.risks.map((r) => (
                <li key={r}>⚠ {r}</li>
              ))}
            </ul>
          ) : (
            <p className="small">Nothing flagged.</p>
          )}
          <button className="btn small" onClick={() => onAsk(summarize(report))} title="Put this report in the prompt box for the council">
            Ask the council about it
          </button>
        </div>
      )}
    </aside>
  );
}
