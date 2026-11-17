import { useEffect, useState } from "react";
import { api, type SecurityScanStatus } from "./api";

function when(ms: number): string {
  return ms ? new Date(ms).toLocaleString() : "Never";
}

/** Settings for the optional, approval-installed Zero Breach adapter. */
export function SecurityScanSettings() {
  const [status, setStatus] = useState<SecurityScanStatus | null>(null);
  const [note, setNote] = useState("");
  const [busy, setBusy] = useState(false);

  const reload = () => api.securityScanStatus().then(setStatus).catch((e) => setNote(String(e)));
  useEffect(() => {
    reload();
    const timer = window.setInterval(reload, 15_000);
    return () => window.clearInterval(timer);
  }, []);

  const save = async (
    enabled: boolean,
    intervalHours = status?.interval_hours ?? 168,
    autoRemove = status?.auto_remove_malicious_extensions ?? true,
  ) => {
    setBusy(true);
    try {
      setStatus(
        await api.securityScanSave({
          enabled,
          interval_hours: intervalHours,
          auto_remove_malicious_extensions: autoRemove,
        }),
      );
      setNote("Saved on this computer.");
    } catch (e) {
      setNote(String(e));
    } finally {
      setBusy(false);
    }
  };

  const run = async () => {
    setBusy(true);
    try {
      setStatus(await api.securityScanRun());
      setNote("Scan requested. It will start after five quiet minutes.");
    } catch (e) {
      setNote(String(e));
    } finally {
      setBusy(false);
    }
  };

  if (!status) return <p className="muted small">Checking the local security scanner…</p>;
  return (
    <div className="security-scan-settings">
      <p className="muted small">
        Optional Zero Breach scans are local and read-only. Consilium runs a quick scan only after five idle minutes,
        stops it when AI work or high load returns, and keeps private reports on this computer. A clean result is not a
        guarantee that a device is safe.
      </p>
      <label className="toggle">
        <input
          type="checkbox"
          checked={status.enabled}
          disabled={busy}
          onChange={(e) => save(e.target.checked)}
        />
        Run low-priority spyware checks when idle
      </label>
      <label className="toggle">
        <input
          type="checkbox"
          checked={status.auto_remove_malicious_extensions}
          disabled={busy || !status.enabled}
          onChange={(e) => save(status.enabled, status.interval_hours, e.target.checked)}
        />
        Automatically remove malicious browser extensions detected by Zero Breach
      </label>
      <label className="msg-field">
        <span>Repeat</span>
        <select
          value={status.interval_hours}
          disabled={busy || !status.enabled}
          onChange={(e) => save(status.enabled, Number(e.target.value))}
        >
          <option value={24}>Daily</option>
          <option value={168}>Weekly</option>
          <option value={336}>Every two weeks</option>
          <option value={720}>Monthly</option>
        </select>
      </label>
      <div className="small">
        <b>
          {!status.installed
            ? "Scanner not installed"
            : !status.verified
              ? "Scanner verification failed"
              : status.running
                ? "Scanning"
                : "Scanner ready"}
        </b>
        {status.last_summary && <span> · {status.last_summary}</span>}
      </div>
      <div className="muted small">Last completed: {when(status.last_finished_ms)}</div>
      {status.paused_reason && <div className="muted small">{status.paused_reason}</div>}
      {!status.installed && <div className="muted small">Expected approved install: {status.tool_path}</div>}
      {status.last_report && <div className="muted small">Private report: {status.last_report}</div>}
      <div className="row">
        <button className="btn ghost small-btn" disabled={busy || status.running || !status.verified} onClick={run}>
          Run when idle
        </button>
        {note && <span className="muted small">{note}</span>}
      </div>
    </div>
  );
}
