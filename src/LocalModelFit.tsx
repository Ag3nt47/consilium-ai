import { useState } from "react";

const MODELS = [
  { name: "Qwen3 0.6B", parameters: 0.6 },
  { name: "Qwen3 8B", parameters: 8 },
  { name: "Llama 3.1 8B", parameters: 8 },
  { name: "14B class", parameters: 14 },
  { name: "32B class", parameters: 32 },
  { name: "Llama 3.1 70B", parameters: 70 },
  { name: "Custom model", parameters: 0 },
];

type Device = { id: number; name: string; memory: string };
const gib = (value: number) => `${value.toFixed(1)} GiB`;
const positive = (value: string) => Math.max(0, Number(value) || 0);

export function LocalModelFit({ availableGiB }: { availableGiB?: number }) {
  const [devices, setDevices] = useState<Device[]>([]);
  const [nextId, setNextId] = useState(1);
  const [hostMemory, setHostMemory] = useState<string | null>(null);
  const [bits, setBits] = useState(4);
  const [overhead, setOverhead] = useState("2");
  const [customWeights, setCustomWeights] = useState("");
  const host = hostMemory ?? (availableGiB && availableGiB > 0 ? availableGiB.toFixed(1) : "");
  const runtime = positive(overhead);
  const hasMemory = [host, ...devices.map(({ memory }) => memory)].some((memory) => positive(memory) > 0);
  const budgets = [{ name: "This computer", memory: host }, ...devices]
    .map(({ name, memory }) => ({ name, capacity: Math.max(0, positive(memory) - runtime) }))
    .filter(({ capacity }) => Number.isFinite(capacity) && capacity > 0)
    .sort((a, b) => b.capacity - a.capacity);
  const combined = budgets.reduce((sum, { capacity }) => sum + capacity, 0);

  return (
    <details className="small">
      <summary>Check local model memory</summary>
      <p className="muted">Enter the usable memory on each device after leaving room for its OS and other apps. Edit this computer’s estimate, and enter the Mac or other devices manually. Count unified memory once.</p>
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
          <input type="number" min="0" step="0.1" value={customWeights} onChange={(e) => setCustomWeights(e.target.value)} />
        </label>
      </div>
      <table className="usage-table">
        <thead><tr><th scope="col">Model</th><th scope="col">Weights</th><th scope="col">Memory estimate</th></tr></thead>
        <tbody>
          {MODELS.filter(({ parameters }) => parameters || Number(customWeights) > 0).map(({ name, parameters }) => {
            const weights = parameters ? parameters * 1e9 * bits / 8 / 2 ** 30 * (bits < 16 ? 1.15 : 1) : Number(customWeights);
            let count = 0;
            let capacity = 0;
            for (const device of budgets) {
              capacity += device.capacity;
              count++;
              if (capacity >= weights) break;
            }
            const single = budgets.filter((device) => device.capacity >= weights);
            const fit = single.length
              ? `One device: ${single.map((device) => device.name || "Unnamed device").join(", ")}`
              : combined >= weights
                ? `Split estimate: ${budgets.slice(0, count).map((device) => device.name || "Unnamed device").join(" + ")}`
                : hasMemory ? `Needs at least ${gib(weights - combined)} more for weights` : "Enter usable memory";
            return <tr key={name}><th scope="row">{name}</th><td>{gib(weights)}</td><td>{fit}</td></tr>;
          })}
        </tbody>
      </table>
      <p className="muted">Weights use parameter count × precision, with 15% packed-weight overhead at 4 or 8 bits. Increase the runtime allowance for longer contexts or larger batches. A split estimate requires a runner that supports the model and can partition it across those devices. Check available disk space, supported GPU memory, and wired-network throughput before testing.</p>
    </details>
  );
}
