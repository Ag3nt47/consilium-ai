import { useState } from "react";
import { api, type LinkPeer, type ProbeSource, type TransferPath, type TransferTrial } from "./api";

const emptyPath = (): TransferPath => ({ source: "", endpoint: "" });

export function LinkTransferTrial({ peers, disabled }: { peers: LinkPeer[]; disabled: boolean }) {
  const [peerId, setPeerId] = useState("");
  const [sources, setSources] = useState<ProbeSource[]>([]);
  const [paths, setPaths] = useState<TransferPath[]>([emptyPath(), emptyPath()]);
  const [loading, setLoading] = useState(false);
  const [running, setRunning] = useState(false);
  const [report, setReport] = useState<TransferTrial | null>(null);
  const [error, setError] = useState("");
  const peer = peers.find((entry) => entry.node_id === peerId && entry.connected);
  const locked = disabled || running || loading;
  const interfaces = paths.map(({ source }) => sources.find(({ address }) => address === source)?.interface);
  const valid = !!peer && interfaces.every(Boolean) && new Set(interfaces).size === paths.length
    && paths.every(({ endpoint }) => peer.addrs.includes(endpoint))
    && new Set(paths.map(({ endpoint }) => endpoint)).size === paths.length;

  const refresh = async () => {
    if (locked) return;
    setLoading(true);
    setReport(null);
    setError("");
    try {
      setSources(await api.linkProbeSources());
    } catch (failure) {
      setSources([]);
      setError(String(failure));
    } finally {
      setLoading(false);
    }
  };
  const run = async () => {
    if (!valid || locked) return;
    setRunning(true);
    setReport(null);
    setError("");
    try {
      setReport(await api.linkTransferTrial(peerId, paths));
    } catch (failure) {
      setError(String(failure));
    } finally {
      setRunning(false);
    }
  };
  const edit = (index: number, key: keyof TransferPath, value: string) => {
    setPaths((current) => current.map((path, row) => row === index ? { ...path, [key]: value } : path));
    setReport(null);
  };

  return (
    <details className="stress-test small" onToggle={(event) => { if (event.currentTarget.open) void refresh(); }}>
      <summary>Test parallel payload transfer</summary>
      <p>Match each local adapter with the address on the other computer that should receive its traffic. The test sends 32 KiB of authenticated sample data per path while the live session continues.</p>
      <label className="row gap-sm">
        Test computer
        <select value={peerId} disabled={locked} onChange={(event) => {
          setPeerId(event.target.value);
          setPaths([emptyPath(), emptyPath()]);
          setReport(null);
        }}>
          <option value="">Choose a computer</option>
          {peers.map(({ node_id, name, connected }) => (
            <option key={node_id} value={node_id} disabled={!connected}>{name}{connected ? "" : " (offline)"}</option>
          ))}
        </select>
      </label>
      <button type="button" className="btn small ghost" disabled={locked} onClick={refresh}>
        {loading ? "Refreshing transfer adapters…" : "Refresh transfer adapters"}
      </button>
      {peer && peer.addrs.length < 2 && <p>Add at least two addresses on the peer card to test separate paths.</p>}
      {paths.map(({ source, endpoint }, index) => (
        <div className="row wrap gap-sm" key={index}>
          <label className="row gap-sm">
            Path {index + 1} local adapter
            <select value={source} disabled={locked} onChange={(event) => edit(index, "source", event.target.value)}>
              <option value="">Choose an adapter</option>
              {source && !interfaces[index] && <option value={source} disabled>{source} (unavailable)</option>}
              {sources.map(({ address, interface: adapter }) => (
                <option key={address} value={address} disabled={interfaces.some((name, row) => row !== index && name === adapter)}>
                  {adapter} ({address})
                </option>
              ))}
            </select>
          </label>
          <label className="row gap-sm">
            Peer endpoint
            <select value={endpoint} disabled={locked || !peer} onChange={(event) => edit(index, "endpoint", event.target.value)}>
              <option value="">Choose an endpoint</option>
              {endpoint && !peer?.addrs.includes(endpoint) && <option value={endpoint} disabled>{endpoint} (unavailable)</option>}
              {peer?.addrs.map((address) => (
                <option key={address} value={address} disabled={paths.some((path, row) => row !== index && path.endpoint === address)}>{address}</option>
              ))}
            </select>
          </label>
        </div>
      ))}
      <div className="row wrap gap-sm">
        <button type="button" className="btn small ghost" disabled={locked || paths.length >= 4} onClick={() => {
          setPaths((current) => [...current, emptyPath()]);
          setReport(null);
        }}>Add path</button>
        <button type="button" className="btn small ghost" disabled={locked || paths.length <= 2} onClick={() => {
          setPaths((current) => current.slice(0, -1));
          setReport(null);
        }}>Remove last path</button>
        <button type="button" className="btn small" disabled={locked || !valid} onClick={run}>
          {running ? "Testing payload transfer…" : "Run payload transfer test"}
        </button>
      </div>
      {error && <p role="alert">{error}</p>}
      {report && (
        <div className="stress-report" role="status">
          <p>{report.overlap_ms == null ? "Overlap across all selected paths was unconfirmed." : `Receiver observed overlapping receipt windows for ${report.overlap_ms} ms.`}</p>
          {report.paths.map(({ source, endpoint, receipts, error: failure }) => (
            <div className={`handshake-item ${failure ? "pending" : "acknowledged"}`} key={source.address}>
              {source.interface} <code>{source.address}</code> → <code>{endpoint}</code>
              {" · "}{receipts.reduce((bytes, receipt) => bytes + receipt.bytes, 0).toLocaleString()} bytes checksum-verified
              {receipts.length > 0 && ` · Receiver ${receipts[0].receiver_ip} · ${receipts[receipts.length - 1].received_ms - receipts[0].received_ms} ms between first and last receipt`}
              {failure && ` · ${failure}`}
            </div>
          ))}
        </div>
      )}
    </details>
  );
}
