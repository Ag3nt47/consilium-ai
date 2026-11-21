// Tests for scripts/clean.mjs: verify safe detection and removal of temporary files and workspaces.
import { test } from "node:test";
import assert from "node:assert/strict";
import { existsSync, mkdirSync, mkdtempSync, rmSync, symlinkSync, utimesSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { findCleanableTargets, runClean } from "../scripts/clean.mjs";

function ownedTemp(root, name) {
  const folder = join(root, name);
  mkdirSync(folder);
  writeFileSync(join(folder, ".consilium-temp-owner"), "io.github.ag3nt47.consilium\n");
  writeFileSync(join(folder, "payload.txt"), "temporary data");
  return folder;
}

test("identifies Consilium temporary artifacts while preserving unrelated files", () => {
  const fakeTmp = mkdtempSync(join(tmpdir(), "consilium-test-clean-tmp-"));
  const fakeData = mkdtempSync(join(tmpdir(), "consilium-test-clean-data-"));
  const fakeRoot = mkdtempSync(join(tmpdir(), "consilium-test-clean-root-"));

  try {
    const owned = ownedTemp(fakeTmp, "consilium-owned");
    const wrongOwner = join(fakeTmp, "consilium-wrong-owner");
    mkdirSync(wrongOwner);
    writeFileSync(join(wrongOwner, ".consilium-temp-owner"), "another application\n");

    // Names alone do not establish ownership.
    mkdirSync(join(fakeTmp, "c0311"));
    writeFileSync(join(fakeTmp, "c0311", "test.txt"), "build");
    writeFileSync(join(fakeTmp, "stage0311.log"), "log content");
    writeFileSync(join(fakeTmp, "stage-ui-after0311.sh"), "#!/bin/sh");
    mkdirSync(join(fakeTmp, "consilium-screenshots"));
    mkdirSync(join(fakeTmp, "reaper-99999"));

    // Make timestamps old so idle check passes
    const oldTime = (Date.now() - 3600 * 1000) / 1000;
    utimesSync(join(fakeTmp, "c0311", "test.txt"), oldTime, oldTime);
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
      force: true,
      customTmpDirs: [fakeTmp],
      customDataDir: fakeData,
      customRootDir: fakeRoot,
      buildActive: false,
      turnActive: false,
    });

    const targetPaths = targets.map((t) => t.path);
    assert.ok(targetPaths.includes(owned));
    assert.ok(!targetPaths.includes(wrongOwner));
    assert.ok(!targetPaths.some((p) => p.endsWith("c0311")));
    assert.ok(!targetPaths.some((p) => p.endsWith("stage0311.log")));
    assert.ok(!targetPaths.some((p) => p.endsWith("stage-ui-after0311.sh")));
    assert.ok(!targetPaths.some((p) => p.endsWith("consilium-screenshots")));
    assert.ok(!targetPaths.some((p) => p.endsWith("reaper-99999")));
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
    const activeFile = ownedTemp(fakeTmp, "consilium-active");

    // Fresh file (modified right now) should be skipped by default
    const freshTargets = findCleanableTargets({
      customTmpDirs: [fakeTmp],
      customDataDir: fakeTmp,
      customRootDir: fakeTmp,
      force: false,
      minAgeMs: 60 * 1000,
      buildActive: false,
      turnActive: false,
    });
    assert.ok(!freshTargets.some((t) => t.path === activeFile));

    // With force: true, it is included
    const forcedTargets = findCleanableTargets({
      customTmpDirs: [fakeTmp],
      customDataDir: fakeTmp,
      customRootDir: fakeTmp,
      force: true,
      buildActive: false,
      turnActive: false,
    });
    assert.ok(forcedTargets.some((t) => t.path === activeFile));
  } finally {
    rmSync(fakeTmp, { recursive: true, force: true });
  }
});

