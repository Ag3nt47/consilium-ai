import { readFileSync } from "node:fs";
import { parseArgs } from "node:util";

try {
  const { values } = parseArgs({ options: {
    help: { type: "boolean" },
    model: { type: "string" },
    device: { type: "string", multiple: true, default: [] },
  } });
  if (values.help) {
    console.log('Pipe model-fit.mjs --json into this script. Pass --model NAME and one --device "Computer=RunnerDevice" for each allocated computer.');
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
    const assigned = new Set();
    const bindings = values.device.map((binding) => {
      const [computer, device, extra] = binding.split("=");
      if (extra !== undefined || !amounts.has(computer) || !device || device !== device.trim() || /[,\x00-\x1f\x7f]/.test(device) || assigned.has(device)) {
        throw new Error('Map each allocated computer to a unique llama.cpp device with --device "Computer=RunnerDevice".');
      }
      assigned.add(device);
      const share = amounts.get(computer) / model.weights;
      amounts.delete(computer);
      return { device, share };
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
