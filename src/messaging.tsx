import { useEffect, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { api, type ConnectorStatus, type MessagingSetup } from "./api";

const SIGNAL_CLI_GUIDE = "https://github.com/AsamK/signal-cli/wiki/Linking-other-devices-(Provisioning)";

/** Top-left indicator: a green check when a messaging app (Signal) answered the last check,
 *  a red cross when it's set up but not answering, grey when nothing is set up.
 *  Clicking it opens the messaging setup panel. */
export function MessagingIndicator() {
  const [list, setList] = useState<ConnectorStatus[] | null>(null);
  const [checking, setChecking] = useState(false);
  const [open, setOpen] = useState(false);

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
    "\nChecked every 30 seconds. Click to open messaging setup.";

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
      {open && (
        <div className="modal-backdrop" onClick={() => setOpen(false)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <h2>Messaging</h2>
            <MessagingSettings status={list} onChanged={check} />
            <div className="row end">
              <button title="Close messaging setup" className="btn primary" onClick={() => setOpen(false)}>
                Done
              </button>
            </div>
          </div>
        </div>
      )}
    </>
  );
}

/** Signal setup and settings. Used in the indicator's panel and in Settings. */
export function MessagingSettings({ status, onChanged }: { status?: ConnectorStatus[] | null; onChanged?: () => void }) {
  const [cfg, setCfg] = useState<MessagingSetup | null>(null);
  const [live, setLive] = useState<ConnectorStatus[] | null>(status ?? null);
  const [account, setAccount] = useState("");
  const [endpoint, setEndpoint] = useState("");
  const [acis, setAcis] = useState("");
  const [enabled, setEnabled] = useState(false);
  const [forward, setForward] = useState(true);
  const [note, setNote] = useState<{ ok: boolean; text: string } | null>(null);
  const [busy, setBusy] = useState(false);

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
      setLive(await api.messagingStatus());
    } catch {
      /* keep the last status */
    }
  };

  useEffect(() => {
    reload();
    window.addEventListener("focus", reload);
    return () => window.removeEventListener("focus", reload);
  }, []);

  useEffect(() => {
    if (status) setLive(status);
  }, [status]);

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

  const step = async (action: "install" | "link" | "start") => {
    try {
      const title = await api.messagingSetup(action);
      if (action === "install" && !title.startsWith("Install signal-cli and qrencode")) {
        setNote({ ok: true, text: `${title}.` });
        setCfg(await api.messagingConfig());
      } else {
        setNote({ ok: true, text: `${title}: a Terminal window opened. Press Enter there to run it.` });
      }
    } catch (e) {
      setNote({ ok: false, text: String(e) });
    }
  };

  const signal = live?.find((c) => c.id === "signal");
  const mark = signal?.state === "ok" ? "✓" : signal?.state === "down" ? "✗" : "○";
  const installed = !!cfg?.signal_cli;
  const hasNumber = !!cfg?.account_hint;

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
        Get notifications on your phone and answer REQUESTs with Approve or Deny. Consilium talks to signal-cli on this
        computer only. Your number is saved in a private file on this computer and is never committed or shown in full.
      </p>

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
                title="Download and install Java and signal-cli for this user, checked against fixed checksums. No password needed. If Homebrew is installed, it opens Terminal and uses Homebrew instead"
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
            title="The phone number of your Signal account"
          />
        </li>
        <li>
          <span>
            <b>Link this computer</b>
            <small>Shows a QR code. On your phone: Signal › Settings › Linked devices › Link new device.</small>
          </span>
          <button title="Open Terminal with the link command and QR code" className="btn small-btn" disabled={!installed} onClick={() => step("link")}>
            Link
          </button>
        </li>
        <li className={signal?.state === "ok" ? "done" : ""}>
          <span>
            <b>Start Signal for Consilium</b>
            <small>Runs signal-cli in a Terminal window on this computer only. Keep that window open.</small>
          </span>
          <button
            title="Open Terminal running signal-cli for Consilium; save your number first"
            className="btn small-btn"
            disabled={!installed || !hasNumber}
            onClick={() => step("start")}
          >
            Start
          </button>
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
    </div>
  );
}
