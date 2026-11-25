import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

const script = fileURLToPath(new URL("./local-preflight.mjs", import.meta.url));

test("changed-source preflight selects a bounded worktree scope without staging files", () => {
  const cwd = mkdtempSync(join(tmpdir(), "consilium-changed-preflight-"));
  const git = (...args) => {
    const result = spawnSync("git", args, { cwd, encoding: "utf8" });
    assert.equal(result.status, 0, result.stderr);
    return result.stdout;
  };
  const write = (file, content) => writeFileSync(join(cwd, file), content);
  const check = () => spawnSync(process.execPath, [script, "--json", "--changed"], { cwd, encoding: "utf8" });
  const clean = (count) => {
    const result = check();
    assert.equal(result.status, 0, result.stderr || result.stdout);
    assert.equal(JSON.parse(result.stdout).syntaxChecked, count);
  };
  const commit = () => git("-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", // consilium-allow-secret-scan (reserved fixture email)
    "-c", "commit.gpgsign=false", "commit", "-qm", "Source fixture");

  try {
    git("init", "-q");
    clean(0);
    write(".gitignore", "ignored.ts\n");
    write(".gitattributes", "generated.ts linguist-generated\n");
    write("tracked.ts", "const value = 0;\n");
    write("removed.ts", "const old = 0;\n");
    git("add", "--", ".gitattributes", ".gitignore", "tracked.ts", "removed.ts");
    clean(2);
    commit();
    clean(0);

    write("staged.ts", "const staged = 1;\n");
    git("add", "--", "staged.ts");
    write("tracked.ts", "const value = 1;\r\n");
    rmSync(join(cwd, "removed.ts"));
    write("new file.ts", "const bad = ;\n");
    write("ignored.ts", "const ignored = ;\n");
    write("generated.ts", "const generated = ;\n");
    write("notes.txt", "Not a source file \n");

    const before = git("status", "--porcelain=v1");
    const failed = check();
    assert.equal(failed.status, 1, failed.stderr || failed.stdout);
    const report = JSON.parse(failed.stdout);
    assert.equal(report.syntaxChecked, 3);
    assert.deepEqual(report.patchErrors, []);
    assert.equal(report.syntaxErrors.length, 1);
    assert.equal(report.syntaxErrors[0].path, "new file.ts");
    assert.match(report.syntaxErrors[0].message, /Parse error at 1:/);
    assert.equal(git("status", "--porcelain=v1"), before);

    for (let index = 0; index < 14; index++) write(`extra-${index}.ts`, "export {};\n");
    const oversized = check();
    assert.equal(oversized.status, 2, oversized.stderr || oversized.stdout);
    assert.match(oversized.stderr, /17 changed source files.*current task explicitly/);
  } finally {
    rmSync(cwd, { recursive: true, force: true });
  }
});
