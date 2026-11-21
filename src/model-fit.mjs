const MODELS = [
  ["Qwen3 0.6B", 0.6],
  ["Qwen3 8B", 8],
  ["Llama 3.1 8B", 8],
  ["14B class", 14],
  ["32B class", 32],
  ["Llama 3.1 70B", 70],
];

const positive = (value) => Number.isFinite(Number(value)) ? Math.max(0, Number(value)) : 0;
export const gib = (value) => `${value.toFixed(1)} GiB`;

export function weightBudgets(devices, overhead) {
  const runtime = positive(overhead);
  return devices.map(({ name, memory }) => ({
    name: name || "Unnamed device",
    capacity: Math.max(0, positive(memory) - runtime),
  }));
}

export function modelFit(devices, bits, overhead, customWeights) {
  const hasMemory = devices.some(({ memory }) => positive(memory) > 0);
  const budgets = weightBudgets(devices, overhead).filter(({ capacity }) => capacity > 0).sort((a, b) => b.capacity - a.capacity);
  const combined = budgets.reduce((sum, { capacity }) => sum + capacity, 0);
  const models = MODELS.map(([name, parameters]) => ({
    name,
    weights: parameters * 1e9 * bits / 8 / 2 ** 30 * (bits < 16 ? 1.15 : 1),
  }));
  if (positive(customWeights) > 0) models.push({ name: "Custom model", weights: positive(customWeights) });

  return models.map(({ name, weights }) => {
    const single = budgets.filter(({ capacity }) => capacity >= weights);
    if (single.length) return { name, weights, fit: `One device: ${single.map((device) => device.name).join(", ")}` };
    if (combined < weights) return {
      name, weights, fit: hasMemory ? `Needs at least ${gib(weights - combined)} more for weights` : "Enter usable memory",
    };
    const selected = [];
    let remaining = weights;
    for (const device of budgets) {
      const allocated = Math.min(device.capacity, remaining);
      selected.push({ name: device.name, allocated });
      remaining -= allocated;
      if (remaining <= 0) break;
    }
    const names = selected.map((device) => device.name).join(" + ");
    const allocation = selected.map(({ name, allocated }) =>
      `${name} ${allocated < 0.05 ? "<0.1 GiB" : gib(allocated)}`
    ).join(", ");
    return { name, weights, fit: `Split estimate: ${names} (${allocation})` };
  });
}

export function ggufShardError(names) {
  const shards = names.map((name) => /^(.*)-(\d{5})-of-(\d{5})\.gguf$/i.exec(name));
  const first = shards.find(Boolean);
  if (!first) return undefined;
  const [, model, , countText] = first;
  const count = Number(countText);
  if (shards.some((shard) => !shard || shard[1] !== model || shard[3] !== countText)) {
    return "Select GGUF shards from one model and one shard set.";
  }
  const indices = new Set(shards.map((shard) => Number(shard[2])));
  if (indices.size !== count || shards.length !== count || [...indices].some((index) => index < 1 || index > count)) {
    return `Select all ${count} GGUF shards exactly once.`;
  }
  return undefined;
}
