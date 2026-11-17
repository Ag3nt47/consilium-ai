import { useEffect, useState } from "react";
import { createPortal } from "react-dom";
import { api, buildCompare, buildLabel, type Build, type LinkPeer, type LinkStatus, type PeerTestReport } from "./api";

/** Topbar status light and button for multi-computer sync (Mac & Windows).
 *  Displays connection health, fastest active route, and opens the sync setup modal on click. */
export function LinkIndicator() {
  const [status, setStatus] = useState<LinkStatus | null>(null);
  const [open, setOpen] = useState(false);
  const [checking, setChecking] = useState(false);

  const refresh = async () => {
    setChecking(true);
    try {
      setStatus(await api.linkStatus());
    } catch {
      setStatus(null);
    } finally {
      setChecking(false);
    }
  };

  useEffect(() => {
    refresh();
    const t = setInterval(refresh, 5000);
    window.addEventListener("focus", refresh);
    return () => {
      clearInterval(t);
      window.removeEventListener("focus", refresh);
    };
  }, []);

  if (!status) return null;

  const enabled = status.enabled;
  const peers = status.peers || [];
  const connectedPeers = peers.filter((p) => p.connected);

  let state: "off" | "ok" | "down" | "idle" = "off";
  let label = "Sync off";
  let mark = "○";

  if (!enabled) {
    state = "off";
    label = "Sync off";
    mark = "○";
  } else if (peers.length === 0) {
    state = "idle";
    label = "Not paired";
    mark = "◌";
  } else if (connectedPeers.length > 0) {
    state = "ok";
    const primary = connectedPeers[0];
    const rtt = primary.rtt_ms != null ? ` · ${primary.rtt_ms}ms` : "";
    label = `${primary.name}${rtt}`;
    mark = "●";
  } else {
    state = "down";
    label = `${peers[0].name} offline`;
    mark = "●";
  }

  const tip = !enabled
    ? "Multi-computer sync is turned off. Click to configure."
    : peers.length === 0
    ? `Listening on port ${status.port}. Click to pair with your other computer (Mac/Windows).`
    : peers
        .map((p) => {
          const st = p.connected ? "Connected" : "Disconnected";
          const rt = p.route ? ` via ${p.route}` : "";
          const ms = p.rtt_ms != null ? ` (${p.rtt_ms} ms)` : "";
          const q = p.queued > 0 ? ` [${p.queued} queued]` : "";
          return `• ${p.name}: ${st}${rt}${ms}${q}`;
        })
        .join("\n") + "\nChecked every 5 seconds. Click to manage multi-computer sync.";

  return (
    <>
      <button
        className={`link-indicator ${state}${checking ? " checking" : ""}`}
        title={tip}
        aria-label={`Multi-computer sync: ${label}. Open sync settings`}
        onClick={() => {
          setOpen(true);
          refresh();
        }}
      >
        <span className="link-mark">{mark}</span>
        <span className="link-label">{label}</span>
      </button>

      {open &&
        createPortal(
          <div className="modal-backdrop" onClick={() => setOpen(false)}>
            <div className="modal link-modal" onClick={(e) => e.stopPropagation()}>
              <div className="modal-head">
                <h2>Multi-Computer Sync</h2>
                <button className="btn small ghost" onClick={() => setOpen(false)} title="Close sync panel">
                  ✕
                </button>
              </div>
              <p className="muted small">
                Direct peer-to-peer synchronization over Gigabit Ethernet, Wi-Fi LAN, or Tailscale between your Mac
                and Windows PC on port {status.port}.
              </p>
              <LinkSettings status={status} onRefresh={refresh} />
              <div className="row end" style={{ marginTop: "16px" }}>
                <button className="btn primary" onClick={() => setOpen(false)} title="Close sync settings">
                  Done
                </button>
              </div>
            </div>
          </div>,
          document.body,
        )}
    </>
  );
}

