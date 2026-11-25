import test from "node:test";
import assert from "node:assert/strict";
import { relayExpiry, signalStatus } from "../src/signal-status.mjs";

const direct = (state) => [{ id: "signal", name: "Signal", state, detail: "Direct check" }];
const relay = { state: "available", peer: { node_id: "paired-pc", name: "My PC" }, valid_for_ms: 2500 };

test("healthy direct status wins over relay availability and ambiguity", () => {
  for (const report of [relay, { state: "multiple" }, null]) {
    assert.equal(signalStatus(direct("ok"), report, 4000, 2000).mode, "direct");
    assert.equal(signalStatus(direct("ok"), report, 4000, 2000).detail, "Direct check");
  }
});

test("a fresh relay names its mode and peer without replacing direct configuration", () => {
  const local = direct("down");
  const status = signalStatus(local, relay, 3500, 3000);
  assert.equal(status.state, "ok");
  assert.equal(status.mode, "relay");
  assert.match(status.detail, /My PC.*encrypted PeerSync/);
  assert.equal(local[0].state, "down");
  assert.equal(signalStatus(local, relay, 3500, 3500).state, "down");
  assert.equal(signalStatus(local, null, 3500, 3000).state, "down");
});

test("multiple candidates remain explicit rather than claiming one account works", () => {
  const status = signalStatus(direct("off"), { state: "multiple" }, 4000, 1000);
  assert.equal(status.mode, "multiple");
  assert.equal(status.state, "down");
  assert.match(status.detail, /Multiple linked computers/);
  assert.equal(signalStatus(direct("off"), { state: "multiple" }, 4000, 4001).state, "off");
});

test("IPC delay consumes the report lease and invalid leases fail closed", () => {
  assert.equal(relayExpiry(relay, 1000), 3500);
  assert.equal(signalStatus(direct("down"), relay, relayExpiry(relay, 1000), 3600).state, "down");
  for (const valid_for_ms of [0, -1, 4001, NaN, Infinity, undefined]) {
    assert.equal(relayExpiry({ ...relay, valid_for_ms }, 1000), 0);
  }
  assert.equal(relayExpiry(relay, NaN), 0);
});
