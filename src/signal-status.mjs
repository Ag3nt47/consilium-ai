/** Compose direct status with a short, independently expiring peer readiness report. */
export function signalStatus(direct, relay, expiresAt, now) {
  const local = direct?.find((status) => status.id === "signal");
  if (local?.state === "ok") return { ...local, mode: "direct" };
  const fresh = Number.isFinite(expiresAt) && Number.isFinite(now) && now < expiresAt;
  if (fresh && relay?.state === "available" && relay.peer) {
    return {
      id: "signal", name: "Signal", state: "ok", mode: "relay",
      detail: `Via ${relay.peer.name} over encrypted PeerSync. That computer reports its Signal connection active.`,
    };
  }
  if (fresh && relay?.state === "multiple") {
    return {
      id: "signal", name: "Signal", state: "down", mode: "multiple",
      detail: "Multiple linked computers report Signal active. A single relay cannot be identified; check the linked computers' Signal setup.",
    };
  }
  return { ...(local ?? { id: "signal", name: "Signal", state: "off", detail: "Checking Signal…" }), mode: "unavailable" };
}

/** Count IPC transit against the backend's remaining lease, never extend it on arrival. */
export function relayExpiry(report, requestStarted) {
  const remaining = report?.valid_for_ms;
  return Number.isFinite(requestStarted) && Number.isFinite(remaining) && remaining > 0 && remaining <= 4000
    ? requestStarted + remaining : 0;
}
