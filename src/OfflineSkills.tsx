import { invoke } from "@tauri-apps/api/core";
import { useState } from "react";
import rawSkills from "../docs/offline-skills.json?raw";

interface Skill {
  summary: string;
  command: string;
  guidance: string;
}

const skills: Record<string, Skill> = JSON.parse(rawSkills);

function Runbook({ id }: { id: string }) {
  const [content, setContent] = useState<string>();
  const [error, setError] = useState<string>();

  async function load() {
    setContent(undefined);
    setError(undefined);
    try {
      setContent(await invoke<string>("read_runbook", { id }));
    } catch (failure) {
      setError(String(failure));
    }
  }

  return (
    <details>
      <summary onClick={(event) => { if (!event.currentTarget.closest("details")?.open) void load(); }}>{id}</summary>
      {error ? <p role="alert">{error}</p> : <pre style={{ whiteSpace: "pre-wrap" }}>{content ?? "Loading…"}</pre>}
    </details>
  );
}

export default function OfflineSkills() {
  const [ids, setIds] = useState<string[]>();
  const [error, setError] = useState<string>();

  async function refresh() {
    setIds(undefined);
    setError(undefined);
    try {
      setIds(await invoke<string[]>("list_runbooks"));
    } catch (failure) {
      setError(String(failure));
    }
  }

  return (
    <details className="small">
      <summary>Offline skills</summary>
      <p className="muted small">Open a skill to see its instructions. Run the examples from a Consilium source checkout.</p>
      {Object.entries(skills).map(([id, skill]) => (
        <details key={id}>
          <summary>{skill.summary}</summary>
          <p>{skill.guidance}</p>
          <pre><code>{skill.command}</code></pre>
        </details>
      ))}
      <details>
        <summary onClick={(event) => { if (!event.currentTarget.closest("details")?.open) void refresh(); }}>Local runbooks</summary>
        <p className="muted small">Save Markdown files in ~/.consilium/skills. Open a runbook to read it.</p>
        {error && <p role="alert">{error}</p>}
        {!error && !ids && <p role="status">Loading runbooks…</p>}
        {ids?.length === 0 && <p>No local runbooks found.</p>}
        {ids?.map((id) => <Runbook key={id} id={id} />)}
        <button type="button" onClick={() => void refresh()}>Refresh runbooks</button>
      </details>
    </details>
  );
}
