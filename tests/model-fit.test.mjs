import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { modelFit } from "../src/model-fit.mjs";

function customFit(devices, weights) {
  return modelFit(devices, 4, 2, weights).find(({ name }) => name === "Custom model").fit;
}

test("split estimate reserves runtime memory on each device", () => {
  const windows = { name: "Windows", memory: 10 };
  const mac = { name: "Mac", memory: 6 };
  assert.equal(customFit([windows], 10), "Needs at least 2.0 GiB more for weights");
  assert.equal(customFit([mac], 10), "Needs at least 6.0 GiB more for weights");
  assert.equal(customFit([mac, windows], 10), "Split estimate: Windows + Mac");
  assert.equal(customFit([mac, windows], 13), "Needs at least 1.0 GiB more for weights");
});

test("single-device fit takes precedence when one computer has enough memory", () => {
  assert.equal(customFit([
    { name: "Windows", memory: 12 },
    { name: "Mac", memory: 6 },
  ], 10), "One device: Windows");
});

test("devices whose free memory is consumed by runtime add no weight capacity", () => {
  assert.equal(customFit([
    { name: "Windows", memory: 10 },
    { name: "Mac", memory: 2 },
    { name: "Phone", memory: 1 },
  ], 10), "Needs at least 2.0 GiB more for weights");
});

test("the CLI estimates fit from the combined size of separate GGUF shards", (t) => {
  const folder = mkdtempSync(join(tmpdir(), "consilium-model-fit-"));
  t.after(() => rmSync(folder, { recursive: true, force: true }));
  const first = join(folder, "model-00001.gguf");
  const second = join(folder, "model-00002.gguf");
  writeFileSync(first, Buffer.alloc(1024 * 1024));
  writeFileSync(second, Buffer.alloc(1024 * 1024));
  const run = (...args) => spawnSync(process.execPath, [
    "scripts/model-fit.mjs", "--host", "0.001", "--device", "Peer=0.001", "--overhead", "0", ...args,
  ], { encoding: "utf8" });

  const combined = run("--gguf", first, "--gguf", second);
  assert.equal(combined.status, 0, combined.stderr);
  assert.match(combined.stdout, /Custom model: .*Split estimate: This computer \+ Peer/);
  const duplicate = run("--gguf", first, "--gguf", first);
  assert.equal(duplicate.status, 1);
  assert.match(duplicate.stderr, /List each GGUF shard only once/);
  const conflict = run("--gguf", first, "--weights", "1");
  assert.equal(conflict.status, 1);
  assert.match(conflict.stderr, /Choose either --weights or --gguf/);
});
