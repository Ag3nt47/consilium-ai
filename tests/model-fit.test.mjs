import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { ggufShardError, modelFit } from "../src/model-fit.mjs";

function customFit(devices, weights) {
  return modelFit(devices, 4, 2, weights).find(({ name }) => name === "Custom model").fit;
}

test("split estimate reserves runtime memory on each device", () => {
  const windows = { name: "Windows", memory: 10 };
  const mac = { name: "Mac", memory: 6 };
  assert.equal(customFit([windows], 10), "Needs at least 2.0 GiB more for weights");
  assert.equal(customFit([mac], 10), "Needs at least 6.0 GiB more for weights");
  assert.equal(customFit([mac, windows], 10), "Split estimate: Windows + Mac (Windows 6.0 GiB, Mac 4.0 GiB)");
  assert.equal(customFit([windows, { name: "Linux", memory: 10 }], 10),
    "Split estimate: Windows + Linux (Windows 5.0 GiB, Linux 5.0 GiB)");
  assert.equal(customFit([mac, windows], 13), "Needs at least 1.0 GiB more for weights");
});

test("split weights balance across three devices after capping a small peer", () => {
  const phone = { name: "Phone", memory: 3 };
  const mac = { name: "Mac", memory: 6 };
  const windows = { name: "Windows", memory: 10 };
  assert.equal(customFit([phone, mac, windows], 10),
    "Split estimate: Windows + Mac + Phone (Windows 5.0 GiB, Mac 4.0 GiB, Phone 1.0 GiB)");
  assert.equal(customFit([phone, mac, windows], 13),
    "Split estimate: Windows + Mac + Phone (Windows 8.0 GiB, Mac 4.0 GiB, Phone 1.0 GiB)");
});

test("single-device fit takes precedence when one computer has enough memory", () => {
  assert.equal(customFit([
    { name: "Windows", memory: 12 },
    { name: "Mac", memory: 6 },
  ], 10), "One device: Windows");
});

test("a requested split balances weights even if a single device can fit them", () => {
  const devices = [{ name: "Windows", memory: 10 }, { name: "Mac", memory: 5 }];
  const fit = (nodes) => modelFit(nodes, 4, 2, 8, true)
    .find(({ name }) => name === "Custom model").fit;
  assert.equal(fit(devices), "Split estimate: Windows + Mac (Windows 5.0 GiB, Mac 3.0 GiB)");
  assert.equal(fit([devices[0], { name: "Mac", memory: 2 }]),
    "Split needs two devices with weight capacity. One device: Windows");
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

test("numbered GGUF shard sets must be complete and match one model", () => {
  assert.equal(ggufShardError(["model-00002-of-00002.gguf", "model-00001-of-00002.gguf"]), undefined);
  assert.match(ggufShardError(["model-00001-of-00002.gguf"]), /all 2/);
  assert.match(ggufShardError(["model-00001-of-00002.gguf", "other-00002-of-00002.gguf"]), /one model/);
  assert.match(ggufShardError(["model-00000-of-00002.gguf", "model-00002-of-00002.gguf"]), /all 2/);
  assert.equal(ggufShardError(["single.gguf"]), undefined);
});

test("the CLI reports per-device and combined weight capacity after runtime reserve", () => {
  const result = spawnSync(process.execPath, [
    "scripts/model-fit.mjs", "--host", "10", "--device", "Mac=6", "--device", "Phone=1",
    "--overhead", "2", "--weights", "11",
  ], { encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /This computer: 10\.0 GiB usable, 8\.0 GiB for weights/);
  assert.match(result.stdout, /Mac: 6\.0 GiB usable, 4\.0 GiB for weights/);
  assert.match(result.stdout, /Phone: 1\.0 GiB usable, 0\.0 GiB for weights/);
  assert.match(result.stdout, /Combined weight capacity: 12\.0 GiB/);
  assert.match(result.stdout, /Custom model: 11\.0 GiB weights.*Split estimate: This computer \+ Mac/);
});

test("the CLI exports a numeric per-device split plan", () => {
  const result = spawnSync(process.execPath, [
    "scripts/model-fit.mjs", "--host", "10", "--device", "Mac=6", "--weights", "10", "--json",
  ], { encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr);
  const plan = JSON.parse(result.stdout);
  assert.equal(plan.runtimeReserveGiB, 2);
  assert.deepEqual(plan.models.find(({ name }) => name === "Custom model").allocation, [
    { name: "This computer", allocated: 6 }, { name: "Mac", allocated: 4 },
  ]);
});

test("the CLI exports an explicitly requested split for a model that fits locally", () => {
  const result = spawnSync(process.execPath, [
    "scripts/model-fit.mjs", "--json", "--split", "--host", "10",
    "--device", "Mac=5", "--overhead", "2", "--weights", "8",
  ], { encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr);
  const model = JSON.parse(result.stdout).models.find(({ name }) => name === "Custom model");
  assert.deepEqual(model.allocation, [
    { name: "This computer", allocated: 5 },
    { name: "Mac", allocated: 3 },
  ]);
});
