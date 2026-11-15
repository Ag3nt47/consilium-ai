// Tests for scripts/clean.mjs: verify safe detection and removal of temporary files and workspaces.
import { test } from "node:test";
import assert from "node:assert/strict";
import { existsSync, mkdirSync, mkdtempSync, rmSync, utimesSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { findCleanableTargets, runClean } from "../scripts/clean.mjs";

test("identifies Consilium temporary artifacts while preserving unrelated files", () => {
  const fakeTmp = mkdtempSync(join(tmpdir(), "consilium-test-clean-tmp-"));
  const fakeData = mkdtempSync(join(tmpdir(), "consilium-test-clean-data-"));
  const fakeRoot = mkdtempSync(join(tmpdir(), "consilium-test-clean-root-"));

  try {
    // Matching artifacts
    mkdirSync(join(fakeTmp, "c0311"));
    writeFileSync(join(fakeTmp, "c0311", "test.txt"), "build");
    writeFileSync(join(fakeTmp, "stage0311.log"), "log content");
    writeFileSync(join(fakeTmp, "stage-ui-after0311.sh"), "#!/bin/sh");
    mkdirSync(join(fakeTmp, "consilium-screenshots"));
    mkdirSync(join(fakeTmp, "reaper-99999"));

    // Make timestamps old so idle check passes
    const oldTime = (Date.now() - 3600 * 1000) / 1000;
    utimesSync(join(fakeTmp, "c0311"), oldTime, oldTime);
    utimesSync(join(fakeTmp, "stage0311.log"), oldTime, oldTime);
    utimesSync(join(fakeTmp, "stage-ui-after0311.sh"), oldTime, oldTime);
    utimesSync(join(fakeTmp, "consilium-screenshots"), oldTime, oldTime);
    utimesSync(join(fakeTmp, "reaper-99999"), oldTime, oldTime);

    // Unrelated files that must NOT be touched
    writeFileSync(join(fakeTmp, "important_notes.txt"), "keep this");
    mkdirSync(join(fakeTmp, "other-app-dir"));

    // Matching repo artifacts
    mkdirSync(join(fakeRoot, "dist"));
    writeFileSync(join(fakeRoot, "dist", "index.html"), "<html></html>");

    const targets = findCleanableTargets({
      customTmpDirs: [fakeTmp],
      customDataDir: fakeData,
      customRootDir: fakeRoot,
    });

    const targetPaths = targets.map((t) => t.path);
    assert.ok(targetPaths.some((p) => p.endsWith("c0311")));
    assert.ok(targetPaths.some((p) => p.endsWith("stage0311.log")));
    assert.ok(targetPaths.some((p) => p.endsWith("stage-ui-after0311.sh")));
    assert.ok(targetPaths.some((p) => p.endsWith("consilium-screenshots")));
    assert.ok(targetPaths.some((p) => p.endsWith("reaper-99999")));
    assert.ok(targetPaths.some((p) => p.endsWith("dist")));

    // Unrelated files must NOT be in targets
    assert.ok(!targetPaths.some((p) => p.endsWith("important_notes.txt")));
    assert.ok(!targetPaths.some((p) => p.endsWith("other-app-dir")));
  } finally {
    rmSync(fakeTmp, { recursive: true, force: true });
    rmSync(fakeData, { recursive: true, force: true });
    rmSync(fakeRoot, { recursive: true, force: true });
  }
});

test("respects minimum idle age unless force is passed", () => {
  const fakeTmp = mkdtempSync(join(tmpdir(), "consilium-test-age-tmp-"));
  try {
    const activeFile = join(fakeTmp, "stage-active.log");
    writeFileSync(activeFile, "currently writing");

    // Fresh file (modified right now) should be skipped by default
    const freshTargets = findCleanableTargets({
      customTmpDirs: [fakeTmp],
      customDataDir: fakeTmp,
      customRootDir: fakeTmp,
      force: false,
      minAgeMs: 60 * 1000,
    });
    assert.ok(!freshTargets.some((t) => t.path === activeFile));

    // With force: true, it is included
    const forcedTargets = findCleanableTargets({
      customTmpDirs: [fakeTmp],
      customDataDir: fakeTmp,
      customRootDir: fakeTmp,
      force: true,
    });
    assert.ok(forcedTargets.some((t) => t.path === activeFile));
  } finally {
    rmSync(fakeTmp, { recursive: true, force: true });
  }
});

test("dry-run reports targets without deleting them", () => {
  const fakeTmp = mkdtempSync(join(tmpdir(), "consilium-test-dry-tmp-"));
  try {
    const artifact = join(fakeTmp, "stage-0310.sh");
    writeFileSync(artifact, "#!/bin/sh\necho test\n");

    const res = runClean({
      dryRun: true,
      force: true,
      customTmpDirs: [fakeTmp],
      customDataDir: fakeTmp,
      customRootDir: fakeTmp,
    });

    assert.ok(existsSync(artifact), "File must still exist after dry-run");
    assert.ok(res.targets.some((t) => t.path === artifact));
  } finally {
    rmSync(fakeTmp, { recursive: true, force: true });
  }
});

test("runClean deletes matching artifacts and leaves unrelated files", () => {
  const fakeTmp = mkdtempSync(join(tmpdir(), "consilium-test-run-tmp-"));
  try {
    const artifact = join(fakeTmp, "stage039.log");
    const unrelated = join(fakeTmp, "user_work.txt");
    writeFileSync(artifact, "build output");
    writeFileSync(unrelated, "hello");

    const res = runClean({
      dryRun: false,
      force: true,
      customTmpDirs: [fakeTmp],
      customDataDir: fakeTmp,
      customRootDir: fakeTmp,
    });

    assert.ok(!existsSync(artifact), "Matching artifact should be deleted");
    assert.ok(existsSync(unrelated), "Unrelated file must be preserved");
    assert.ok(res.count >= 1);
  } finally {
    rmSync(fakeTmp, { recursive: true, force: true });
  }
});
