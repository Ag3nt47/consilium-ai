import { useState } from "react";
import { api, type OllamaModel } from "./api";

import { effectiveReserve, equalSplitMemoryGiB, gib, ggufShardError, modelFit, weightBudgets } from "./model-fit.mjs";

type Device = { id: number | string; name: string; memory: string; runners?: string[]; platform?: string };
const memoryInput = (gib: number) => (Math.floor(Math.max(0, gib) * 10) / 10).toFixed(1);
const runnerNames = new Set(["ollama", "llama-cli", "llama-server", "ggml-rpc-server", "rpc-server"]);
const deviceName = (name: string) => name.trim().toLowerCase();

export function LocalModelFit({ availableGiB, ollamaModels = [] }: {
  availableGiB?: number;
  ollamaModels?: OllamaModel[];
}) {
  const [devices, setDevices] = useState<Device[]>([]);
  const [nextId, setNextId] = useState(1);
  const [hostRunners, setHostRunners] = useState<string[] | null>(null);
  const [scannedHostMemory, setScannedHostMemory] = useState<string | null>(null);
  const [hostMemory, setHostMemory] = useState<string | null>(null);
  const [bits, setBits] = useState(4);
  const [overhead, setOverhead] = useState("2");
  const [hostReserve, setHostReserve] = useState("");
  const [deviceReserves, setDeviceReserves] = useState(() => new Map<Device["id"], string>());
  const [preferSplit, setPreferSplit] = useState(false);
  const [distribution, setDistribution] = useState<"balanced" | "capacity">("balanced");
  const [customWeights, setCustomWeights] = useState("");
  const [customModelName, setCustomModelName] = useState("");
  const [fileMessage, setFileMessage] = useState("");
  const [scanning, setScanning] = useState(false);
  const [scanStatus, setScanStatus] = useState("");
  const [copyStatus, setCopyStatus] = useState("");
  const host = hostMemory ?? scannedHostMemory ?? (availableGiB && availableGiB > 0 ? memoryInput(availableGiB) : "");
  const hostName = "This computer";
  const usedNames = new Set([deviceName(hostName)]);
  const included = new Set<Device>();
  // Prefer fresh scans over manual entries when both refer to the same named computer.
  for (const device of [...devices.filter((entry) => typeof entry.id === "string"), ...devices.filter((entry) => typeof entry.id === "number")]) {
    const name = deviceName(device.name);
    if (!name || usedNames.has(name)) continue;
    usedNames.add(name);
    included.add(device);
  }
  const budgets = [{ name: hostName, memory: host, reserve: hostReserve }, ...devices.filter((device) => included.has(device))
    .map(({ id, name, memory }) => ({ name: name.trim(), memory, reserve: deviceReserves.get(id) ?? "" }))];
  const models = modelFit(budgets, bits, overhead, customWeights, preferSplit, distribution).map((model) =>
    customModelName && model.name === "Custom model" ? { ...model, name: customModelName } : model);
  const splits = models.flatMap(({ name, weights, allocation }) => allocation && allocation.length >= 2
    ? [{ name, weights, allocation }] : []);
  const exportableSplits = splits.filter(({ allocation }) => allocation.every(({ name }) => !name.includes("=")));
  const capacities = weightBudgets(budgets, overhead);
  const reserves = budgets.map(({ reserve }) => effectiveReserve(reserve, overhead));
  const totalCapacity = capacities.reduce((total, device) => total + device.capacity, 0);

  const showEqualTarget = preferSplit && budgets.length > 1;
  const equalTarget = (weights: number) => {
    const targets = reserves.map((reserve) => equalSplitMemoryGiB(weights, budgets.length, reserve));
    const shortfalls = budgets.flatMap(({ name, memory }, index) => {
      const available = Number(memory);
      const target = targets[index];
      if (memory === "" || !Number.isFinite(available) || available < 0) {
        return [`${name || "Unnamed device"}: enter usable memory`];
      }
      return available < target
        ? [`${name || "Unnamed device"} needs ${gib(Math.ceil((target - available) * 10) / 10)} more`]
        : [];
    });
    const rounded = (target: number) => gib(Math.ceil(target * 10) / 10);
    const uniform = targets.every((target) => target === targets[0]);
    const required = uniform ? [rounded(targets[0])] : budgets.map(({ name }, index) => `${name}: ${rounded(targets[index])}`);
    return [...required, ...shortfalls].join(" · ");
  };

  async function copyReport(report: string, label: string) {
    try {
      await navigator.clipboard.writeText(report);
      setCopyStatus(`${label} copied.`);
    } catch {
      setCopyStatus(`Could not copy the ${label.toLowerCase()}.`);
    }
  }

  function copyEstimate() {
    const report = [
      "Consilium local model memory estimate",
      `Catalog precision: ${bits}-bit. Default runtime allowance: ${gib(effectiveReserve(undefined, overhead))} per device.`,
      `Combined weight capacity: ${gib(totalCapacity)}.`,
      ...budgets.map(({ name, memory }, index) => `${name || "Unnamed device"}: ${memory ? gib(Number(memory)) : "memory not entered"} usable, ${gib(reserves[index])} reserved, ${gib(capacities[index].capacity)} for weights.`),
      "",
      ...models.map(({ name, weights, fit }) => {
        const target = showEqualTarget ? ` Equal split across ${budgets.length} devices: ${equalTarget(weights)}.` : "";
        return `${name}: ${gib(weights)} weights. ${fit}.${target}`;
      }),
      "",
      "Memory estimate only. Verify runner compatibility and inference performance separately.",
    ].join("\n");
    return copyReport(report, "Memory estimate");
  }

  async function scanPeers() {
    setScanning(true);
    setScanStatus("");
    setHostRunners(null);
    const localPressure = api.systemPressure().then((pressure) => {
      const availableMiB = pressure.available_memory_mb ?? pressure.total_memory_mb * (1 - pressure.memory_percent / 100);
      setScannedHostMemory(memoryInput(Math.min(pressure.total_memory_mb, availableMiB) / 1024));
    }).catch(() => setScannedHostMemory(null));
    const localInventory = api.localToolInventory().catch(() => null);
    try {
      const [before, link] = await Promise.all([api.peerResources(), api.linkStatus()]);
      const connected = new Map(link.peers.filter((peer) => peer.connected).map((peer) => [peer.node_id, peer.name]));
      const expected = connected.size;
      if (!expected) {
        setDevices((previous) => previous.filter((device) => typeof device.id === "number"));
        setScanStatus("No paired computers are connected. Connect a computer or enter memory manually.");
        return;
      }
      const requestId = await api.requestPeerResources();
      const deadline = performance.now() + 6000;
      const fresh = new Map<string, Awaited<ReturnType<typeof api.peerResources>>[string]>();
      do {
        const reports = await api.peerResources();
        for (const [id, report] of Object.entries(reports)) {
          if (!connected.has(id) || (report.total_memory_mb ?? 0) <= 0) continue;
          const requested = report.supports_request_id
            ? report.request_id === requestId
            : report.updated_at > (before[id]?.updated_at ?? 0);
          if (requested) fresh.set(id, report);
        }
        if (fresh.size >= expected) break;
        const remaining = deadline - performance.now();
        if (remaining <= 0) break;
        await new Promise((resolve) => setTimeout(resolve, Math.min(500, remaining)));
      } while (true);
      const found = Array.from(fresh, ([id, report]) => {
        const name = connected.get(id) ?? id;
        const totalMiB = report.total_memory_mb ?? 0;
        const availableMiB = report.available_memory_mb ?? totalMiB * (1 - report.memory_percent / 100);
        const memory = Math.max(0, Math.min(totalMiB, availableMiB)) / 1024;
        const runners = report.tools?.filter((tool) => runnerNames.has(tool)) ?? [];
        const platform = [report.os, report.build_arch].filter(Boolean).join(" ");
        return { id, name, memory: memoryInput(memory), runners, platform };
      });
      setDevices((previous) => [
        ...previous.filter((device) => typeof device.id === "number"),
        ...found,
      ]);
      setScanStatus(found.length
        ? `Imported ${found.length} of ${expected} requested paired-computer memory reports.`
        : "No new memory reports arrived. Previous paired estimates were cleared. Check that the computers are online and updated, or enter memory manually.");
    } catch {
      setDevices((previous) => previous.filter((device) => typeof device.id === "number"));
      setScanStatus("Peer capability scan unavailable. Previous paired estimates were cleared. Enter device memory manually.");
    } finally {
      const [localTools] = await Promise.all([localInventory, localPressure]);
      setHostRunners(localTools?.filter((tool) => runnerNames.has(tool)) ?? null);
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
        {hostRunners && (
          <span className="muted">Model tools on this computer: {hostRunners.join(", ") || "none detected"}.</span>
        )}
        {devices.map((device) => (
          <div className="row wrap" key={device.id}>
            {device.platform && <span className="muted">Consilium build: {device.platform}</span>}
            <label>
              Device {typeof device.id === "number" ? device.id : device.name} name
              <input value={device.name} onChange={(e) => {
                const name = e.target.value;
                setDevices((previous) => previous.map((item) => item.id === device.id ? { ...item, name } : item));
              }} />
            </label>
            <label>
              Device {typeof device.id === "number" ? device.id : device.name} usable memory (GiB)
              <input type="number" min="0" step="0.1" value={device.memory} onChange={(e) => {
                const memory = e.target.value;
                setDevices((previous) => previous.map((item) => item.id === device.id ? { ...item, memory } : item));
              }} />
            </label>
            <button type="button" className="btn ghost small-btn" onClick={() => setDevices((previous) => previous.filter((item) => item.id !== device.id))}>Remove device {typeof device.id === "number" ? device.id : device.name}</button>
            {device.runners && <span className="muted">Model tools detected: {device.runners.join(", ") || "none reported"}.</span>}
            {!included.has(device) && <span className="muted">Enter a unique device name to include this device.</span>}
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
          Default runtime and context allowance per device (GiB)
          <input type="number" min="0" step="0.1" value={overhead} onChange={(e) => setOverhead(e.target.value)} />
        </label>
        <details className="small">
          <summary>Set runtime allowances by device</summary>
          <p className="muted">Leave a field empty to use the default. Use a device-specific allowance when its runner or context needs more memory.</p>
          <label>
            This computer runtime allowance (GiB)
            <input type="number" min="0" step="0.1" value={hostReserve} placeholder={overhead}
              onChange={(e) => setHostReserve(e.target.value)} />
          </label>
          {devices.filter((device) => included.has(device)).map(({ id, name }) => (
            <label key={id}>
              {name} runtime allowance (GiB)
              <input type="number" min="0" step="0.1" value={deviceReserves.get(id) ?? ""} placeholder={overhead}
                onChange={(e) => {
                  const reserve = e.target.value;
                  setDeviceReserves((previous) => new Map(previous).set(id, reserve));
                }} />
            </label>
          ))}
        </details>
        <label className="row">
          <input type="checkbox" checked={preferSplit} onChange={(e) => setPreferSplit(e.target.checked)} />
          Estimate a split even when one device can hold the model
        </label>
        <label>
          How to divide weights across devices
          <select value={distribution} onChange={(e) => setDistribution(e.target.value === "capacity" ? "capacity" : "balanced")}>
            <option value="balanced">As evenly as memory allows</option>
            <option value="capacity">More on devices with more free memory</option>
          </select>
        </label>
        {ollamaModels.some(({ size }) => Number.isFinite(size) && size > 0) && (
          <label>
            Use an installed Ollama model size
            <select value="" onChange={(event) => {
              const model = ollamaModels.find(({ name }) => name === event.target.value);
              if (!model) return;
              setCustomWeights(String(model.size / 2 ** 30));
              setCustomModelName(model.name);
              setFileMessage(`Estimated from the installed size of ${model.name}.`);
            }}>
              <option value="">Choose a model</option>
              {ollamaModels.filter(({ size }) => Number.isFinite(size) && size > 0).map(({ name, size }) => (
                <option key={name} value={name}>{name} ({gib(size / 2 ** 30)})</option>
              ))}
            </select>
          </label>
        )}
        <label>
          Custom model weight file size (GiB)
          <input type="number" min="0" step="any" value={customWeights} onChange={(e) => {
            setCustomWeights(e.target.value);
            setCustomModelName("");
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
            const shardError = ggufShardError(files.map((file) => file.name));
            if (shardError) {
              setFileMessage(shardError);
              return;
            }
            setCustomWeights(String(files.reduce((total, file) => total + file.size, 0) / 2 ** 30));
            setCustomModelName("");
            setFileMessage(`Estimated from ${files.length} GGUF ${files.length === 1 ? "file" : "files"}.`);
          }} />
        </label>
        {fileMessage && <p className="muted small" role="status">{fileMessage}</p>}
      </div>
      <p className="muted">Combined capacity for model weights after runtime allowance: {gib(totalCapacity)}.</p>
      <ul>
        {capacities.map(({ name, capacity }, index) => (
          <li key={index}>{name}: {gib(capacity)} for weights · {gib(reserves[index])} reserved for runtime</li>
        ))}
      </ul>
      <table className="usage-table">
        <thead><tr>
          <th scope="col">Model</th><th scope="col">Weights</th><th scope="col">Memory estimate</th>
          {showEqualTarget && <th scope="col">Usable memory per device for an equal split</th>}
        </tr></thead>
        <tbody>
          {models.map(({ name, weights, fit }) => (
            <tr key={name}>
              <th scope="row">{name}</th><td>{gib(weights)}</td><td>{fit}</td>
              {showEqualTarget && <td>{equalTarget(weights)}</td>}
            </tr>
          ))}
        </tbody>
      </table>
      <button type="button" className="btn ghost small-btn" onClick={() => void copyEstimate()}>Copy memory estimate</button>
      {splits.length > 0 && (
        <button type="button" className="btn ghost small-btn" disabled={!exportableSplits.length}
          onClick={() => void copyReport(JSON.stringify({ models: exportableSplits }, null, 2), "Split plan JSON")}>
          Copy split plan JSON
        </button>
      )}
      {copyStatus && <span className="muted small" role="status"> {copyStatus}</span>}
      {exportableSplits.length < splits.length && <p className="muted small">Use device names without an equals sign to export those splits.</p>}
      {exportableSplits.length > 0 && <p className="muted small">Save the JSON and pass it to <code>scripts/llama-split-args.mjs</code> with the exact model name and your runner's device names.</p>}
      <p className="muted">Weights use parameter count × precision, with 15% packed-weight overhead at 4 or 8 bits. Increase the runtime allowance for longer contexts or larger batches. A split estimate requires a runner that supports the model and can partition it across those devices. Check available disk space, supported GPU memory, and wired-network throughput before testing.</p>
    </details>
  );
}
