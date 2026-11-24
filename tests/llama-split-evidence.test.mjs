import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

const local = "CUDA0";
const remote = "RPC0[127.0.0.1:50052]";
const start = "load_tensors: loading model tensors, this can take a while... (load_mode = mmap)";
const localWeights = "load_tensors: CUDA0 model buffer size = 6144.00 MiB";
const remoteWeights = `load_tensors: ${remote} model buffer size = 2048.00 MiB`;
const prompt = "llama_perf_context_print: prompt eval time = 40.00 ms / 16 tokens (2.50 ms per token, 400.00 tokens per second)";
const generated = "llama_perf_context_print:        eval time = 250.00 ms / 8 runs   (31.25 ms per token, 32.00 tokens per second)";

function reader(t) {
  const dir = mkdtempSync(join(tmpdir(), "consilium-llama-evidence-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const log = join(dir, "trial.log");
  return (content, encoding, expected = [local, remote]) => {
    writeFileSync(log, content, encoding);
    return spawnSync(process.execPath, ["scripts/llama-split-evidence.mjs", "--log", log,
      ...expected.flatMap((name) => ["--expect-buffer", name])], { encoding: "utf8" });
  };
}

test("summarizes split buffers and generation from a saved Windows trial log", (t) => {
  const run = reader(t);
  const text = [
    start, localWeights, remoteWeights,
    "load_tensors: CUDA0 model buffer size = 12.50 MiB",
    `llama_kv_cache: ${remote} KV buffer size = 64.25 MiB`,
    "llama_perf_context_print:        load time = 7501.00 ms",
    prompt, generated, "ordinary request text to omit",
  ].join("\r\n");
  const result = run("\uFEFF" + text, "utf16le");
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(JSON.parse(result.stdout), {
    modelLoads: 1,
    buffers: [{ buffer: local, weightMiB: 6156.5, kvMiB: 0 }, { buffer: remote, weightMiB: 2048, kvMiB: 64.25 }],
    missingWeightBuffers: [],
    loadMs: 7501,
    generation: { elapsedMs: 250, count: 8, tokensPerSecond: 32 },
  });
  assert.doesNotMatch(result.stdout, /ordinary request text/);
});

test("requires weights after the last load marker and keeps generation evidence separate", (t) => {
  const run = reader(t);
  const latest = run([
    start, localWeights, remoteWeights, generated, start, localWeights,
    `load_tensors: ${remote} model buffer size = 0.00 MiB`,
    `llama_kv_cache: ${remote} KV buffer size = 64.25 MiB`, prompt,
  ].join("\n"));
  assert.equal(latest.status, 1, latest.stderr);
  const evidence = JSON.parse(latest.stdout);
  assert.equal(evidence.modelLoads, 2);
  assert.deepEqual(evidence.missingWeightBuffers, [remote]);
  assert.equal(evidence.generation, null);

  const noInference = run([start, localWeights, remoteWeights, prompt].join("\n"));
  assert.equal(noInference.status, 1, noInference.stderr);
  assert.deepEqual(JSON.parse(noInference.stdout).missingWeightBuffers, []);
});

test("accepts a server decode sample and requires distinct expected buffer names", (t) => {
  const run = reader(t);
  const server = [
    start, localWeights, remoteWeights,
    "slot print_timing: id 0 | task 4 |        eval time = 500.00 ms / 12 tokens (41.67 ms per token, 24.00 tokens per second)",
  ].join("\n");
  const valid = run(server);
  assert.equal(valid.status, 0, valid.stderr);
  assert.equal(JSON.parse(valid.stdout).generation.tokensPerSecond, 24);
  const duplicate = run(server, "utf8", [local, local]);
  assert.equal(duplicate.status, 2);
  assert.match(duplicate.stderr, /distinct, exact/);
});
