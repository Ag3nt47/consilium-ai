import { useEffect, useState } from "react";
import { createPortal } from "react-dom";
import { api, buildCompare, buildLabel, type Build, type LinkPeer, type LinkStatus, type PeerTestReport, type StressReport } from "./api";
import { LocalModelFit } from "./LocalModelFit";

/** Topbar status light and button for multi-computer sync (Mac & Windows).
 *  Displays connection health, fastest active route, and opens the sync setup modal on click. */
export function LinkIndicator({ onConnectedCount }: { onConnectedCount?: (count: number) => void }) {
  const [status, setStatus] = useState<LinkStatus | null>(null);
  const [open, setOpen] = useState(false);
  const [checking, setChecking] = useState(false);
  const [showDiscoverySettings, setShowDiscoverySettings] = useState(false);
  const [pairAddress, setPairAddress] = useState("");
  const [discoveryAlerts, setDiscoveryAlerts] = useState(() =>
    localStorage.getItem("consilium.peerlink.discovery-alerts") !== "off"
  );
  const [candidates, setCandidates] = useState<string[]>([]);
  const [dismissed, setDismissed] = useState<string[]>([]);
  const linkEnabled = status?.enabled === true;
  useEffect(() => {
    onConnectedCount?.(linkEnabled ? status?.peers.filter((peer) => peer.connected).length ?? 0 : 0);
  }, [linkEnabled, onConnectedCount, status]);

  useEffect(() => {
    if (!linkEnabled || !discoveryAlerts) return;
    let active = true;
    let timer: ReturnType<typeof setTimeout>;
    async function scan() {
      try {
        const found = await api.linkDiscoverLan();
        if (active) setCandidates(found);
      } catch {
        if (active) setCandidates([]);
      } finally {
        if (active) timer = setTimeout(scan, 120_000);
      }
    }
    void scan();
    return () => {
      active = false;
      clearTimeout(timer);
    };
  }, [linkEnabled, discoveryAlerts]);

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
  const knownEndpoints = new Set(peers.flatMap((peer) => peer.addrs ?? []));
  const candidate = enabled && discoveryAlerts
    ? candidates.find((address) => !knownEndpoints.has(address) && !dismissed.includes(address))
    : undefined;

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
          setPairAddress("");
          setShowDiscoverySettings(false);
          setOpen(true);
          refresh();
        }}
      >
        <span className="link-mark">{mark}</span>
        <span className="link-label">{label}</span>
      </button>

      {candidate && !open && (
        <div className="row wrap gap-sm" role="status">
          <span>Connection found. Would you like to connect?</span>
          <button className="btn small" onClick={() => {
            setPairAddress(candidate);
            setShowDiscoverySettings(false);
            setOpen(true);
          }}>Connect</button>
          <button className="btn small ghost" onClick={() =>
            setDismissed((previous) => [...previous, candidate])
          }>Dismiss</button>
          <button className="btn small ghost" onClick={() => {
            setPairAddress("");
            setShowDiscoverySettings(true);
            setOpen(true);
          }}>Discovery settings</button>
        </div>
      )}

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
              <details open={showDiscoverySettings || undefined}>
                <summary>Discovery settings</summary>
                <label className="row gap-sm small">
                  <input type="checkbox" checked={discoveryAlerts} onChange={(event) => {
                    const enabled = event.target.checked;
                    localStorage.setItem("consilium.peerlink.discovery-alerts", enabled ? "on" : "off");
                    setDiscoveryAlerts(enabled);
                    if (!enabled) setCandidates([]);
                  }} />
                  Look for new PeerLink devices while the direct link is enabled
                </label>
              </details>
              <LinkSettings status={status} onRefresh={refresh} pairAddress={pairAddress} />
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
export function LinkSettings({ status: initialStatus, onRefresh, pairAddress = "" }: { status?: LinkStatus | null; onRefresh?: () => void; pairAddress?: string }) {
  const [status, setStatus] = useState<LinkStatus | null>(initialStatus ?? null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [mode, setMode] = useState<"host" | "join">(pairAddress ? "join" : "host");
  // The communication stress test: running, and its last results (also posted in the chat).
  const [stressing, setStressing] = useState(false);
  const [stress, setStress] = useState<StressReport[] | null>(null);
  const runStress = async () => {
    setStressing(true);
    setError(null);
    try {
      setStress(await api.stressTestLink());
    } catch (e) {
      setError(`Stress test failed: ${e}`);
    } finally {
      setStressing(false);
    }
  };

  // Host mode (showing code)
  const [hostCode, setHostCode] = useState<string | null>(null);
  const [expiresAt, setExpiresAt] = useState<number | null>(null);
  const [timeLeft, setTimeLeft] = useState<string>("");
  const [copied, setCopied] = useState<string | null>(null);

  // Join mode (entering code)
  const [joinAddr, setJoinAddr] = useState(pairAddress);
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
    if (initialStatus) {
      setStatus(initialStatus);
      return;
    }
    void loadStatus();
    const timer = window.setInterval(loadStatus, 5000);
    return () => window.clearInterval(timer);
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

  const autoDiscover = async () => {
    setAutoDiscovering(true);
    setError(null);
    setDiscoveredAddrs([]);
    try {
      const found = await api.linkDiscoverLan();
      setDiscoveredAddrs(found);
      if (!found.length) {
        setError("No PeerLink devices answered. Enable the direct link on the other computer and check the network, or enter its address manually.");
      }
    } catch (error) {
      setError("Could not scan the local network: " + error);
    } finally {
      setAutoDiscovering(false);
    }
  };

  if (!status) {
    return <div className="muted small">Loading sync status…</div>;
  }

  const peers = status.peers || [];

  return (
    <div className="link-settings">
      {error && <div className="link-error small">{error}</div>}
      {status.enabled && status.error && (
        <div className="link-error small" role="status">
          <strong>Incoming connections unavailable.</strong> {status.error}
          {" "}This computer may still connect out. Check whether another app owns the port, then turn the direct link off and on.
        </div>
      )}

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

            <div className="stress-test" style={{ marginTop: "8px" }}>
              <button
                className="btn small"
                onClick={runStress}
                disabled={stressing || !(status.peers ?? []).some((p) => p.connected)}
                title="Tries each configured endpoint in turn and checks for an authenticated packet. Shows the local interface when available, then tries to reconnect using the saved endpoints. Results are posted in chat. Physical failover requires a separate test with a transport disconnected."
              >
                {stressing ? "Stress testing routes…" : "Communication stress test"}
              </button>
              {stress?.map((r) => {
                const passed = r.routes.filter((route) => route.ok);
                const interfaces = [...new Set(passed.flatMap(({ local_interface }) => local_interface ? [local_interface] : []))];
                return (
                  <div key={r.computer} className="stress-report">
                    <div className="muted small">
                      {r.computer}:{" "}
                      {r.routes.length === 0
                        ? "not connected, nothing tested"
                        : `${r.routes.filter((x) => x.ok).length} of ${r.routes.length} endpoints carried traffic`}
                    </div>
                    {r.restored && (
                      <div className="muted small">
                        {r.restored.ok ? `Reconnected through ${r.restored.addr}` : "Reconnection unconfirmed"}
                        {r.restored.local_interface && ` via ${r.restored.local_interface}`}
                        {r.restored.local_addr && `, source ${r.restored.local_addr}`}
                        {" · "}{r.restored.detail}
                      </div>
                    )}
                    {passed.length > 0 && (
                      <div className="muted small">
                        {interfaces.length > 0
                          ? `Local interfaces that carried authenticated traffic: ${interfaces.join(", ")}.`
                          : "The local interface could not be identified for any successful endpoint."}
                        {interfaces.length < 2 && " Connect another interface and rerun the test to check a second physical path."}
                      </div>
                    )}
                    {r.routes.map((x) => (
                      <div key={x.addr} className={`handshake-item ${x.ok ? "acknowledged" : "pending"}`}>
                        <span className="handshake-mark">{x.ok ? "✓" : "✗"}</span> {x.kind} <code>{x.addr}</code>{" "}
                        <span className="muted small">
                          {x.detail}{x.ok && `, local interface ${x.local_interface ?? "unavailable"}${x.local_addr ? `, source ${x.local_addr}` : ""}`}
                        </span>
                      </div>
                    ))}
                  </div>
                );
              })}
            </div>

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
            Pair Another Computer
          </div>

          <div className="link-card">
            <details className="small">
              <summary>Walk me through setup and failover testing</summary>
              <ol>
                <li>Open Consilium on both computers and enable the direct link. Connect them to the same trusted network using Ethernet or Wi-Fi.</li>
                <li>On the first computer, choose <strong>Show Pairing Code</strong>. Generate a code and copy one of its local addresses above.</li>
                <li>On the second computer, choose <strong>Join Another Computer</strong>. Enter the address and code from the first computer, then pair. Auto-Find on LAN can suggest addresses.</li>
                <li>On the linked computer card, choose <strong>Test Connection</strong>. Look for an authenticated active session and check each endpoint separately. An amber result means the address answered on TCP but did not confirm the paired computer. If an endpoint fails, verify the address, confirm Consilium is running on that computer, and check that its firewall allows Consilium on the trusted network.</li>
                <li>For a backup path, connect another interface on both computers. Keep Ethernet connected while joining Wi-Fi. For Bluetooth, first establish a Bluetooth network connection in the operating system. Add the other computer’s address on the backup path if it is missing.</li>
                <li>Run <strong>Communication stress test</strong> above and check that authenticated traffic used two distinct local interfaces. With access to both computers, disconnect the active path, check that the alternate interface becomes active, then restore the first path.</li>
              </ol>
            </details>
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
                        title="Look for devices running PeerLink on directly connected local networks"
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
                        <span className="muted small">PeerLink endpoints found. Pair with a code to verify the device.</span>
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
          {peers.length > 0 && <LocalModelFit />}

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
  const [testingHandshake, setTestingHandshake] = useState(false);
  const [handshakeResult, setHandshakeResult] = useState<string | null>(null);
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

  const routeCount = peer.addrs?.length ?? 0;
  const routeLabel = peer.local_interface ?? "Interface unavailable";

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

  const testAiHandshake = async () => {
    setTestingHandshake(true);
    setHandshakeResult(null);
    try {
      const report = await api.linkTestAiHandshake(peer.node_id);
      const confirmed = report.filter((ai) => ai.reads && ai.read).length;
      setHandshakeResult(report.some((ai) => ai.side === "there")
        ? `Fresh AI status received. ${confirmed} of ${report.length} AIs have confirmations in both directions.`
        : "Fresh AI status received. The linked computer reported no active AIs.");
    } catch (error) {
      setHandshakeResult(String(error));
    } finally {
      setTestingHandshake(false);
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
              <span className="failover-badge single" title="Configured endpoints may share a physical interface. Run the Communication stress test to check which interfaces carry authenticated traffic.">
                {routeCount} configured {routeCount === 1 ? "endpoint" : "endpoints"}
              </span>
            </div>
            <code className="peer-node-id">{peer.node_id.slice(0, 10)}…</code>
          </div>
        </div>
        <div className="row align-center gap-sm">
          <button
            className="btn small"
            onClick={runTest}
            disabled={testing}
            title="Check PeerLink endpoint reachability and authentication"
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

      {peer.connected && (
        <div className="handshake-section">
          <div className="row between align-center wrap">
            <span className="failover-title">AI handshake:</span>
            <button className="btn small ghost" disabled={testingHandshake} onClick={testAiHandshake}
              title="Check the linked computer's latest AI status and confirmations without sending prompts">
              {testingHandshake ? "Checking AI handshake…" : "Test AI handshake"}
            </button>
            <span className="muted small">
              {(peer.handshake ?? []).length === 0
                ? "Waiting for that computer to report its AIs (it needs this version)"
                : (peer.handshake ?? []).every((h) => h.reads && h.read)
                  ? "✓ Every AI has heard the other computer"
                  : "A ✓ appears once an AI has read the other computer's AIs and been read by them"}
            </span>
          </div>
          {handshakeResult && <p className="small muted" role="status">{handshakeResult}</p>}
          <div className="handshake-list">
            {(peer.handshake ?? []).map((h) => (
              <div key={`${h.side}-${h.ai}`} className={`handshake-item ${h.reads && h.read ? "acknowledged" : "pending"}`}>
                <span className="handshake-mark" aria-label={h.reads && h.read ? "acknowledged" : "not yet"}>
                  {h.reads && h.read ? "✓" : "…"}
                </span>{" "}
                <code>{h.handle}</code>{" "}
                <span className="muted small">
                  {h.side === "here" ? "this computer" : peer.name} ·{" "}
                  {h.reads ? "read theirs" : "hasn't read theirs yet"}, {h.read ? "read by them" : "not read by them yet"}
                </span>
              </div>
            ))}
          </div>
        </div>
      )}

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
            return (
              <div key={addr} className={`failover-chip ${isActive ? "active" : "standby"}`}>
                <span className="chip-icon">{isActive ? "🟢" : "⚪"}</span>
                <code>{addr}</code>
                {isActive && peer.local_interface && <span className="chip-kind">{peer.local_interface}</span>}
                <span className="chip-role">{isActive ? "Active route" : "Candidate route"}</span>
              </div>
            );
          })}
        </div>
      </div>

      {testReport && (
        <div className="peer-test-report">
          <div className="row between align-center test-header">
            <span className="test-summary-title">
              {testReport.handshake_ok ? "✔ Active session authenticated" : "⚠️ No authenticated active session"}
            </span>
            <button className="btn small ghost" onClick={() => setTestReport(null)}>
              Dismiss
            </button>
          </div>
          <p className="test-summary-text small">{testReport.summary}</p>
          {testReport.routes.length > 0 && (
            <div className="test-routes-list">
              <span className="muted small">TCP reachability and peer authentication by endpoint</span>
              <span className="small">{testReport.routes.filter((route) => route.authenticated_ms != null).length} of {testReport.routes.length} endpoints authenticated</span>
              {testReport.routes.map((r) => (
                <div key={r.addr} className="test-route-row">
                  <div className="test-route-info">
                    <code>{r.addr}</code>
                    <span className="route-kind-badge">{r.kind}</span>
                    {r.local_ip && (
                      <span className="muted small">via {r.local_interface ? `${r.local_interface} (${r.local_ip})` : r.local_ip}</span>
                    )}
                  </div>
                  <span className={`test-route-status ${r.authenticated_ms != null ? "authenticated" : r.reachable ? "unconfirmed" : "unreachable"}`}>
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
