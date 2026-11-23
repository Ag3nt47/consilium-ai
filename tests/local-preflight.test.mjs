import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

const script = fileURLToPath(new URL("../scripts/local-preflight.mjs", import.meta.url));

test("preflight checks unstaged new files and accepts clean CRLF", () => {
  const cwd = mkdtempSync(join(tmpdir(), "consilium-preflight-"));
  try {
    const git = (...args) => {
      const result = spawnSync("git", args, { cwd, encoding: "utf8" });
      assert.equal(result.status, 0, result.stderr);
    };
    git("init", "-q");
    git("-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "-c", "commit.gpgsign=false", // consilium-allow-secret-scan (reserved fixture email)
      "commit", "--allow-empty", "-qm", "Initial commit");
    writeFileSync(join(cwd, "new file.txt"), "clean\r\n");
    const preflight = () => spawnSync(process.execPath, [script, "new file.txt"], { cwd, encoding: "utf8" });
    const clean = preflight();
    assert.equal(clean.status, 0, clean.stderr);
    writeFileSync(join(cwd, "new file.txt"), "private fixture text \r\n");
    const dirty = preflight();
    assert.equal(dirty.status, 1, dirty.stderr);
    assert.match(dirty.stderr, /new file\.txt:1: trailing whitespace/);
    assert.doesNotMatch(dirty.stderr, /private fixture text/);
    const machine = spawnSync(process.execPath, [script, "--json", "new file.txt"], { cwd, encoding: "utf8" });
    assert.equal(machine.status, 1);
    assert.equal(machine.stderr, "");
    const report = JSON.parse(machine.stdout);
    assert.equal(report.ok, false);
    assert.equal(report.syntaxChecked, 0);
    assert.equal(report.patchErrors.length, 1);
    assert.match(report.patchErrors[0], /new file\.txt:1: trailing whitespace/);
    assert.deepEqual(report.syntaxErrors, []);
  } finally {
    rmSync(cwd, { recursive: true, force: true });
  }
});
