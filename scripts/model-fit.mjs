import { freemem } from "node:os";
import { parseArgs } from "node:util";
import { gib, modelFit } from "../src/model-fit.mjs";

function memory(value, label) {
  const parsed = Number(value);
  if (!value?.trim() || !Number.isFinite(parsed) || parsed < 0) throw new Error(`${label} must be a nonnegative number in GiB`);
  return parsed;
}

try {
  const { values } = parseArgs({
    options: {
      help: { type: "boolean" },
      host: { type: "string" },
      device: { type: "string", multiple: true, default: [] },
      bits: { type: "string", default: "4" },
      overhead: { type: "string", default: "2" },
      weights: { type: "string" },
    },
  });
  if (values.help) {
    console.log('Usage: node scripts/model-fit.mjs [--host GiB] [--device "Name=GiB"] [--bits 4|8|16] [--overhead GiB] [--weights GiB]');
    console.log("Device memory is usable RAM before inference overhead. Repeat --device for additional computers.");
  } else {
    const bits = Number(values.bits);
    if (![4, 8, 16].includes(bits)) throw new Error("--bits must be 4, 8, or 16");
    const devices = [{
      name: "This computer",
      memory: values.host === undefined ? freemem() / 2 ** 30 : memory(values.host, "--host"),
    }];
    for (const entry of values.device) {
      const [name, size, extra] = entry.split("=");
      if (!name.trim() || extra !== undefined) throw new Error('--device must be "Name=GiB"');
      devices.push({ name: name.trim(), memory: memory(size, name) });
    }
    const overhead = memory(values.overhead, "--overhead");
    const custom = values.weights === undefined ? 0 : memory(values.weights, "--weights");
    console.log(`${bits}-bit weights; ${gib(overhead)} runtime allowance per device`);
    for (const { name, memory: available } of devices) console.log(`${name}: ${gib(available)} usable`);
    for (const { name, weights, fit } of modelFit(devices, bits, overhead, custom)) console.log(`${name}: ${gib(weights)} weights — ${fit}`);
    console.log("Memory estimates only. Confirm model and split-runner compatibility before testing.");
  }
} catch (error) {
  console.error(error.message);
  process.exitCode = 1;
}