test("dry-run reports targets without deleting them", () => {
  const fakeTmp = mkdtempSync(join(tmpdir(), "consilium-test-dry-tmp-"));
  try {
    const artifact = ownedTemp(fakeTmp, "consilium-dry-run");

    const res = runClean({
      dryRun: true,
      force: true,
      customTmpDirs: [fakeTmp],
      customDataDir: fakeTmp,
      customRootDir: fakeTmp,
      buildActive: false,
      turnActive: false,
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
    const artifact = ownedTemp(fakeTmp, "consilium-delete");
    const unrelated = join(fakeTmp, "user_work.txt");
    writeFileSync(unrelated, "hello");

    const res = runClean({
      dryRun: false,
      force: true,
      customTmpDirs: [fakeTmp],
      customDataDir: fakeTmp,
      customRootDir: fakeTmp,
      buildActive: false,
      turnActive: false,
    });

    assert.ok(!existsSync(artifact), "Matching artifact should be deleted");
    assert.ok(existsSync(unrelated), "Unrelated file must be preserved");
    assert.ok(res.count >= 1);
  } finally {
    rmSync(fakeTmp, { recursive: true, force: true });
  }
});

test("preserves live builds, AI workspaces, updates, and capacity locks", () => {
  const fakeTmp = mkdtempSync(join(tmpdir(), "consilium-test-active-tmp-"));
  const fakeData = mkdtempSync(join(tmpdir(), "consilium-test-active-data-"));
  const fakeRoot = mkdtempSync(join(tmpdir(), "consilium-test-active-root-"));
  try {
    const buildDir = ownedTemp(fakeTmp, "consilium-build");
    const turnDir = ownedTemp(fakeTmp, "consilium-turn");
    const updateTmp = join(fakeData, "update", "next.tmp");
    mkdirSync(updateTmp, { recursive: true });
    mkdirSync(join(fakeData, "agent-slots"));
    writeFileSync(join(fakeData, "agent-slots", "0"), "capacity lock");
    mkdirSync(join(fakeRoot, "dist"));
    const old = (Date.now() - 2 * 60 * 60 * 1000) / 1000;
    for (const path of [buildDir, turnDir, updateTmp]) utimesSync(path, old, old);

    const targets = findCleanableTargets({
      all: true,
      force: true,
      customTmpDirs: [fakeTmp],
      customDataDir: fakeData,
      customRootDir: fakeRoot,
      buildActive: true,
      turnActive: true,
    });
    const paths = targets.map((item) => item.path);
    assert.ok(!paths.includes(buildDir));
    assert.ok(!paths.includes(turnDir));
    assert.ok(!paths.includes(updateTmp));
    assert.ok(!paths.includes(join(fakeData, "agent-slots", "0")));
    assert.ok(!paths.includes(join(fakeRoot, "dist")));
  } finally {
    rmSync(fakeTmp, { recursive: true, force: true });
    rmSync(fakeData, { recursive: true, force: true });
    rmSync(fakeRoot, { recursive: true, force: true });
  }
});

test("counts only successful deletions as reclaimed space", () => {
  const fakeTmp = mkdtempSync(join(tmpdir(), "consilium-test-failure-tmp-"));
  try {
    const artifact = ownedTemp(fakeTmp, "consilium-failure");
    const result = runClean({
      force: true,
      customTmpDirs: [fakeTmp],
      customDataDir: fakeTmp,
      customRootDir: fakeTmp,
      buildActive: false,
      turnActive: false,
      remove: () => {
        throw new Error("simulated failure");
      },
    });
    assert.equal(result.count, 0);
    assert.equal(result.totalBytes, 0);
    assert.ok(existsSync(artifact));
  } finally {
    rmSync(fakeTmp, { recursive: true, force: true });
  }
});

test("detects a real stage process without matching future shell text", () => {
  const fakeTmp = mkdtempSync(join(tmpdir(), "consilium-test-process-tmp-"));
  const fakeData = mkdtempSync(join(tmpdir(), "consilium-test-process-data-"));
  const fakeRoot = mkdtempSync(join(tmpdir(), "consilium-test-process-root-"));
  try {
    const buildDir = ownedTemp(fakeTmp, "consilium-build");
    const old = (Date.now() - 2 * 60 * 60 * 1000) / 1000;
    utimesSync(buildDir, old, old);
    const common = {
      minAgeMs: 0,
      customTmpDirs: [fakeTmp],
      customDataDir: fakeData,
      customRootDir: fakeRoot,
      turnActive: false,
    };
    assert.ok(
      findCleanableTargets({
        ...common,
        customProcessList: "bash /bin/bash -lc 'npm run build later'",
      }).some((item) => item.path === buildDir),
    );
    assert.ok(
      !findCleanableTargets({
        ...common,
        customProcessList: "node /workspace/scripts/stage-update.mjs",
      }).some((item) => item.path === buildDir),
    );
  } finally {
    rmSync(fakeTmp, { recursive: true, force: true });
    rmSync(fakeData, { recursive: true, force: true });
    rmSync(fakeRoot, { recursive: true, force: true });
  }
});


test("preserves links to marked directories", (t) => {
  const fakeTmp = mkdtempSync(join(tmpdir(), "consilium-test-link-tmp-"));
  const outside = mkdtempSync(join(tmpdir(), "consilium-test-link-outside-"));
  try {
    const owned = ownedTemp(outside, "owned");
    const link = join(fakeTmp, "consilium-linked");
    try {
      symlinkSync(owned, link, process.platform === "win32" ? "junction" : "dir");
    } catch (err) {
      if (!["EPERM", "EACCES", "ENOSYS"].includes(err.code)) throw err;
      t.skip("Directory links unavailable");
      return;
    }
    const result = runClean({
      force: true,
      customTmpDirs: [fakeTmp],
      customDataDir: fakeTmp,
      customRootDir: fakeTmp,
      buildActive: false,
      turnActive: false,
    });
    assert.equal(result.count, 0);
    assert.ok(existsSync(link));
    assert.ok(existsSync(join(owned, "payload.txt")));
  } finally {
    rmSync(fakeTmp, { recursive: true, force: true });
    rmSync(outside, { recursive: true, force: true });
  }
});
