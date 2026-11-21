import { realpathSync, statSync } from "node:fs";
import { freemem } from "node:os";
import { basename } from "node:path";
import { parseArgs } from "node:util";
import { gib, ggufShardError, modelFit } from "../src/model-fit.mjs";

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
      gguf: { type: "string", multiple: true, default: [] },
    },
  });
  if (values.help) {
    console.log('Usage: node scripts/model-fit.mjs [--host GiB] [--device "Name=GiB"] [--bits 4|8|16] [--overhead GiB] [--weights GiB | --gguf PATH ...]');
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
    if (files.size) console.log(`GGUF on-disk size: ${gib(custom)} across ${files.size} file(s)`);
    console.log(`${bits}-bit weights; ${gib(overhead)} runtime allowance per device`);
    for (const { name, memory: available } of devices) console.log(`${name}: ${gib(available)} usable`);
    for (const { name, weights, fit } of modelFit(devices, bits, overhead, custom)) console.log(`${name}: ${gib(weights)} weights — ${fit}`);
    console.log("Memory estimates only. Confirm model and split-runner compatibility before testing.");
  }
} catch (error) {
  console.error(error.message);
  process.exitCode = 1;
}
