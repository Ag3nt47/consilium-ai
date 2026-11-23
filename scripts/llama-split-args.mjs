import { readFileSync } from "node:fs";
import { parseArgs } from "node:util";

function readRunnerMemory(path) {
  const memory = new Map();
  const lines = readFileSync(path, "utf8").split(/\r?\n/);
  const header = lines.findIndex((line) => line.trim() === "Available devices:");
  if (header < 0) throw new Error("Expected saved llama.cpp --list-devices output.");
  for (const line of lines.slice(header + 1)) {
    const match = /^\s*(\S+):\s+.+\(\d+ MiB,\s*(\d+) MiB free\)\s*$/.exec(line);
    if (!match) continue;
    const [, name, free] = match;
    if (memory.has(name)) throw new Error(`Duplicate runner device in inventory: ${name}`);
    memory.set(name, Number(free));
  }
  if (!memory.size) throw new Error("No free-memory readings were found in the runner inventory.");
  return memory;
}

try {
  const { values } = parseArgs({ options: {
    help: { type: "boolean" },
    model: { type: "string" },
    device: { type: "string", multiple: true, default: [] },
    "runner-inventory": { type: "string" },
  } });
  if (values.help) {
    console.log('Pipe model-fit.mjs --json into this script. Pass --model NAME and one --device "Computer=RunnerDevice" for each allocated computer.');
    console.log("Optionally pass --runner-inventory PATH to check weights against saved llama.cpp --list-devices memory readings.");
  } else {
    if (!values.model) throw new Error("Select an exact model name with --model.");
    const report = JSON.parse(readFileSync(0, "utf8"));
    const model = report.models?.find((row) => row.name === values.model);
    const allocation = model?.allocation;
    if (!Array.isArray(allocation) || allocation.length < 2) throw new Error("The selected model needs an allocation across at least two devices.");
    const amounts = new Map(allocation.map(({ name, allocated }) => [name, allocated]));
    const total = [...amounts.values()].reduce((sum, amount) => sum + amount, 0);
    if (amounts.size !== allocation.length || !Number.isFinite(model.weights) || model.weights <= 0 ||
      [...amounts.values()].some((amount) => !Number.isFinite(amount) || amount <= 0) ||
      Math.abs(total / model.weights - 1) > 1e-9) {
      throw new Error("The selected model has an invalid allocation.");
    }
    const inventory = values["runner-inventory"] ? readRunnerMemory(values["runner-inventory"]) : null;
    const assigned = new Set();
    const bindings = values.device.map((binding) => {
      const [computer, device, extra] = binding.split("=");
      if (extra !== undefined || !amounts.has(computer) || !device || device !== device.trim() || /[,\x00-\x1f\x7f]/.test(device) || assigned.has(device)) {
        throw new Error('Map each allocated computer to a unique llama.cpp device with --device "Computer=RunnerDevice".');
      }
      assigned.add(device);
      const allocated = amounts.get(computer);
      if (inventory) {
        const freeMiB = inventory.get(device);
        if (freeMiB === undefined) throw new Error(`Runner device ${device} was missing from the inventory.`);
        const weightMiB = Math.ceil(allocated * 1024);
        if (weightMiB > freeMiB) {
          throw new Error(`Runner device ${device} has ${freeMiB} MiB free. Its planned weights alone need ${weightMiB} MiB.`);
        }
      }
      amounts.delete(computer);
      return { device, share: allocated / model.weights };
    });
    if (amounts.size) throw new Error("Supply a llama.cpp device for every allocated computer.");
    console.log(JSON.stringify([
      "--split-mode", "layer",
      "--device", bindings.map(({ device }) => device).join(","),
      "--tensor-split", bindings.map(({ share }) => share).join(","),
    ]));
  }
} catch (error) {
  console.error(error.message);
  process.exitCode = 1;
}
