import { useState } from "react";
import { api } from "./api";

import { gib, modelFit, weightBudgets } from "./model-fit.mjs";

type Device = { id: number | string; name: string; memory: string; runners?: string[] };
const memoryInput = (gib: number) => (Math.floor(Math.max(0, gib) * 10) / 10).toFixed(1);
const runnerNames = new Set(["ollama", "llama-cli", "llama-server", "ggml-rpc-server", "rpc-server"]);
const deviceName = (name: string) => name.trim().toLowerCase();

export function LocalModelFit({ availableGiB }: { availableGiB?: number }) {
  const [devices, setDevices] = useState<Device[]>([]);
  const [nextId, setNextId] = useState(1);
  const [hostMemory, setHostMemory] = useState<string | null>(null);
  const [bits, setBits] = useState(4);
  const [overhead, setOverhead] = useState("2");
  const [customWeights, setCustomWeights] = useState("");
  const [fileMessage, setFileMessage] = useState("");
  const [scanning, setScanning] = useState(false);
  const [scanStatus, setScanStatus] = useState("");
  const [copyStatus, setCopyStatus] = useState("");
  const host = hostMemory ?? (availableGiB && availableGiB > 0 ? memoryInput(availableGiB) : "");
  const reportedNames = new Set(devices.filter((device) => typeof device.id === "string").map((device) => deviceName(device.name)));
  const included = devices.filter((device) => typeof device.id === "string" || !reportedNames.has(deviceName(device.name)));
  const budgets = [{ name: "This computer", memory: host }, ...included];
  const models = modelFit(budgets, bits, overhead, customWeights);
  const capacities = weightBudgets(budgets, overhead);
  const totalCapacity = capacities.reduce((total, device) => total + device.capacity, 0);

  async function copyEstimate() {
    const report = [
      "Consilium local model memory estimate",
      `Catalog precision: ${bits}-bit. Runtime allowance: ${gib(Number(overhead))} per device.`,
      `Combined weight capacity: ${gib(totalCapacity)}.`,
      ...budgets.map(({ name, memory }, index) => `${name || "Unnamed device"}: ${memory ? gib(Number(memory)) : "memory not entered"} usable, ${gib(capacities[index].capacity)} for weights.`),
      "",
      ...models.map(({ name, weights, fit }) => `${name}: ${gib(weights)} weights. ${fit}.`),
      "",
      "Memory estimate only. Verify runner compatibility and inference performance separately.",
    ].join("\n");
    try {
      await navigator.clipboard.writeText(report);
      setCopyStatus("Memory estimate copied.");
    } catch {
      setCopyStatus("Could not copy the memory estimate.");
    }
  }

  async function scanPeers() {
    setScanning(true);
    setScanStatus("");
    try {
      const before = await api.peerResources();
      await api.requestPeerResources();
      await new Promise((resolve) => setTimeout(resolve, 3000));
      const reports = await api.peerResources();
      const found = Object.entries(reports).flatMap(([name, report]) => {
        if (report.updated_at <= (before[name]?.updated_at ?? 0) || !report.total_memory_mb) return [];
        const availableMiB = report.available_memory_mb ?? report.total_memory_mb * (1 - report.memory_percent / 100);
        const memory = Math.max(0, Math.min(report.total_memory_mb, availableMiB)) / 1024;
        const runners = report.tools?.filter((tool) => runnerNames.has(tool)) ?? [];
        return [{ id: name, name, memory: memoryInput(memory), runners }];
      });
      setDevices((previous) => [
        ...previous.filter((device) => typeof device.id === "number"),
        ...found,
      ]);
      setScanStatus(found.length
        ? `Imported ${found.length} fresh paired-computer report(s).`
        : "No new memory reports arrived. Previous paired estimates were cleared. Check that the computers are online and updated, or enter memory manually.");
    } catch {
      setDevices((previous) => previous.filter((device) => typeof device.id === "number"));
      setScanStatus("Peer capability scan unavailable. Previous paired estimates were cleared. Enter device memory manually.");
    } finally {
      setScanning(false);
    }
  }

  return (
    <details className="small">
      <summary>Check local model memory</summary>
      <p className="muted">Use the paired-computer scan or enter usable memory after leaving room for the OS and other apps. Count unified memory once.</p>
      <button type="button" className="btn ghost small-btn" disabled={scanning} onClick={() => void scanPeers()}>
        {scanning ? "Checking paired computers…" : "Check paired computers"}
      </button>
      {scanStatus && <p className="muted" role="status">{scanStatus}</p>}
      <div className="custom-ai-fields">
        <label>
          This computer usable memory (GiB)
          <input type="number" min="0" step="0.1" value={host} onChange={(e) => setHostMemory(e.target.value)} />
        </label>
        {devices.map((device) => (
          <div className="row wrap" key={device.id}>
            <label>
              Device {device.id} name
              <input value={device.name} onChange={(e) => {
                const name = e.target.value;
                setDevices((previous) => previous.map((item) => item.id === device.id ? { ...item, name } : item));
              }} />
            </label>
            <label>
              Device {device.id} usable memory (GiB)
              <input type="number" min="0" step="0.1" value={device.memory} onChange={(e) => {
                const memory = e.target.value;
                setDevices((previous) => previous.map((item) => item.id === device.id ? { ...item, memory } : item));
              }} />
            </label>
            <button type="button" className="btn ghost small-btn" onClick={() => setDevices((previous) => previous.filter((item) => item.id !== device.id))}>Remove device {device.id}</button>
            {device.runners && <span className="muted">Model tools detected: {device.runners.join(", ") || "none reported"}.</span>}
            {!included.includes(device) && <span className="muted">Excluded because a paired report uses the same name.</span>}
          </div>
        ))}
        <button type="button" className="btn ghost small-btn" onClick={() => {
          setDevices((previous) => [...previous, { id: nextId, name: nextId === 1 ? "Mac" : `Device ${nextId}`, memory: "" }]);
          setNextId((previous) => previous + 1);
        }}>Add a device to estimate</button>
        <label>
          Weight precision
          <select value={bits} onChange={(e) => setBits(Number(e.target.value))}>
            <option value={4}>4-bit</option>
            <option value={8}>8-bit</option>
            <option value={16}>16-bit</option>
          </select>
        </label>
        <label>
          Runtime and context allowance per device (GiB)
          <input type="number" min="0" step="0.1" value={overhead} onChange={(e) => setOverhead(e.target.value)} />
        </label>
        <label>
          Custom model weight file size (GiB)
          <input type="number" min="0" step="any" value={customWeights} onChange={(e) => {
            setCustomWeights(e.target.value);
            setFileMessage("");
          }} />
        </label>
        <label>
          Estimate from GGUF files
          <input type="file" accept=".gguf" multiple onChange={(e) => {
            const files = Array.from(e.currentTarget.files ?? []);
            e.currentTarget.value = "";
            if (!files.length) return;
            if (files.some((file) => !/\.gguf$/i.test(file.name) || file.size === 0)) {
              setFileMessage("Choose nonempty .gguf files for a single model.");
              return;
            }
            setCustomWeights(String(files.reduce((total, file) => total + file.size, 0) / 2 ** 30));
            setFileMessage(`Estimated from ${files.length} GGUF ${files.length === 1 ? "file" : "files"}.`);
          }} />
        </label>
        {fileMessage && <p className="muted small" role="status">{fileMessage}</p>}
      </div>
      <p className="muted">Combined capacity for model weights after runtime allowance: {gib(totalCapacity)}.</p>
      <ul>
        {capacities.map(({ name, capacity }, index) => (
          <li key={index}>{name}: {gib(capacity)} for weights</li>
        ))}
      </ul>
      <table className="usage-table">
        <thead><tr><th scope="col">Model</th><th scope="col">Weights</th><th scope="col">Memory estimate</th></tr></thead>
        <tbody>
          {models.map(({ name, weights, fit }) => (
            <tr key={name}><th scope="row">{name}</th><td>{gib(weights)}</td><td>{fit}</td></tr>
          ))}
        </tbody>
      </table>
      <button type="button" className="btn ghost small-btn" onClick={() => void copyEstimate()}>Copy memory estimate</button>
      {copyStatus && <span className="muted small" role="status"> {copyStatus}</span>}
      <p className="muted">Weights use parameter count × precision, with 15% packed-weight overhead at 4 or 8 bits. Increase the runtime allowance for longer contexts or larger batches. A split estimate requires a runner that supports the model and can partition it across those devices. Check available disk space, supported GPU memory, and wired-network throughput before testing.</p>
    </details>
  );
}
