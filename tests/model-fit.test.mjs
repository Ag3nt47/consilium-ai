import assert from "node:assert/strict";
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
