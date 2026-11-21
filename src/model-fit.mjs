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
    let capacity = 0;
    for (const device of budgets) {
      selected.push(device.name);
      capacity += device.capacity;
      if (capacity >= weights) break;
    }
    return { name, weights, fit: `Split estimate: ${selected.join(" + ")}` };
  });
}
