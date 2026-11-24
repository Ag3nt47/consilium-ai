import { useState } from "react";
import { api, type EnvironmentAudit as Report } from "./api";

export default function EnvironmentAudit({ conversation }: { conversation?: string }) {
  const [report, setReport] = useState<Report>();
  const [error, setError] = useState("");
  const [running, setRunning] = useState(false);

  async function run() {
    setRunning(true);
    setReport(undefined);
    setError("");
    try {
      setReport(await api.readEnvironmentAudit(conversation));
    } catch (failure) {
      setError(String(failure));
    } finally {
      setRunning(false);
    }
  }

  return (
    <details className="small">
      <summary>Local environment audit</summary>
      <p className="muted small">Check this computer’s tools, selected project and installed Ollama manifests without starting a model.</p>
      <button type="button" className="btn small" disabled={running} onClick={run}>
        {running ? "Reading environment…" : "Run local audit"}
      </button>
      {error && <p role="alert">{error}</p>}
      {report && (
        <div role="status">
          <p>{report.platform} · {report.architecture}</p>
          {report.project_set && <p>{report.git_summary ?? "Git status is unavailable for the selected project."}</p>}
          <details>
            <summary>Tools ({report.tools.filter(([, found]) => found).length} found)</summary>
            <ul>
              {report.tools.map(([tool, found]) => <li key={tool}><code>{tool}</code> · {found ? "File found" : "Unavailable"}</li>)}
            </ul>
          </details>
          <details>
            <summary>Project dependencies ({report.dependencies.length})</summary>
            {report.dependency_note && <p className="muted small">{report.dependency_note}</p>}
            {report.dependencies.map(({ name, requested, locked, installed, lockfile_matches }) => (
              <p key={name}>
                <code>{name}</code> · Requested {requested} · Locked {locked ?? "unavailable"} · Installed {installed ?? "unavailable"}
                {lockfile_matches === false && " · Lockfile mismatch"}
                {locked && installed && locked !== installed && " · Installed version differs"}
              </p>
            ))}
          </details>
          <details>
            <summary>Ollama manifests ({report.ollama_models.length})</summary>
            {report.ollama_note && <p className="muted small">{report.ollama_note}</p>}
            {report.ollama_models.length > 0 && (
              <ul>{report.ollama_models.map((model) => <li key={model}><code>{model}</code></li>)}</ul>
            )}
          </details>
        </div>
      )}
    </details>
  );
}
