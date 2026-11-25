import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { defaultDataDir, inspectDurableState, launchCommand } from "./peer-sync-doctor.mjs";

test("doctor reports durable sync state without exposing keys or endpoint addresses", () => {
  const root = mkdtempSync(join(tmpdir(), "consilium-peer-doctor-"));
  const link = join(root, "link");
  mkdirSync(link);
  try {
    writeFileSync(join(link, "link.json"), JSON.stringify({
      enabled: true,
      name: "Test Mac",
      peers: [{ name: "Windows PC", key: "super-secret-key", addrs: ["192.168.1.2:8470"] }],
    }));
    writeFileSync(join(link, "outbox.json"), JSON.stringify({ peer: [{ id: "queued" }] }));
    writeFileSync(join(link, "mirrors.json"), JSON.stringify({ map: { a: "one", b: "two" } }));
    writeFileSync(join(link, "inbox.jsonl"), `${JSON.stringify({ id: "received" })}\n`);
    const report = inspectDurableState(root);
    assert.equal(report.enabled, true);
    assert.equal(report.peerCount, 1);
    assert.equal(report.queued, 1);
    assert.equal(report.inboxRecords, 1);
    assert.equal(report.mirrorCount, 2);
    assert.deepEqual(report.peers, [{ name: "Windows PC", routeCount: 1 }]);
    const printed = JSON.stringify(report);
    assert.doesNotMatch(printed, /super-secret-key|192\.168\.1\.2/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("doctor identifies damaged durable state and an interrupted inbox tail", () => {
  const root = mkdtempSync(join(tmpdir(), "consilium-peer-doctor-bad-"));
  const link = join(root, "link");
  mkdirSync(link);
  try {
    writeFileSync(join(link, "link.json"), "{");
    writeFileSync(join(link, "outbox.json"), "{}");
    writeFileSync(join(link, "mirrors.json"), "{}");
    writeFileSync(join(link, "inbox.jsonl"), `${JSON.stringify({ id: "complete" })}\n{"id":`);
    const report = inspectDurableState(root);
    assert.match(report.problems.join("\n"), /link\.json is not valid JSON/);
    assert.equal(report.inboxRecords, 1);
    assert.equal(report.invalidInboxRecords, 1);
    assert.match(report.warnings.join("\n"), /interrupted inbox record/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("platform paths and launch commands stay native", () => {
  assert.equal(defaultDataDir("darwin", {}, "/tmp/test-home"), "/tmp/test-home/Library/Application Support/io.github.ag3nt47.consilium");
  assert.equal(defaultDataDir("win32", { APPDATA: "C:\\Users\\test\\AppData\\Roaming" }, "unused"),
    "C:\\Users\\test\\AppData\\Roaming\\io.github.ag3nt47.consilium");
  assert.deepEqual(launchCommand("darwin", {}), { command: "/usr/bin/open", args: ["-a", "Consilium"] });
});
