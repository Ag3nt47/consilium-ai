import { useEffect, useRef, useState } from "react";
import { api, type BluetoothStatus } from "./api";

export function NativeBluetooth({ directEnabled }: { directEnabled: boolean }) {
  const [status, setStatus] = useState<BluetoothStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const request = useRef(0);
  const changing = useRef(false);
  const mounted = useRef(false);

  useEffect(() => {
    mounted.current = true;
    const refresh = async () => {
      if (changing.current) return;
      const current = ++request.current;
      try {
        const next = await api.bluetoothStatus();
        if (mounted.current && request.current === current) {
          setStatus(next ?? null);
          setError("");
        }
      } catch (failure) {
        if (mounted.current && request.current === current) setError(String(failure));
      }
    };
    void refresh();
    const timer = window.setInterval(refresh, 5000);
    return () => { mounted.current = false; request.current++; window.clearInterval(timer); };
  }, []);

  const change = async (enabled: boolean) => {
    if (changing.current) return;
    changing.current = true;
    const current = ++request.current;
    setBusy(true);
    setError("");
    try {
      const next = await api.bluetoothSetEnabled(enabled);
      if (mounted.current && request.current === current) setStatus(next);
    } catch (failure) {
      if (mounted.current && request.current === current) setError(String(failure));
    } finally {
      changing.current = false;
      if (mounted.current) setBusy(false);
    }
  };

  if (!status?.supported) return error ? <p className="muted small" role="alert">Could not read Bluetooth status: {error}</p> : null;
  return (
    <div className="link-card small">
      <label className="row gap-sm">
        <input type="checkbox" checked={status.enabled}
          disabled={busy || (!directEnabled && !status.enabled)}
          onChange={(event) => void change(event.target.checked)} />
        Native Bluetooth · Windows and Mac
      </label>
      <p className="muted">Pair your computers in their Bluetooth settings, then enable this on both. Keep the direct computer link enabled.</p>
      <p role="status">{busy ? "Saving Bluetooth setting…" : status.detail}</p>
      {!directEnabled && <p className="muted">Turn on the direct computer link above to use Bluetooth.</p>}
      {error && <p role="alert">{error}</p>}
      {status.enabled && <button type="button" className="btn small ghost" disabled={busy || !directEnabled}
        onClick={() => void change(true)}>Restart Bluetooth</button>}
      {status.endpoint && <details><summary>Bluetooth address</summary><code>{status.endpoint}</code></details>}
    </div>
  );
}
