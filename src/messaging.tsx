import { terminalName } from "./platform";
import { useCallback, useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { openUrl } from "@tauri-apps/plugin-opener";
import QRCode from "qrcode";
import { api, type ConnectorStatus, type MessagingSetup, type SignalSetupState, type SignalRelayStatus } from "./api";
import { relayExpiry, signalStatus } from "./signal-status.mjs";

const SIGNAL_CLI_GUIDE = "https://github.com/AsamK/signal-cli/wiki/Linking-other-devices-(Provisioning)";

function useSignalStatus(initial?: ConnectorStatus[] | null) {
  const [direct, setDirect] = useState<ConnectorStatus[] | null>(initial ?? null);
  const [relay, setRelay] = useState<SignalRelayStatus | null>(null);
  const [expiresAt, setExpiresAt] = useState(0);
  const [checking, setChecking] = useState(false);
  const [, expire] = useState(0);
  const current = useRef({ direct, relay, expiresAt });
  current.current = { direct, relay, expiresAt };
  const checkId = useRef(0);
  const check = useCallback(async () => {
    const id = ++checkId.current;
    setChecking(true);
    try {
      const result = await api.messagingStatus();
      if (id === checkId.current) setDirect(result);
    } catch {
      if (id === checkId.current) setDirect(null);
    } finally {
      if (id === checkId.current) setChecking(false);
    }
  }, []);

  useEffect(() => { if (initial) setDirect(initial); }, [initial]);
  useEffect(() => {
    let alive = true;
    let pending = false;
    let wasDirectActive: boolean | null = null;
    const poll = async () => {
      if (pending) return;
      pending = true;
      const started = performance.now();
      try {
        const report = await api.signalRelayStatus();
        if (!alive) return;
        setRelay(report);
        setExpiresAt(relayExpiry(report, started));
        if (wasDirectActive !== null && report.local_direct_active !== wasDirectActive) void check();
        wasDirectActive = report.local_direct_active;
      } catch {
        if (alive) { setRelay(null); setExpiresAt(0); }
      } finally { pending = false; }
    };
    void poll();
    void check();
    const fast = window.setInterval(poll, 1000);
    const slow = window.setInterval(() => {
      const c = current.current;
      // A healthy relay needs no repeated RPC against a missing local account.
      if (c.relay?.local_direct_active || signalStatus(c.direct, c.relay, c.expiresAt, performance.now()).mode !== "relay") void check();
    }, 30_000);
    const focus = () => { void poll(); void check(); };
    window.addEventListener("focus", focus);
    return () => {
      alive = false;
      ++checkId.current;
      clearInterval(fast);
      clearInterval(slow);
      window.removeEventListener("focus", focus);
    };
  }, [check]);
  useEffect(() => {
    const timer = window.setTimeout(() => expire((n) => n + 1), Math.max(0, expiresAt - performance.now()) + 1);
    return () => clearTimeout(timer);
  }, [expiresAt]);
  return { direct, setDirect, signal: signalStatus(direct, relay, expiresAt, performance.now()), checking, check };
}

/** Top-left indicator: a green check when a messaging app (Signal) answered the last check,
 *  a red cross when it's set up but not answering, grey when nothing is set up.
 *  Clicking it opens the messaging setup panel. */
export function MessagingIndicator() {
  const { direct, signal, checking, check } = useSignalStatus();
  const [open, setOpen] = useState(false);
  const list = [signal];
  const on = list.filter((c) => c.state !== "off");
  const allOk = on.length > 0 && on.every((c) => c.state === "ok");
  const state = on.length === 0 ? "off" : allOk ? "ok" : "down";
  const mark = state === "ok" ? "✓" : state === "down" ? "✗" : "○";
  const label = signal.mode === "relay" ? "Signal · relay" : on.length === 0 ? "Messaging off" : on.map((c) => c.name).join(", ");
  const tip =
    list.map((c) => `${c.state === "ok" ? "✓" : c.state === "down" ? "✗" : "○"} ${c.name}: ${c.detail}`).join("\n") +
    "\nRelay reports expire after 4 seconds; direct connections are checked separately. Click to open messaging setup.";

  return (
    <>
      <button
        className={`msg-indicator ${state}${checking ? " checking" : ""}`}
        title={tip}
        aria-label={`Messaging connection: ${state === "ok" ? "verified" : state === "down" ? "not working" : "not set up"}. Open messaging setup`}
        onClick={() => {
          setOpen(true);
          check();
        }}
      >
        <span className="msg-mark">{mark}</span>
        <span className="msg-label">{label}</span>
      </button>
      {open &&
        // Rendered on <body>: the top bar uses backdrop-filter in some themes, which would
        // otherwise trap this fixed-position window inside the header and squash the layout.
        createPortal(
        <div className="modal-backdrop" onClick={() => setOpen(false)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <h2>Messaging</h2>
            <MessagingSettings status={direct} onChanged={check} />
            <div className="row end">
              <button title="Close messaging setup" className="btn primary" onClick={() => setOpen(false)}>
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

/** Signal setup and settings. Used in the indicator's panel and in Settings. */
export function MessagingSettings({ status, onChanged }: { status?: ConnectorStatus[] | null; onChanged?: () => void }) {
  const [cfg, setCfg] = useState<MessagingSetup | null>(null);
  const { signal, direct: live, setDirect: setLive, check } = useSignalStatus(status);
  const [directSetup, setDirectSetup] = useState(false);
  const [account, setAccount] = useState("");
  const [endpoint, setEndpoint] = useState("");
  const [acis, setAcis] = useState("");
  const [enabled, setEnabled] = useState(false);
  const [forward, setForward] = useState(true);
  const [note, setNote] = useState<{ ok: boolean; text: string } | null>(null);
  const [busy, setBusy] = useState(false);
  const [link, setLink] = useState<SignalSetupState | null>(null);
  const [qr, setQr] = useState<string | null>(null);

  const fill = (c: MessagingSetup) => {
    setCfg(c);
    setEndpoint(c.endpoint);
    setAcis(c.allowed_acis.join("\n"));
    setEnabled(c.enabled);
    setForward(c.auto_forward_decisions);
    setAccount("");
  };

  const reload = async () => {
    try {
      fill(await api.messagingConfig());
    } catch (e) {
      setNote({ ok: false, text: String(e) });
    }
    try {
      await check();
    } catch {
      /* keep the last status */
    }
  };

  useEffect(() => {
    reload();
    window.addEventListener("focus", reload);
    return () => window.removeEventListener("focus", reload);
  }, []);

  // Follow the background install until it finishes.
  useEffect(() => {
    if (!cfg?.install_running) return;
    const t = window.setInterval(() => {
      api.messagingConfig().then(setCfg).catch(() => {});
    }, 3000);
    return () => window.clearInterval(t);
  }, [cfg?.install_running]);

  const save = async (nextEnabled = enabled) => {
    setBusy(true);
    try {
      const c = await api.messagingSave({
        enabled: nextEnabled,
        account: account.trim() ? account.trim() : null,
        endpoint,
        allowed_acis: acis.split(/[\s,]+/).filter(Boolean),
        auto_forward_decisions: forward,
      });
      fill(c);
      setNote({ ok: true, text: "Saved on this computer." });
      setLive(await api.messagingStatus().catch(() => live));
      onChanged?.();
    } catch (e) {
      setNote({ ok: false, text: String(e) });
    } finally {
      setBusy(false);
    }
  };

  // In-app link: QR code, background start, then a Note to Self round trip.
  const beginLink = async () => {
    try {
      setLink(await api.signalSetupBegin());
    } catch (e) {
      setNote({ ok: false, text: String(e) });
    }
  };

  useEffect(() => {
    api.signalSetupState().then(setLink).catch(() => {});
  }, []);

  const active = !!link && ["linking", "starting", "verifying"].includes(link.phase);
  useEffect(() => {
    if (!active) return;
    const t = window.setInterval(() => {
      api
        .signalSetupState()
        .then((s) => {
          setLink(s);
          if (s.phase === "verified") {
            api.messagingConfig().then(fill).catch(() => {});
            api.messagingStatus().then(setLive).catch(() => {});
            onChanged?.();
          }
        })
        .catch(() => {});
    }, 1500);
    return () => window.clearInterval(t);
  }, [active]);

  useEffect(() => {
    const uri = link?.phase === "linking" ? link.link_uri : null;
    if (!uri) return setQr(null);
    QRCode.toDataURL(uri, { margin: 1, width: 240, errorCorrectionLevel: "M" })
      .then(setQr)
      .catch(() => setQr(null));
  }, [link?.link_uri, link?.phase]);

  // Saving a number never starts phone linking; Show QR code is an explicit choice.
  const saveNumber = async () => {
    const a = account.trim();
    if (!a || busy) return;
    setBusy(true);
    try {
      const c = await api.messagingSave({
        enabled,
        account: a,
        endpoint,
        allowed_acis: acis.split(/[\s,]+/).filter(Boolean),
        auto_forward_decisions: forward,
      });
      fill(c);
      setNote({ ok: true, text: "Number saved on this computer." });
    } catch (e) {
      setNote({ ok: false, text: String(e) });
    } finally {
      setBusy(false);
    }
  };

  const step = async (action: "install" | "link" | "start") => {
    try {
      const title = await api.messagingSetup(action);
      if (action === "install" && !title.startsWith("Install signal-cli and qrencode")) {
        setNote({ ok: true, text: `${title}.` });
        setCfg(await api.messagingConfig());
      } else {
        setNote({ ok: true, text: `${title}: a ${terminalName} window opened. Press Enter there to run it.` });
      }
    } catch (e) {
      setNote({ ok: false, text: String(e) });
    }
  };

  const mark = signal?.state === "ok" ? "✓" : signal?.state === "down" ? "✗" : "○";
  const installed = !!cfg?.signal_cli;
  const directHealthy = live?.find((connector) => connector.id === "signal")?.state === "ok";
  const hasNumber = !!cfg?.account_hint;
  // Linked isn't enough: until the Note to Self code comes back, no sender is allowed and replies are ignored.
  const verified = (cfg?.allowed_acis.length ?? 0) > 0;

  return (
    <div className="msg-setup">
      <div className={`msg-status-row msg-indicator ${signal?.state ?? "off"}`} title="The result of the last connection check">
        <span className="msg-mark">{mark}</span>
        <span>
          <b>Signal</b> · {signal?.detail ?? "Checking…"}
        </span>
        <button title="Check the Signal connection now" className="btn ghost small-btn" onClick={reload}>
          Check now
        </button>
      </div>
      <p className="muted small">
        Get notifications on your phone and answer REQUESTs with Approve or Deny.
        {signal.mode === "relay" ? " Signal is available through your linked computer over encrypted PeerSync; no phone linking is needed here." :
          " A linked computer can provide Signal over encrypted PeerSync. Direct setup below uses signal-cli on this computer."}
      </p>
      <details open={directSetup || active} onToggle={(e) => setDirectSetup(e.currentTarget.open)}>
        <summary title="Choose direct phone linking on this computer">Set up Signal directly on this computer</summary>
        <p className="muted small">Use this only if you want this computer linked to your phone too. Your number stays in a private local file. A lost relay report never starts QR linking.</p>
      <ol className="msg-steps">
        <li className={installed ? "done" : ""}>
          <span>
            <b>Install signal-cli</b>
            <small>
              {installed
                ? `Installed at ${cfg?.signal_cli}`
                : cfg?.install_running
                  ? "Installing Java and signal-cli… this takes a few minutes."
                  : cfg?.install_error
                    ? `The last install stopped: ${cfg.install_error}`
                    : "The program that connects Consilium to Signal. One click, no password needed."}
            </small>
          </span>
          {!installed &&
            (cfg?.can_install ? (
              <button
                title="Download and install Java and signal-cli for this user, checked against fixed checksums. No password needed. On a Mac with Homebrew, it opens Terminal and uses Homebrew instead"
                className="btn small-btn"
                disabled={!cfg || cfg.install_running}
                onClick={() => step("install")}
              >
                {cfg?.install_running ? "Installing…" : cfg?.install_error ? "Try again" : "Install"}
              </button>
            ) : (
              <button title="Open signal-cli's install guide in your browser" className="link" onClick={() => openUrl(SIGNAL_CLI_GUIDE)}>
                Install guide
              </button>
            ))}
        </li>
        <li className={hasNumber ? "done" : ""}>
          <span>
            <b>Your Signal phone number</b>
            <small>{hasNumber ? `Saved: ${cfg?.account_hint}. Type a new one to replace it.` : "International form, like +15551234567."}</small>
          </span>
          <input
            className="msg-input"
            type="tel"
            autoComplete="off"
            placeholder={hasNumber ? cfg?.account_hint : "+15551234567"}
            value={account}
            onChange={(e) => setAccount(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && saveNumber()}
            onBlur={() => saveNumber()}
            title="The phone number of your Signal account. Press Enter to save it"
          />
          {account.trim() && (
            <button title="Save this number on this computer" className="btn small-btn" disabled={busy} onClick={saveNumber}>
              Save
            </button>
          )}
        </li>
        <li className={link?.phase === "verified" || (verified && directHealthy && !active) ? "done" : ""}>
          <span>
            <b>Link this computer</b>
            <small>
              {link && link.phase !== "idle"
                ? link.detail
                : installed && hasNumber && !verified
                  ? "Phone linking and verification are not complete. Select Show QR code to continue, then reply to the code in Note to Self. Existing Signal account data is kept."
                  : `Shows a QR code here to scan with your phone. This computer appears as "${link?.device_name ?? "Consilium"}". With several paired computers, link only one.`}
            </small>
          </span>
          {!active && (
            <button
              title="Show a QR code to link this computer to your Signal account, then test it"
              className="btn small-btn"
              disabled={!installed || !hasNumber}
              onClick={beginLink}
            >
              {link?.phase === "failed" ? "Try again" : link?.phase === "verified" ? "Link again" : "Show QR code"}
            </button>
          )}
          {active && (
            <button title="Stop linking" className="btn ghost small-btn" onClick={() => api.signalSetupCancel().then(() => setLink(null))}>
              Cancel
            </button>
          )}
          {qr && (
            <div className="msg-qr">
              <img src={qr} alt="Signal link QR code" width={240} height={240} />
            </div>
          )}
          {link?.phase === "starting" && <div className="msg-qr-note">Starting Signal…</div>}
          {link?.phase === "verifying" && link.code && (
            <div className="msg-qr-note">
              Open <b>Note to Self</b> in Signal on your phone and send <span className="msg-code">{link.code}</span>
            </div>
          )}
        </li>
      </ol>

      <label className="toggle">
        <input type="checkbox" disabled={!cfg} checked={enabled} onChange={(e) => setEnabled(e.target.checked)} />
        Use Signal for notifications and approvals
      </label>
      <label className="toggle">
        <input type="checkbox" disabled={!cfg} checked={forward} onChange={(e) => setForward(e.target.checked)} />
        Let Approve and Deny from my phone answer pending REQUESTs
      </label>

      <details className="msg-advanced">
        <summary title="Connection address and allowed senders">Advanced</summary>
        <label className="msg-field">
          <span>Local address of signal-cli</span>
          <input className="msg-input" value={endpoint} onChange={(e) => setEndpoint(e.target.value)} title="Must be on this computer, like 127.0.0.1:7583" />
        </label>
        <label className="msg-field">
          <span>Allowed sender IDs (Signal ACI UUIDs, one per line)</span>
          <textarea className="msg-input" rows={2} value={acis} onChange={(e) => setAcis(e.target.value)} title="Only messages from these Signal accounts are accepted" />
        </label>
        <small className="muted">Settings file: {cfg?.config_path}</small>
      </details>

      <div className="msg-save-row">
        {note && <span className={note.ok ? "msg-note ok" : "msg-note err"}>{note.text}</span>}
        <button title="Save these messaging settings on this computer" className="btn primary small-btn" disabled={busy || !cfg} onClick={() => save()}>
          Save
        </button>
      </div>
      </details>
    </div>
  );
}
