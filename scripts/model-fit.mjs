import { realpathSync, statSync } from "node:fs";
import { freemem } from "node:os";
import { basename } from "node:path";
import { parseArgs } from "node:util";
import { equalSplitMemoryGiB, gib, ggufShardError, modelFit, weightBudgets } from "../src/model-fit.mjs";

function memory(value, label) {
  const parsed = Number(value);
  if (!value?.trim() || !Number.isFinite(parsed) || parsed < 0) throw new Error(`${label} must be a nonnegative number in GiB`);
  return parsed;
}

try {
  const { values } = parseArgs({
    options: {
      help: { type: "boolean" },
      json: { type: "boolean" },
      split: { type: "boolean" },
      host: { type: "string" },
      device: { type: "string", multiple: true, default: [] },
      bits: { type: "string", default: "4" },
      overhead: { type: "string", default: "2" },
      weights: { type: "string" },
      gguf: { type: "string", multiple: true, default: [] },
    },
  });
  if (values.help) {
    console.log('Usage: node scripts/model-fit.mjs [--json] [--split] [--host GiB] [--device "Name=GiB"] [--bits 4|8|16] [--overhead GiB] [--weights GiB | --gguf PATH ...]');
    console.log("--split estimates a balanced allocation even when one device can hold the model.");
    console.log("Device memory is usable RAM before inference overhead. Repeat --device with a unique name for each additional computer.");
  } else {
    const bits = Number(values.bits);
    if (![4, 8, 16].includes(bits)) throw new Error("--bits must be 4, 8, or 16");
    const devices = [{
      name: "This computer",
      memory: values.host === undefined ? freemem() / 2 ** 30 : memory(values.host, "--host"),
    }];
    const deviceNames = new Set([devices[0].name.toLowerCase()]);
    for (const entry of values.device) {
      const [label, size, extra] = entry.split("=");
      const name = label.trim();
      if (!name || extra !== undefined) throw new Error('--device must be "Name=GiB"');
      const key = name.toLowerCase();
      if (deviceNames.has(key)) throw new Error("Use a unique name for each device.");
      deviceNames.add(key);
      devices.push({ name, memory: memory(size, name) });
    }
    const overhead = memory(values.overhead, "--overhead");
    if (values.weights !== undefined && values.gguf.length) throw new Error("Choose either --weights or --gguf.");
    const files = new Set();
    let modelBytes = 0;
    for (const entry of values.gguf) {
      const path = realpathSync(entry);
      const stat = statSync(path);
      if (!/\.gguf$/i.test(path) || !stat.isFile() || stat.size === 0) throw new Error("--gguf requires a nonempty GGUF file.");
      if (files.has(path)) throw new Error("List each GGUF shard only once.");
      files.add(path);
      modelBytes += stat.size;
    }
    const shardError = ggufShardError([...files].map((file) => basename(file)));
    if (shardError) throw new Error(shardError);
    const custom = values.weights === undefined ? modelBytes / 2 ** 30 : memory(values.weights, "--weights");
    const capacities = weightBudgets(devices, overhead);
    const models = modelFit(devices, bits, overhead, custom, values.split).map((row) => {
      if (!values.split || devices.length < 2) return row;
      const target = equalSplitMemoryGiB(row.weights, devices.length, overhead);
      const equalSplitShortfalls = devices
        .filter(({ memory }) => memory < target)
        .map(({ name, memory }) => ({ name, missingGiB: target - memory }));
      return { ...row, equalSplitMemoryGiB: target, equalSplitShortfalls };
    });
    if (values.json) {
      console.log(JSON.stringify({ bits, runtimeReserveGiB: overhead, ggufFileCount: files.size, ggufBytes: modelBytes, capacities, models }));
    } else {
      if (files.size) console.log(`GGUF on-disk size: ${gib(custom)} across ${files.size} file(s)`);
      console.log(`${bits}-bit weights; ${gib(overhead)} runtime allowance per device`);
      for (const [index, { name, memory: available }] of devices.entries()) {
        console.log(`${name}: ${gib(available)} usable, ${gib(capacities[index].capacity)} for weights`);
      }
      console.log(`Combined weight capacity: ${gib(capacities.reduce((sum, device) => sum + device.capacity, 0))}`);
      for (const { name, weights, fit, equalSplitMemoryGiB: target, equalSplitShortfalls = [] } of models) {
        const equal = target === undefined ? "" : ` · Equal split target: ${gib(Math.ceil(target * 10) / 10)} usable per device`;
        console.log(`${name}: ${gib(weights)} weights — ${fit}${equal}`);
        if (equalSplitShortfalls.length) {
          const shortfalls = equalSplitShortfalls.map(({ name, missingGiB }) =>
            `${name} needs ${gib(Math.ceil(missingGiB * 10) / 10)} more`
          );
          console.log(`  Equal split memory shortfall: ${shortfalls.join(", ")}`);
        }
      }
      console.log("Memory estimates only. Confirm model and split-runner compatibility before testing.");
    }
  }
} catch (error) {
  console.error(error.message);
  process.exitCode = 1;
}
