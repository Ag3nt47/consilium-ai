import { readFileSync, statSync } from "node:fs";
import { parseArgs } from "node:util";

const maxLogBytes = 16 * 1024 * 1024;

function readLog(path) {
  const stat = statSync(path);
  if (!stat.isFile() || stat.size > maxLogBytes) throw new Error("Choose a saved trial log up to 16 MiB.");
  const bytes = readFileSync(path);
  if (bytes.length > maxLogBytes) throw new Error("Choose a saved trial log up to 16 MiB.");
  return bytes[0] === 0xff && bytes[1] === 0xfe
    ? bytes.subarray(2).toString("utf16le")
    : bytes.toString("utf8").replace(/^\uFEFF/, "");
}

function summarize(log, expected) {
  const buffers = new Map();
  let loads = 0;
  let loadMs = null;
  let generation = null;
  for (const raw of log.split(/[\r\n]+/)) {
    const line = raw.replace(/\x1b\[[0-?]*[ -/]*[@-~]/g, "").trim();
    if (/(?:^|\s)load_tensors:\s+loading model tensors[, ]/.test(line)) {
      loads++;
      buffers.clear();
      loadMs = null;
      generation = null;
      continue;
    }
    if (!loads) continue;
    const model = /(?:^|\s)load_tensors:\s+(.+?)\s+model buffer size =\s*([\d.]+) MiB$/.exec(line);
    const cache = /(?:^|\s)llama_kv_cache:\s+(.+?)\s+KV buffer size =\s*([\d.]+) MiB$/.exec(line);
    const sample = model ?? cache;
    if (sample) {
      const [, name, size] = sample;
      const value = Number(size);
      if (!Number.isFinite(value)) continue;
      const row = buffers.get(name) ?? { buffer: name, weightMiB: 0, kvMiB: 0 };
      if (model) row.weightMiB += value;
      else row.kvMiB += value;
      buffers.set(name, row);
      continue;
    }
    const load = /(?:^|\s)llama_perf_context_print:\s+load time =\s*([\d.]+) ms$/.exec(line);
    if (load) {
      const value = Number(load[1]);
      if (Number.isFinite(value)) loadMs = value;
    }
    const evalTime = /(?:llama_perf_context_print:|print_timings?:.*\|)\s+eval time =\s*([\d.]+) ms\s*\/\s*(\d+) (?:tokens|runs)\s*\(\s*[\d.]+ ms per token,\s*([\d.]+) tokens per second\)$/.exec(line);
    if (evalTime) {
      const [elapsedMs, count, tokensPerSecond] = evalTime.slice(1).map(Number);
      if ([elapsedMs, count, tokensPerSecond].every((value) => Number.isFinite(value) && value > 0)) {
        generation = { elapsedMs, count, tokensPerSecond };
      }
    }
  }
  const missingWeightBuffers = expected.filter((name) => !(buffers.get(name)?.weightMiB > 0));
  return { modelLoads: loads, buffers: [...buffers.values()], missingWeightBuffers, loadMs, generation };
}

try {
  const { values } = parseArgs({ options: {
    help: { type: "boolean" },
    log: { type: "string" },
    "expect-buffer": { type: "string", multiple: true, default: [] },
  } });
  if (values.help) {
    console.log('Usage: node scripts/llama-split-evidence.mjs --log trial.log --expect-buffer CUDA0 --expect-buffer "RPC0[127.0.0.1:50052]"');
    console.log("Summarizes weight, KV and generation telemetry from the last model load in a saved llama.cpp b12621 log.");
  } else {
    const expected = values["expect-buffer"];
    if (!values.log || expected.length < 2 || new Set(expected).size !== expected.length ||
      expected.some((name) => !name || name.trim() !== name)) {
      throw new Error("Pass --log and at least two distinct, exact --expect-buffer names.");
    }
    const report = summarize(readLog(values.log), expected);
    console.log(JSON.stringify(report, null, 2));
    if (report.missingWeightBuffers.length || !report.generation) process.exitCode = 1;
  }
} catch (error) {
  console.error(error.message);
  process.exitCode = 2;
}
