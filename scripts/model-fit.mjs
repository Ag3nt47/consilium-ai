import { readFileSync, realpathSync, statSync } from "node:fs";
import { freemem, homedir } from "node:os";
import { basename, join } from "node:path";
import { parseArgs } from "node:util";
import { equalSplitMemoryGiB, gib, ggufShardError, modelFit, weightBudgets } from "../src/model-fit.mjs";

const OLLAMA_MODEL_LAYER = "application/vnd.ollama.image.model";
const OLLAMA_WEIGHT_LAYERS = new Set([
  OLLAMA_MODEL_LAYER, "application/vnd.ollama.image.adapter", "application/vnd.ollama.image.projector",
]);

function memory(value, label) {
  const parsed = Number(value);
  if (!value?.trim() || !Number.isFinite(parsed) || parsed < 0) throw new Error(`${label} must be a nonnegative number in GiB`);
  return parsed;
}

function installedModel(reference) {
  const match = /^(?:([\w][\w.-]*)\/)?([\w][\w.-]*)(?::([\w][\w.-]*))?$/.exec(reference);
  if (!match) throw new Error("--ollama expects [namespace/]name[:tag].");
  const [, namespace = "library", name, tag = "latest"] = match;
  const root = process.env.OLLAMA_MODELS || join(homedir(), ".ollama", "models");
  const manifestPath = join(root, "manifests", "registry.ollama.ai", namespace, name, tag);
  const manifestStat = statSync(manifestPath, { throwIfNoEntry: false });
  if (!manifestStat?.isFile() || manifestStat.size > 1024 * 1024) {
    throw new Error("The local Ollama model manifest is missing or too large.");
  }
  const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
  if (!Array.isArray(manifest?.layers)) throw new Error("The local Ollama model manifest has no layers.");
  if (!manifest.layers.some((layer) => layer?.mediaType === OLLAMA_MODEL_LAYER)) {
    throw new Error("The Ollama manifest has no model weight layer.");
  }
  const layers = new Map();
  for (const layer of manifest.layers) {
    if (!OLLAMA_WEIGHT_LAYERS.has(layer?.mediaType)) continue;
    if (!/^sha256:[a-f0-9]{64}$/.test(layer.digest) || !Number.isSafeInteger(layer.size) || layer.size <= 0) {
      throw new Error("The Ollama manifest has invalid weight layer metadata.");
    }
    const blob = statSync(join(root, "blobs", layer.digest.replace(":", "-")), { throwIfNoEntry: false });
    if (!blob?.isFile() || blob.size !== layer.size) throw new Error("An Ollama weight blob is missing or has the wrong size.");
    layers.set(layer.digest, blob.size);
  }
  const modelName = namespace === "library" ? name : `${namespace}/${name}`;
  return { name: `${modelName}:${tag}`, bytes: [...layers.values()].reduce((sum, size) => sum + size, 0) };
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
      ollama: { type: "string" },
    },
  });
  if (values.help) {
    console.log('Usage: node scripts/model-fit.mjs [--json] [--split] [--host GiB] [--device "Name=GiB"] [--bits 4|8|16] [--overhead GiB] [--weights GiB | --gguf PATH ... | --ollama MODEL]');
    console.log("--split estimates a balanced allocation even when one device can hold the model.");
    console.log("Device memory is usable RAM before inference overhead. Repeat --device with a unique name for each additional computer.");
    console.log("For --gguf, select one model file or all numbered shards of one model.");
    console.log("For --ollama, use an installed [namespace/]name[:tag]. Reads local files without launching a runner.");
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
    if (values.ollama !== undefined && (values.weights !== undefined || values.gguf.length)) {
      throw new Error("Choose one model size source: --ollama, --weights, or --gguf.");
    }
    const installed = values.ollama === undefined ? null : installedModel(values.ollama);
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
    const custom = values.weights === undefined ? (installed?.bytes ?? modelBytes) / 2 ** 30 : memory(values.weights, "--weights");
    const capacities = weightBudgets(devices, overhead);
    const models = modelFit(devices, bits, overhead, custom, values.split).map((row) => {
      if (installed && row.name === "Custom model") row.name = installed.name;
      if (!values.split || devices.length < 2) return row;
      const target = equalSplitMemoryGiB(row.weights, devices.length, overhead);
      const equalSplitShortfalls = devices
        .filter(({ memory }) => memory < target)
        .map(({ name, memory }) => ({ name, missingGiB: target - memory }));
      return { ...row, equalSplitMemoryGiB: target, equalSplitShortfalls };
    });
    if (values.json) {
      const ollama = installed ? { ollamaModel: installed.name, ollamaBytes: installed.bytes } : {};
      console.log(JSON.stringify({ bits, runtimeReserveGiB: overhead, ggufFileCount: files.size, ggufBytes: modelBytes, ...ollama, capacities, models }));
    } else {
      if (files.size) console.log(`GGUF on-disk size: ${gib(custom)} across ${files.size} file(s)`);
      if (installed) console.log(`Installed Ollama weights for ${installed.name}: ${gib(custom)}`);
      console.log(`${bits}-bit catalog estimates; ${gib(overhead)} runtime allowance per device`);
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
