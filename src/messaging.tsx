import { useEffect, useState } from "react";
import { api, type ConnectorStatus } from "./api";

/** Top-left indicator: a green check when a messaging app (Signal) answered the last check,
 *  a red cross when it's set up but not answering, grey when nothing is set up. */
export function MessagingIndicator() {
  const [list, setList] = useState<ConnectorStatus[] | null>(null);
  const [checking, setChecking] = useState(false);

  const check = async () => {
    setChecking(true);
    try {
      setList(await api.messagingStatus());
    } catch {
      setList(null);
    } finally {
      setChecking(false);
    }
  };

  useEffect(() => {
    check();
    const t = setInterval(check, 30_000);
    window.addEventListener("focus", check);
    return () => {
      clearInterval(t);
      window.removeEventListener("focus", check);
    };
  }, []);

  if (!list || list.length === 0) return null;
  const on = list.filter((c) => c.state !== "off");
  const allOk = on.length > 0 && on.every((c) => c.state === "ok");
  const state = on.length === 0 ? "off" : allOk ? "ok" : "down";
  const mark = state === "ok" ? "✓" : state === "down" ? "✗" : "○";
  const label = on.length === 0 ? "Messaging off" : on.map((c) => c.name).join(", ");
  const tip =
    list.map((c) => `${c.state === "ok" ? "✓" : c.state === "down" ? "✗" : "○"} ${c.name}: ${c.detail}`).join("\n") +
    "\nChecked every 30 seconds. Click to check now.";

  return (
    <button
      className={`msg-indicator ${state}${checking ? " checking" : ""}`}
      title={tip}
      aria-label={`Messaging connection: ${state === "ok" ? "verified" : state === "down" ? "not working" : "not set up"}`}
      onClick={check}
    >
      <span className="msg-mark">{mark}</span>
      <span className="msg-label">{label}</span>
    </button>
  );
}
