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

test("RPC tunnel arguments preserve the saved registration order and the selected device order", (t) => {
  const folder = mkdtempSync(join(tmpdir(), "consilium-rpc-inventory-"));
  t.after(() => rmSync(folder, { recursive: true, force: true }));
  const inventory = join(folder, "devices.txt");
  writeFileSync(inventory, [
    "Available devices:",
    "  CUDA0: NVIDIA GPU (8192 MiB, 6400 MiB free)",
    "  RPC0: 127.0.0.1:50052 (4096 MiB, 3000 MiB free)",
    "  RPC1: 127.0.0.1:50052 (4096 MiB, 3000 MiB free)",
    "  RPC2: 127.0.0.1:50053 (6144 MiB, 4000 MiB free)",
  ].join("\r\n"));
  const input = JSON.stringify({ models: [{ name: "Distributed model", weights: 10, allocation: [
    { name: "This computer", allocated: 6 }, { name: "Mac", allocated: 2 }, { name: "Linux", allocated: 2 },
  ] }] });
  const run = (tunnels, bindings = ["Linux=RPC2", "This computer=CUDA0", "Mac=RPC1"], saved = true) =>
    spawnSync(process.execPath, [
      "scripts/llama-split-args.mjs", "--model", "Distributed model",
      ...(saved ? ["--runner-inventory", inventory] : []),
      ...tunnels.flatMap((value) => ["--rpc-tunnel", value]),
      ...bindings.flatMap((value) => ["--device", value]),
    ], { input, encoding: "utf8" });
  const tunnels = ["Mac=127.0.0.1:50052", "Linux=127.0.0.1:50053"];
  const valid = run(tunnels);
  assert.equal(valid.status, 0, valid.stderr);
  assert.deepEqual(JSON.parse(valid.stdout), [
    "--rpc", "127.0.0.1:50052,127.0.0.1:50053",
    "--split-mode", "layer", "--device", "RPC2,CUDA0,RPC1", "--tensor-split", "0.2,0.6,0.2",
  ]);
  for (const [result, message] of [
    [run([...tunnels].reverse()), /order must match/],
    [run([tunnels[1]]), /order must match/],
    [run([tunnels[0], "Linux=192.168.1.50:50053"]), /distinct local tunnel/],
    [run([tunnels[0], "Linux=127.0.0.1:65536"]), /distinct local tunnel/],
    [run([tunnels[0], "Linux=127.0.0.1:50052"]), /distinct local tunnel/],
    [run(tunnels, ["Linux=RPC1", "This computer=CUDA0", "Mac=RPC2"]), /does not match the RPC tunnel/],
    [run(tunnels, undefined, false), /require saved --list-devices output/],
  ]) {
    assert.equal(result.status, 1);
    assert.match(result.stderr, message);
    assert.equal(result.stdout, "");
  }
});
