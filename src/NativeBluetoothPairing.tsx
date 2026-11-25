import { useEffect, useRef, useState } from "react";
import { api, type BluetoothPairingStatus } from "./api";

function isActive(status: BluetoothPairingStatus | null) {
  return status?.state === "starting" || status?.state === "awaiting_code" || status?.state === "confirming";
}

export function NativeBluetoothPairing({ nodeId }: { nodeId: string }) {
  const [snapshot, setSnapshot] = useState<{ status: BluetoothPairingStatus; deadline: number } | null>(null);
  const status = snapshot?.status ?? null;
  const [entry, setEntry] = useState<{ challengeId: string; code: string } | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [pollError, setPollError] = useState("");
  const owned = useRef<BluetoothPairingStatus | null>(null);
  const mounted = useRef(false);
  const mutation = useRef(false);
  const revision = useRef(0);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      revision.current++;
      const attempt = owned.current;
      if (attempt && isActive(attempt)) void api.bluetoothPairingCancel(attempt.attempt_id).catch(() => {});
    };
  }, []);

  useEffect(() => {
    if (!snapshot || snapshot.status.state !== "awaiting_code" || snapshot.deadline === 0) return;
    const timer = window.setTimeout(() => {
      setSnapshot((current) => current === snapshot ? { ...snapshot, deadline: 0 } : current);
      setEntry(null);
    }, Math.max(0, snapshot.deadline - performance.now()));
    return () => window.clearTimeout(timer);
  }, [snapshot]);

  const attemptId = isActive(status) ? status?.attempt_id : undefined;
  useEffect(() => {
    if (!attemptId) return;
    let stopped = false;
    let timer: number;
    const poll = async () => {
      const request = revision.current;
      if (!mutation.current) {
        try {
          const sentAt = performance.now();
          const next = await api.bluetoothPairingStatus(attemptId);
          if (stopped || revision.current !== request) return;
          owned.current = next;
          setSnapshot({ status: next, deadline: sentAt + next.remaining_ms });
          setPollError("");
          if (!isActive(next)) setEntry(null);
        } catch (failure) {
          if (stopped || revision.current !== request) return;
          setEntry(null);
          setPollError(String(failure));
        } finally {
          if (!stopped) timer = window.setTimeout(poll, 400);
        }
      } else {
        timer = window.setTimeout(poll, 400);
      }
    };
    timer = window.setTimeout(poll, 400);
    return () => { stopped = true; window.clearTimeout(timer); };
  }, [attemptId]);

  async function change(action: () => Promise<BluetoothPairingStatus>) {
    if (mutation.current) return;
    mutation.current = true;
    const request = ++revision.current;
    setBusy(true);
    setError("");
    setPollError("");
    setEntry(null);
    try {
      const sentAt = performance.now();
      const next = await action();
      if (!mounted.current || revision.current !== request) {
        if (isActive(next)) void api.bluetoothPairingCancel(next.attempt_id).catch(() => {});
        return;
      }
      owned.current = next;
      setSnapshot({ status: next, deadline: sentAt + next.remaining_ms });
    } catch (failure) {
      if (mounted.current) setError(String(failure));
    } finally {
      mutation.current = false;
      if (mounted.current) setBusy(false);
    }
  }

  const codeAvailable = !!snapshot && snapshot.deadline > performance.now();
  const challenge = status?.state === "awaiting_code" && codeAvailable && !busy && !pollError ? status.challenge_id : null;
  const enteredCode = entry?.challengeId === challenge ? entry.code : "";
  return (
    <div className="link-card small">
      {!attemptId && (
        <button className="btn small ghost" type="button" disabled={busy}
          onClick={() => void change(() => api.bluetoothPairingBegin(nodeId))}>
          {busy ? "Starting Bluetooth pairing…" : "Show Bluetooth code"}
        </button>
      )}
      {status && <p role="status">{status.state === "awaiting_code" && !codeAvailable ? "The Bluetooth code expired." : status.detail}</p>}
      {status && challenge && status.code && (
        <form onSubmit={(event) => {
          event.preventDefault();
          if (!busy && (snapshot?.deadline ?? 0) > performance.now() && /^[0-9]{6}$/.test(enteredCode)) {
            void change(() => api.bluetoothPairingConfirm(status.attempt_id, challenge, enteredCode));
          }
        }}>
          <p>Windows Bluetooth code <strong><code>{status.code}</code></strong></p>
          <p className="muted">Compare both numbers. Type the number shown on your Mac, then confirm on both computers.</p>
          <div className="row gap-sm align-center">
            <label className="row gap-sm">
              Enter Mac code
              <input type="text" inputMode="numeric" autoComplete="off" maxLength={6} disabled={busy}
                value={enteredCode} onChange={(event) => setEntry({ challengeId: challenge, code: event.target.value.replace(/[^0-9]/g, "") })} />
            </label>
            <button className="btn small" type="submit" disabled={busy || !/^[0-9]{6}$/.test(enteredCode)}>Confirm match</button>
          </div>
        </form>
      )}
      {attemptId && (
        <button className="btn small ghost" type="button" disabled={busy}
          onClick={() => void change(() => api.bluetoothPairingCancel(attemptId))}>Cancel Bluetooth pairing</button>
      )}
      {(error || pollError) && <p role="alert">{error || pollError}</p>}
    </div>
  );
}
