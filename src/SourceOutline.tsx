import { useEffect, useRef, useState } from "react";
import type { OutlineReply, OutlineRequest } from "./source-outline.worker";

export default function SourceOutline({ copyText }: { copyText: (text: string) => Promise<void> }) {
  const worker = useRef<Worker | null>(null);
  const [file, setFile] = useState<File>();
  const [scope, setScope] = useState("");
  const [filter, setFilter] = useState("");
  const [running, setRunning] = useState(false);
  const [outline, setOutline] = useState("");
  const [error, setError] = useState("");
  const [copyStatus, setCopyStatus] = useState("");

  useEffect(() => () => worker.current?.terminate(), []);

  function clear() {
    setOutline("");
    setError("");
    setCopyStatus("");
  }

  function build() {
    if (!file || worker.current) return;
    clear();
    setRunning(true);
    let parser: Worker;
    try {
      parser = new Worker(new URL("./source-outline.worker.ts", import.meta.url), { type: "module" });
    } catch {
      setError("Could not start the local source parser.");
      setRunning(false);
      return;
    }
    worker.current = parser;
    const finish = () => {
      parser.terminate();
      worker.current = null;
      setRunning(false);
    };
    parser.onmessage = ({ data }: MessageEvent<OutlineReply>) => {
      if ("error" in data) {
        setError(data.error);
      } else {
        const summary = [`${JSON.stringify(file.name)} — ${data.rows.length} rows`, ...data.rows];
        if (data.omitted) summary.push(`${data.omitted} additional rows omitted.`);
        setOutline(summary.join("\n"));
      }
      finish();
    };
    const failed = () => {
      setError("The local source parser stopped unexpectedly.");
      finish();
    };
    parser.onerror = failed;
    parser.onmessageerror = failed;
    const request: OutlineRequest = { file, functionName: scope.trim(), filter: filter.trim() };
    try {
      parser.postMessage(request);
    } catch {
      failed();
    }
  }

  async function copy() {
    try {
      await copyText(outline);
      setCopyStatus("Outline copied.");
    } catch {
      setCopyStatus("Could not copy the outline.");
    }
  }

  return (
    <details className="small">
      <summary>JavaScript and TypeScript outline</summary>
      <p className="muted small">List symbols and line ranges from a file up to 1 MiB. Copy the outline into a task or chat.</p>
      <input type="file" aria-label="Source file" accept=".js,.jsx,.ts,.tsx,.mjs,.cjs,.mts,.cts"
        disabled={running} onChange={(event) => { setFile(event.target.files?.[0]); clear(); }} />
      <div className="row wrap gap-sm">
        <input aria-label="Top-level function" placeholder="Top-level function (optional)" value={scope}
          disabled={running} onChange={(event) => { setScope(event.target.value); clear(); }} />
        <input aria-label="Symbol filter" placeholder="Symbol filter (optional)" value={filter}
          disabled={running} onChange={(event) => { setFilter(event.target.value); clear(); }} />
      </div>
      <button type="button" className="btn small" disabled={!file || running} onClick={build}>
        {running ? "Building outline…" : "Build outline"}
      </button>
      {error && <p role="alert">{error}</p>}
      {outline && (
        <>
          <pre style={{ whiteSpace: "pre-wrap", maxHeight: 240, overflow: "auto" }}>{outline}</pre>
          <button type="button" className="btn small ghost" onClick={copy}>Copy outline</button>
        </>
      )}
      {copyStatus && <p role="status">{copyStatus}</p>}
    </details>
  );
}