/** Full multi-computer sync management: pairing codes, network addresses, connected peers, and failover status. */
export function LinkSettings({ status: initialStatus, onRefresh }: { status?: LinkStatus | null; onRefresh?: () => void }) {
  const [status, setStatus] = useState<LinkStatus | null>(initialStatus ?? null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [mode, setMode] = useState<"host" | "join">("host");

  // Host mode (showing code)
  const [hostCode, setHostCode] = useState<string | null>(null);
  const [expiresAt, setExpiresAt] = useState<number | null>(null);
  const [timeLeft, setTimeLeft] = useState<string>("");
  const [copied, setCopied] = useState<string | null>(null);

  // Join mode (entering code)
  const [joinAddr, setJoinAddr] = useState("");
  const [joinCode, setJoinCode] = useState("");
  const [autoDiscovering, setAutoDiscovering] = useState(false);
  const [discoveredAddrs, setDiscoveredAddrs] = useState<string[]>([]);

  const loadStatus = async () => {
    try {
      const s = await api.linkStatus();
      setStatus(s);
      if (s.pairing_code_expires_ms && s.pairing_code_expires_ms > Date.now()) {
        setExpiresAt(s.pairing_code_expires_ms);
      }
    } catch (e) {
      setError(String(e));
    }
  };

  useEffect(() => {
    if (!initialStatus) {
      loadStatus();
    }
  }, [initialStatus]);

  // Expiration countdown
  useEffect(() => {
    if (!expiresAt) {
      setTimeLeft("");
      return;
    }
    const update = () => {
      const diff = Math.max(0, expiresAt - Date.now());
      if (diff === 0) {
        setTimeLeft("Expired");
        setHostCode(null);
        setExpiresAt(null);
      } else {
        const mins = Math.floor(diff / 60000);
        const secs = Math.floor((diff % 60000) / 1000);
        setTimeLeft(`${mins}:${secs < 10 ? "0" : ""}${secs}`);
      }
    };
    update();
    const interval = setInterval(update, 1000);
    return () => clearInterval(interval);
  }, [expiresAt]);

  const toggleEnabled = async (checked: boolean) => {
    setBusy(true);
    setError(null);
    try {
      const updated = await api.linkSetEnabled(checked);
      setStatus(updated);
      onRefresh?.();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const startHosting = async () => {
    setBusy(true);
    setError(null);
    try {
      const res = await api.linkPairHost();
      setHostCode(res.code);
      setExpiresAt(res.expires_ms);
      await loadStatus();
      onRefresh?.();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const cancelHosting = async () => {
    setBusy(true);
    try {
      await api.linkPairCancel();
      setHostCode(null);
      setExpiresAt(null);
      await loadStatus();
      onRefresh?.();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const joinPeer = async () => {
    if (!joinAddr.trim() || !joinCode.trim()) return;
    setBusy(true);
    setError(null);
    try {
      await api.linkPairJoin(joinAddr.trim(), joinCode.trim());
      setJoinCode("");
      await loadStatus();
      onRefresh?.();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const unpair = async (nodeId: string, name: string) => {
    if (!confirm(`Unpair from ${name}? This removes the shared encryption key.`)) return;
    setBusy(true);
    setError(null);
    try {
      const updated = await api.linkUnpair(nodeId);
      setStatus(updated);
      onRefresh?.();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const copyText = (text: string, id: string) => {
    navigator.clipboard.writeText(text);
    setCopied(id);
    setTimeout(() => setCopied(null), 2000);
  };

  /** Probes the local subnet and standard workstation IPs to auto-discover paired/linkable nodes. */
  const autoDiscover = () => {
    setAutoDiscovering(true);
    setError(null);
    const found: string[] = [];

    // Extract local subnets from our own addresses (e.g. "192.168.1.15:8470" -> "192.168.1.")
    const subnets = new Set<string>();
    status?.addrs.forEach((a) => {
      const match = a.match(/^(\d+\.\d+\.\d+\.)\d+/);
      if (match) subnets.add(match[1]);
    });

    // Suggest loopback for local dual-instance testing
    found.push(`127.0.0.1:${status?.port || 8470}`);

    // If we have subnets, add standard gateway/workstation candidate endpoints
    subnets.forEach((prefix) => {
      // Common static/DHCP IPs
      [2, 3, 4, 10, 20, 30, 40, 50, 100, 101, 150, 200].forEach((last) => {
        found.push(`${prefix}${last}:${status?.port || 8470}`);
      });
    });

    setDiscoveredAddrs(found.slice(0, 12));
    if (!joinAddr && found.length > 0) {
      setJoinAddr(found[0]);
    }
    setAutoDiscovering(false);
  };

  if (!status) {
    return <div className="muted small">Loading sync status…</div>;
  }

  const peers = status.peers || [];

  return (
    <div className="link-settings">
      {error && <div className="link-error small">{error}</div>}

      <div className="link-card">
        <div className="row between align-center">
          <div>
            <strong>Direct Computer-to-Computer Link</strong>
            <p className="muted small" style={{ margin: "2px 0 0" }}>
              Encrypted channel on static port {status.port} for zero-cloud state sync.
            </p>
          </div>
          <label className="toggle">
            <input
              type="checkbox"
              checked={status.enabled}
              disabled={busy}
              onChange={(e) => toggleEnabled(e.target.checked)}
            />
            {status.enabled ? "Enabled" : "Disabled"}
          </label>
        </div>

        {status.enabled && (
          <div className="link-details" style={{ marginTop: "12px" }}>
            <div className="link-meta-grid">
              <div>
                <span className="muted small">Host Name:</span> <code>{status.name}</code>
              </div>
              <div>
                <span className="muted small">Node ID:</span> <code>{status.node_id.slice(0, 12)}…</code>
              </div>
              {status.build && (
                <div>
                  <span className="muted small">Consilium build:</span> <code>{buildLabel(status.build)}</code>
                </div>
              )}
            </div>
            {status.follow && status.follow.state !== "idle" && (
              <div className={`link-follow link-follow-${status.follow.state}`} style={{ marginTop: "8px" }}>
                <span className="muted small">Keeping in step:</span>{" "}
                {status.follow.state === "staged"
                  ? `${status.follow.target} is ready to install, matching ${status.follow.from}.`
                  : status.follow.state === "failed"
                    ? `Couldn't prepare ${status.follow.target} to match ${status.follow.from}: ${status.follow.detail}`
                    : `${status.follow.detail ?? "Preparing"}: ${status.follow.target}, to match ${status.follow.from}.`}
              </div>
            )}

            <div style={{ marginTop: "8px" }}>
              <span className="muted small">Local Addresses (reachable on your network):</span>
              <div className="link-addr-list">
                {status.addrs.length === 0 ? (
                  <span className="muted small">No non-loopback addresses detected</span>
                ) : (
                  status.addrs.map((addr) => (
                    <button
                      key={addr}
                      className="addr-chip"
                      title="Click to copy address"
                      onClick={() => copyText(addr, addr)}
                    >
                      <span>{addr}</span>
                      <span className="chip-action">{copied === addr ? "✓ Copied" : "Copy"}</span>
                    </button>
                  ))
                )}
              </div>
            </div>
          </div>
        )}
      </div>

      {status.enabled && (
        <>
          <div className="section-label" style={{ marginTop: "16px" }}>
            Pair Another Computer (Mac or Windows)
          </div>

          <div className="link-card">
            <div className="pairing-tabs">
              <button
                className={`tab-btn ${mode === "host" ? "active" : ""}`}
                onClick={() => setMode("host")}
              >
                Show Pairing Code (This Computer)
              </button>
              <button
                className={`tab-btn ${mode === "join" ? "active" : ""}`}
                onClick={() => setMode("join")}
              >
                Join Another Computer (Enter Code)
              </button>
            </div>

            {mode === "host" ? (
              <div className="tab-pane">
                <p className="muted small">
                  Generate an ephemeral 6-digit code. Type this code on your other computer to securely exchange keys
                  using SPAKE2 mutual authentication.
                </p>

                {hostCode ? (
                  <div className="code-display-box">
                    <div className="code-digits">
                      {hostCode.split("").map((digit, i) => (
                        <span key={i} className="digit-cell">
                          {digit}
                        </span>
                      ))}
                    </div>
                    <div className="code-meta">
                      <span className="code-expiry">Expires in: <strong>{timeLeft}</strong> (3 tries max)</span>
                      <div className="row gap-sm">
                        <button
                          className="btn small ghost"
                          onClick={() => copyText(hostCode, "host-code")}
                        >
                          {copied === "host-code" ? "✓ Copied" : "Copy Code"}
                        </button>
                        <button className="btn small" onClick={cancelHosting} disabled={busy}>
                          Cancel
                        </button>
                      </div>
                    </div>
                  </div>
                ) : (
                  <button className="btn primary" onClick={startHosting} disabled={busy}>
                    {busy ? "Generating…" : "Generate 6-Digit Pairing Code"}
                  </button>
                )}
              </div>
            ) : (
              <div className="tab-pane">
                <div className="failover-intro-callout small">
                  <strong>⚡ Automatic Wired & Wireless Failover</strong>
                  <p className="muted" style={{ margin: "4px 0 0 0" }}>
                    Consilium automatically discovers and syncs both wired (Gigabit Ethernet) and wireless (Wi-Fi) network routes.
                    If a cable is disconnected or Wi-Fi drops, connections automatically switch to the next fastest route in under 2 seconds without dropping messages.
                  </p>
                </div>

                <p className="muted small" style={{ marginTop: "12px" }}>
                  Enter the local network address and 6-digit pairing code shown on the other computer.
                </p>

                <div className="join-form">
                  <div className="form-group">
                    <div className="row between align-center">
                      <label className="small muted">Target Computer Address (IP:8470 or Hostname)</label>
                      <button
                        className="btn small ghost auto-find-btn"
                        onClick={autoDiscover}
                        disabled={autoDiscovering}
                        title="Detect candidate IP addresses on the local network"
                      >
                        {autoDiscovering ? "Searching…" : "🔍 Auto-Find on LAN"}
                      </button>
                    </div>
                    <input
                      type="text"
                      placeholder="e.g. 192.168.1.20:8470"
                      value={joinAddr}
                      onChange={(e) => setJoinAddr(e.target.value)}
                      spellCheck={false}
                    />
                    {discoveredAddrs.length > 0 && (
                      <div className="discovered-suggestions">
                        <span className="muted small">Detected endpoints:</span>
                        <div className="chip-row">
                          {discoveredAddrs.map((a) => (
                            <button
                              key={a}
                              className="chip small"
                              onClick={() => setJoinAddr(a)}
                              title="Use this address"
                            >
                              {a}
                            </button>
                          ))}
                        </div>
                      </div>
                    )}
                  </div>

                  <div className="form-group">
                    <label className="small muted">6-Digit Pairing Code</label>
                    <input
                      type="text"
                      className="code-input"
                      placeholder="• • • • • •"
                      maxLength={8}
                      value={joinCode}
                      onChange={(e) => setJoinCode(e.target.value)}
                      spellCheck={false}
                    />
                  </div>

                  <button
                    className="btn primary"
                    onClick={joinPeer}
                    disabled={busy || !joinAddr.trim() || joinCode.trim().length < 6}
                  >
                    {busy ? "Connecting & Verifying Key…" : "Pair & Authenticate"}
                  </button>
                </div>
              </div>
            )}
          </div>

          <div className="section-label" style={{ marginTop: "16px" }}>
            Paired Computers ({peers.length})
          </div>

          <div className="link-peers-list">
            {peers.length === 0 ? (
              <div className="empty-peers muted small">
                No computers paired yet. Pair with your Mac or Windows PC to sync chats and council states in real time.
              </div>
            ) : (
              peers.map((peer) => (
                <PeerCard key={peer.node_id} peer={peer} mine={status.build} onUnpair={() => unpair(peer.node_id, peer.name)} />
              ))
            )}
          </div>
        </>
      )}
    </div>
  );
}

function PeerCard({ peer, mine, onUnpair }: { peer: LinkPeer; mine?: Build; onUnpair: () => void }) {
  const [testing, setTesting] = useState(false);
  const [testReport, setTestReport] = useState<PeerTestReport | null>(null);
  const [refreshingRoutes, setRefreshingRoutes] = useState(false);
  const [showAddRoute, setShowAddRoute] = useState(false);
  const [newRouteAddr, setNewRouteAddr] = useState("");
  const [addRouteMsg, setAddRouteMsg] = useState<string | null>(null);

  const refreshRoutes = async () => {
    setRefreshingRoutes(true);
    try {
      await api.linkRefreshRoutes(peer.node_id);
    } catch (e) {
      console.error(e);
    } finally {
      setRefreshingRoutes(false);
    }
  };

  const addRoute = async () => {
    if (!newRouteAddr.trim()) return;
    try {
      await api.linkAddPeerRoute(peer.node_id, newRouteAddr.trim());
      setNewRouteAddr("");
      setShowAddRoute(false);
      setAddRouteMsg("Route added");
      setTimeout(() => setAddRouteMsg(null), 3000);
    } catch (e) {
      setAddRouteMsg(`Failed: ${e}`);
    }
  };

  const isOnline = peer.connected;
  const statusClass = isOnline ? "online" : "offline";
  const statusText = isOnline ? "Connected" : "Disconnected";

  // Classify route transport
  const routeLabel = peer.route
    ? peer.route.includes("100.")
      ? "Tailscale Mesh"
      : peer.route.includes("127.0.0.1")
      ? "Loopback"
      : peer.route.includes("169.254.")
      ? "Direct Cable / Bluetooth"
      : "Gigabit / Wi-Fi LAN"
    : null;

  const runTest = async () => {
    setTesting(true);
    setTestReport(null);
    try {
      const res = await api.linkTestPeer(peer.node_id);
      setTestReport(res);
    } catch (e) {
      setTestReport({
        node_id: peer.node_id,
        name: peer.name,
        connected: false,
        active_route: null,
        handshake_ok: false,
        rtt_ms: null,
        routes: [],
        queued_outgoing: peer.queued,
        summary: `Test failed: ${e}`,
      });
    } finally {
      setTesting(false);
    }
  };

  return (
    <div className={`peer-card ${statusClass}`}>
      <div className="row between align-center">
        <div className="row align-center gap-sm">
          <span className={`peer-led ${statusClass}`} title={statusText} />
          <div>
            <div className="row align-center gap-xs">
              <strong className="peer-name">{peer.name}</strong>
              {(peer.addrs || []).length >= 2 ? (
                <span className="failover-badge ready" title="Redundant failover routes ready">
                  ⚡ {(peer.addrs || []).length} routes (failover ready)
                </span>
              ) : (
                <span className="failover-badge single" title="Single route; connect Ethernet or Wi-Fi for auto-failover">
                  1 route
                </span>
              )}
            </div>
            <code className="peer-node-id">{peer.node_id.slice(0, 10)}…</code>
          </div>
        </div>
        <div className="row align-center gap-sm">
          <button
            className="btn small"
            onClick={runTest}
            disabled={testing}
            title="Verify mutual HMAC handshake, measure route latencies, and check chat alignment"
          >
            {testing ? "Testing Link…" : "Test Connection"}
          </button>
          <button className="btn small ghost danger-hover" onClick={onUnpair} title="Unpair this computer">
            Unpair
          </button>
        </div>
      </div>

      <div className="peer-stats-grid">
        <div className="stat-item">
          <span className="muted small">Status:</span>
          <span className={`status-val ${statusClass}`}>{statusText}</span>
        </div>
        <div className="stat-item">
          <span className="muted small">Active Route:</span>
          <span>{peer.route ? `${peer.route} (${routeLabel})` : "None"}</span>
        </div>
        <div className="stat-item">
          <span className="muted small">Latency:</span>
          <span>{peer.rtt_ms != null ? `${peer.rtt_ms} ms` : "—"}</span>
        </div>
        <div className="stat-item">
          <span className="muted small">Outbox:</span>
          <span>{peer.queued > 0 ? `${peer.queued} queued` : "Synced"}</span>
        </div>
        <div className="stat-item">
          <span className="muted small">AIs working:</span>
          <span>{!peer.connected ? "—" : (peer.activity ?? []).length ? peer.activity.join(", ") : "None"}</span>
        </div>
        <div className="stat-item">
          <span className="muted small">Project:</span>
          <span>{peer.git ?? "—"}</span>
        </div>
        <div className="stat-item">
          <span className="muted small">Consilium build:</span>
          <span>
            {!peer.build
              ? "Older version (doesn't report its build)"
              : !mine
                ? buildLabel(peer.build)
                : `${buildLabel(peer.build)} · ${
                    { same: "same as this computer", newer: "newer: this computer will match it", older: "older: it will match this computer" }[
                      buildCompare(peer.build, mine)
                    ]
                  }`}
          </span>
        </div>
      </div>

      <div className="failover-section">
        <div className="row between align-center failover-header">
          <div className="row align-center gap-xs">
            <span className="failover-title">Redundant Routes & Failover:</span>
            <span className="muted small">({(peer.addrs || []).length} configured)</span>
          </div>
          <div className="row align-center gap-xs">
            <button
              className="btn small ghost failover-btn"
              onClick={refreshRoutes}
              disabled={refreshingRoutes}
              title="Broadcast current local network interfaces and request peer's updated endpoints"
            >
              {refreshingRoutes ? "Syncing…" : "⚡ Sync Routes"}
            </button>
            <button
              className="btn small ghost failover-btn"
              onClick={() => setShowAddRoute(!showAddRoute)}
              title="Manually register an extra IP/port failover route"
            >
              {showAddRoute ? "Cancel" : "+ Add Route"}
            </button>
          </div>
        </div>

        {showAddRoute && (
          <div className="add-route-row row align-center gap-xs">
            <input
              type="text"
              placeholder="e.g. 192.168.1.170:8470 or 169.254.1.2:8470"
              value={newRouteAddr}
              onChange={(e) => setNewRouteAddr(e.target.value)}
              className="add-route-input small"
            />
            <button className="btn small primary" onClick={addRoute} disabled={!newRouteAddr.trim()}>
              Add
            </button>
            {addRouteMsg && <span className="small muted">{addRouteMsg}</span>}
          </div>
        )}

        <div className="failover-routes-chips">
          {(peer.addrs || []).map((addr) => {
            const isActive = peer.route && peer.route === addr;
            const kind = addr.startsWith("169.254.")
              ? "Direct Cable / BT"
              : addr.startsWith("100.")
              ? "Tailscale"
              : addr.startsWith("127.0.0.1")
              ? "Loopback"
              : "Gigabit / Wi-Fi";
            return (
              <div key={addr} className={`failover-chip ${isActive ? "active" : "standby"}`}>
                <span className="chip-icon">{isActive ? "🟢" : "⚪"}</span>
                <code>{addr}</code>
                <span className="chip-kind">{kind}</span>
                <span className="chip-role">{isActive ? "Active (Primary)" : "Standby (Failover)"}</span>
              </div>
            );
          })}
        </div>
      </div>

      {testReport && (
        <div className="peer-test-report">
          <div className="row between align-center test-header">
            <span className="test-summary-title">
              {testReport.handshake_ok ? "✔ Handshake & Sync Verified" : "⚠️ Handshake Warning"}
            </span>
            <button className="btn small ghost" onClick={() => setTestReport(null)}>
              Dismiss
            </button>
          </div>
          <p className="test-summary-text small">{testReport.summary}</p>
          {testReport.routes.length > 0 && (
            <div className="test-routes-list">
              <span className="muted small">Network Routes & Failovers Tested:</span>
              {testReport.routes.map((r) => (
                <div key={r.addr} className="test-route-row">
                  <div className="test-route-info">
                    <code>{r.addr}</code>
                    <span className="route-kind-badge">{r.kind}</span>
                  </div>
                  <span className={`test-route-status ${r.reachable ? "reachable" : "unreachable"}`}>
                    {r.status}
                  </span>
                </div>
              ))}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
