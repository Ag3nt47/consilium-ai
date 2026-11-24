import { useState } from "react";
import { api, type LanNote, type LinkPeer } from "./api";

export function LanNotes({ peers }: { peers: LinkPeer[] }) {
  const [recipient, setRecipient] = useState("");
  const [text, setText] = useState("");
  const [notes, setNotes] = useState<LanNote[]>([]);
  const [reading, setReading] = useState(false);
  const [sending, setSending] = useState(false);
  const [loaded, setLoaded] = useState(false);
  const [notice, setNotice] = useState("");
  const [error, setError] = useState("");
  const eligible = peers.filter((peer) => peer.connected && peer.supports_lan_notes);
  const selected = eligible.find((peer) => peer.node_id === recipient);
  const length = Array.from(text.trim()).length;

  async function refresh() {
    if (reading) return;
    setReading(true);
    setError("");
    try {
      setNotes(await api.linkLanNotes());
      setLoaded(true);
    } catch (failure) {
      setError(String(failure));
    } finally {
      setReading(false);
    }
  }

  async function send() {
    if (!selected || !length || length > 1000 || sending) return;
    setSending(true);
    setError("");
    setNotice("");
    try {
      await api.linkSendLanNote(recipient, text.trim());
      setText("");
      setNotice(`Note queued for ${selected.name}.`);
    } catch (failure) {
      setError(String(failure));
    } finally {
      setSending(false);
    }
  }

  return (
    <details className="small" onToggle={(event) => { if (event.currentTarget.open) void refresh(); }}>
      <summary>LAN notes</summary>
      <p className="muted small">Send a note directly to a linked computer. Notes stay outside council conversations.</p>
      <label className="row gap-sm">
        Recipient
        <select value={recipient} disabled={sending || eligible.length === 0} onChange={(event) => setRecipient(event.target.value)}>
          <option value="">Choose a computer</option>
          {recipient && !selected && <option value={recipient} disabled>Previous computer is unavailable</option>}
          {eligible.map(({ node_id, name }) => <option key={node_id} value={node_id}>{name}</option>)}
        </select>
      </label>
      {eligible.length === 0 && <p className="muted small">Connect a computer with LAN notes support to send.</p>}
      <textarea aria-label="LAN note" rows={3} value={text} disabled={sending}
        onChange={(event) => { setText(event.target.value); setNotice(""); }} />
      {length > 1000 && <p className="muted small">Notes are limited to 1,000 characters.</p>}
      <div className="row wrap gap-sm">
        <button type="button" className="btn small" disabled={!selected || !length || length > 1000 || sending} onClick={send}>
          {sending ? "Queuing note…" : "Send LAN note"}
        </button>
        <button type="button" className="btn small ghost" disabled={reading} onClick={refresh}>
          {reading ? "Reading notes…" : "Refresh received notes"}
        </button>
      </div>
      {error && <p role="alert">{error}</p>}
      {notice && <p role="status">{notice}</p>}
      {loaded && notes.length === 0 && <p className="muted small">No received notes yet.</p>}
      {notes.length > 0 && (
        <ul className="link-notes">
          {notes.map((note) => (
            <li key={note.id}>
              <strong>{note.source}</strong>{" · "}
              <time>{new Date(note.sent_at).toLocaleString()}</time>
              <p>{note.text}</p>
            </li>
          ))}
        </ul>
      )}
    </details>
  );
}
