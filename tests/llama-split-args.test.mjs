import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

const plan = JSON.stringify({
  models: [{ name: "Custom model", weights: 8, allocation: [
    { name: "This computer", allocated: 6 }, { name: "Mac", allocated: 2 },
  ] }],
});

function mapDevices(...bindings) {
  return spawnSync(process.execPath, [
    "scripts/llama-split-args.mjs", "--model", "Custom model",
    ...bindings.flatMap((binding) => ["--device", binding]),
  ], { input: plan, encoding: "utf8" });
}

test("llama.cpp layer proportions follow the explicit runner device order", () => {
  const result = mapDevices("Mac=RPC0", "This computer=CUDA0");
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(JSON.parse(result.stdout), [
    "--split-mode", "layer", "--device", "RPC0,CUDA0", "--tensor-split", "0.25,0.75",
  ]);
});

test("the placement arguments require one distinct runner device per allocated computer", () => {
  for (const bindings of [
    ["Mac=RPC0"], ["Mac=RPC0", "This computer=RPC0"],
    ["Mac=RPC0,CUDA0", "This computer=CUDA1"], ["Other=RPC0", "This computer=CUDA0"],
  ]) {
    const result = mapDevices(...bindings);
    assert.equal(result.status, 1);
    assert.match(result.stderr, /computer/i);
    assert.equal(result.stdout, "");
  }
});

test("a saved runner inventory checks available memory and device names", (t) => {
  const folder = mkdtempSync(join(tmpdir(), "consilium-runner-inventory-"));
  t.after(() => rmSync(folder, { recursive: true, force: true }));
  const inventory = join(folder, "devices.txt");
  const save = (free) => writeFileSync(inventory, [
    "Available devices:",
    `  Vulkan0: NVIDIA GPU (8192 MiB, ${free} MiB free)`,
    "  RPC[127.0.0.1:50052]: Apple Metal (4096 MiB, 2800 MiB free)",
  ].join("\r\n"));
  const run = (remote = "RPC[127.0.0.1:50052]") => spawnSync(process.execPath, [
    "scripts/llama-split-args.mjs", "--model", "Custom model", "--runner-inventory", inventory,
    "--device", "This computer=Vulkan0", "--device", `Mac=${remote}`,
  ], { input: plan, encoding: "utf8" });
  save(6400);
  const ok = run();
  assert.equal(ok.status, 0, ok.stderr);
  assert.equal(JSON.parse(ok.stdout).at(-1), "0.75,0.25");
  assert.match(run("RPC2").stderr, /RPC2 was missing/);
  save(6000);
  const tooLarge = run();
  assert.equal(tooLarge.status, 1, tooLarge.stderr);
  assert.match(tooLarge.stderr, /Vulkan0 has 6000 MiB free.*need 6144 MiB/);
  assert.equal(tooLarge.stdout, "");
});
