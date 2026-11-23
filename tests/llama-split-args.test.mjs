import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
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
