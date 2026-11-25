import { test } from "node:test";
import assert from "node:assert/strict";
import { hiddenFromTray } from "../src/tray-filter.mjs";

const key = "event-1#0";
const has = new Set([key]);
const none = new Set();

test("only a box the backend runs and this window didn't start is hidden", () => {
  assert.equal(hiddenFromTray(key, has, none), true, "auto-run started it: hidden");
  assert.equal(hiddenFromTray(key, has, has), false, "human clicked Run here: stays");
  assert.equal(hiddenFromTray(key, none, has), false, "started here, backend not reporting yet: stays");
  assert.equal(hiddenFromTray(key, none, none), false, "idle box: stays");
});

test("other keys are unaffected", () => {
  assert.equal(hiddenFromTray("event-2#1", has, none), false);
});