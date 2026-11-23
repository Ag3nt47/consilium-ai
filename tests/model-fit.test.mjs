import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
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

test("a capacity-weighted split allocates by post-reserve RAM and skips full devices", () => {
  const devices = [
    { name: "Windows", memory: 14 },
    { name: "Mac", memory: 6 },
    { name: "Phone", memory: 2 },
  ];
  const estimate = (weights) => modelFit(devices, 4, 2, weights, true, "capacity")
    .find(({ name }) => name === "Custom model");
  assert.deepEqual(estimate(8).allocation, [{ name: "Windows", allocated: 6 }, { name: "Mac", allocated: 2 }]);
  assert.deepEqual(estimate(16).allocation, [{ name: "Windows", allocated: 12 }, { name: "Mac", allocated: 4 }]);
  assert.match(estimate(17).fit, /Needs at least 1.0 GiB more/);
});

test("the CLI exports a capacity-weighted split plan", () => {
  const result = spawnSync(process.execPath, [
    "scripts/model-fit.mjs", "--json", "--host", "14", "--device", "Mac=6", "--weights", "8", "--split", "--capacity-weighted",
  ], { encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr);
  const custom = JSON.parse(result.stdout).models.find(({ name }) => name === "Custom model");
  assert.match(custom.fit, /^Capacity-weighted split estimate/);
  assert.deepEqual(custom.allocation, [{ name: "This computer", allocated: 6 }, { name: "Mac", allocated: 2 }]);
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

test("the CLI does not count the same device name twice", () => {
  for (const names of [["Mac=6", " mac =6"], ["This computer=6"]]) {
    const result = spawnSync(process.execPath, [
      "scripts/model-fit.mjs", "--json", "--host", "8",
      ...names.flatMap(name => ["--device", name]),
    ], { encoding: "utf8" });
    assert.equal(result.status, 1);
    assert.match(result.stderr, /Use a unique name for each device/);
    assert.equal(result.stdout, "");
  }
});

test("the CLI estimates fit from the combined size of separate GGUF shards", (t) => {
  const folder = mkdtempSync(join(tmpdir(), "consilium-model-fit-"));
  t.after(() => rmSync(folder, { recursive: true, force: true }));
  const first = join(folder, "model-00001-of-00002.gguf");
  const second = join(folder, "model-00002-of-00002.gguf");
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

  const other = join(folder, "other.gguf");
  const standalone = join(folder, "standalone.gguf");
  writeFileSync(other, Buffer.alloc(1));
  writeFileSync(standalone, Buffer.alloc(1));
  const unrelated = run("--gguf", standalone, "--gguf", other);
  assert.equal(unrelated.status, 1);
  assert.match(unrelated.stderr, /Select one GGUF file or all numbered shards/);
  assert.equal(unrelated.stdout, "");
});

test("numbered GGUF shard sets must be complete and match one model", () => {
  assert.equal(ggufShardError(["model-00002-of-00002.gguf", "model-00001-of-00002.gguf"]), undefined);
  assert.match(ggufShardError(["model-00001-of-00002.gguf"]), /all 2/);
  assert.match(ggufShardError(["model-00001-of-00002.gguf", "other-00002-of-00002.gguf"]), /one model/);
  assert.match(ggufShardError(["model-00000-of-00002.gguf", "model-00002-of-00002.gguf"]), /all 2/);
  assert.equal(ggufShardError(["single.gguf"]), undefined);
});

test("the CLI uses installed Ollama weight blobs without running Ollama", (t) => {
  const root = mkdtempSync(join(tmpdir(), "consilium-ollama-fit-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const manifests = join(root, "manifests", "registry.ollama.ai", "library", "qwen3");
  const blobs = join(root, "blobs");
  mkdirSync(manifests, { recursive: true });
  mkdirSync(blobs);
  const digest = "a".repeat(64);
  writeFileSync(join(manifests, "1.7b"), JSON.stringify({ layers: [
    { mediaType: "application/vnd.ollama.image.model", digest: `sha256:${digest}`, size: 1024 },
    { mediaType: "application/vnd.ollama.image.template", size: 256 },
  ] }));
  const blob = join(blobs, `sha256-${digest}`);
  writeFileSync(blob, Buffer.alloc(1024));
  const run = (...args) => spawnSync(process.execPath, ["scripts/model-fit.mjs", "--json", ...args], {
    encoding: "utf8", env: { ...process.env, OLLAMA_MODELS: root },
  });

  const result = run("--ollama", "qwen3:1.7b", "--split", "--capacity-weighted", "--host", "14", "--device", "Mac=6");
  assert.equal(result.status, 0, result.stderr);
  const report = JSON.parse(result.stdout);
  assert.equal(report.ollamaModel, "qwen3:1.7b");
  assert.equal(report.ollamaBytes, 1024);
  const model = report.models.find(({ name }) => name === "qwen3:1.7b");
  assert.equal(model.weights, 1024 / 2 ** 30);
  assert.deepEqual(model.allocation, [
    { name: "This computer", allocated: 768 / 2 ** 30 },
    { name: "Mac", allocated: 256 / 2 ** 30 },
  ]);
  assert.match(run("--ollama", "qwen3:1.7b", "--weights", "1").stderr, /Choose one model size source/);
  assert.match(run("--ollama", "../qwen3:1.7b").stderr, /expects/);
  writeFileSync(blob, Buffer.alloc(1));
  assert.match(run("--ollama", "qwen3:1.7b").stderr, /wrong size/);
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
  assert.equal(model.equalSplitMemoryGiB, 6);
  assert.deepEqual(model.equalSplitShortfalls, [{ name: "Mac", missingGiB: 1 }]);
  assert.deepEqual(model.allocation, [
    { name: "This computer", allocated: 5 },
    { name: "Mac", allocated: 3 },
  ]);
});
