#!/usr/bin/env node
// Cleans up temporary files, build workspaces, staging logs, and leftover artifacts
// so the host computer does not get bogged down or run out of disk space.
//
// Usage:
//   node scripts/clean.mjs            Safe clean: idle temp workspaces, logs, test artifacts, dist/
//   node scripts/clean.mjs --all      Deep clean: includes src-tauri/target build cache and cached repos
//   node scripts/clean.mjs --target   Cleans src-tauri/target Rust build cache
//   node scripts/clean.mjs --repos    Cleans cached GitHub repo shallow clones
//   node scripts/clean.mjs --force    Ignore minimum age; live turns and builds remain protected
//   node scripts/clean.mjs --dry-run  Lists files and bytes to clean without deleting

import { execFileSync } from "node:child_process";
import { existsSync, lstatSync, readFileSync, readdirSync, rmSync } from "node:fs";
import { homedir, platform, tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(fileURLToPath(new URL(".", import.meta.url)), "..");
const ID = "io.github.ag3nt47.consilium";
const OS = platform();

const DATA =
  process.env.CONSILIUM_DATA ||
  (OS === "darwin"
    ? join(homedir(), "Library", "Application Support", ID)
    : OS === "win32"
      ? join(process.env.APPDATA || join(homedir(), "AppData", "Roaming"), ID)
      : join(process.env.XDG_DATA_HOME || join(homedir(), ".local", "share"), ID));

// Only dedicated disposable folders explicitly marked by a producer belong to us.
const TEMP_OWNER_FILE = ".consilium-temp-owner";
const TEMP_OWNER_CONTENT = ID + "\n";

function isOwnedTemp(folder) {
  try {
    const dir = lstatSync(folder);
    if (!dir.isDirectory() || dir.isSymbolicLink()) return false;
    const marker = join(folder, TEMP_OWNER_FILE);
    const st = lstatSync(marker);
    return st.isFile() && !st.isSymbolicLink()
      && st.size === Buffer.byteLength(TEMP_OWNER_CONTENT)
      && readFileSync(marker, "utf8") === TEMP_OWNER_CONTENT;
  } catch {
    return false;
  }
}

// A full hour avoids mistaking a slow AI turn or release link for abandoned work.
const DEFAULT_MIN_AGE_MS = 60 * 60 * 1000;

function processList() {
  try {
    if (OS === "win32") {
      return execFileSync(
        "powershell.exe",
        ["-NoProfile", "-NonInteractive", "-Command", "Get-CimInstance Win32_Process | ForEach-Object { \"$($_.Name) $($_.CommandLine)\" }"],
        { encoding: "utf8", timeout: 5000, windowsHide: true },
      ).toLowerCase();
    }
    return execFileSync("ps", ["-A", "-o", "comm=", "-o", "args="], { encoding: "utf8", timeout: 5000 }).toLowerCase();
  } catch {
    return null;
  }
}

function activity(processes = processList()) {
  // If process inspection fails, preserve anything that might be live.
  if (processes === null) return { buildActive: true, turnActive: true };
  const lines = processes.split(/\r?\n/).filter(Boolean);
  const executable = (line) => line.trim().split(/\s+/, 1)[0].split(/[\\/]/).pop().replace(/\.exe$/i, "");
  const buildActive = lines.some((line) => {
    const exe = executable(line);
    if (/^(cargo|rustc)$/.test(exe)) return true;
    if (/^(bash|sh|zsh)$/.test(exe) && /stage[^\s]*\.sh/i.test(line)) return true;
    if (!/^(node|npm|npx|tauri|vite|tsc)$/.test(exe)) return false;
    return /tauri(\.exe)?\s+build|stage-(update|ui)(\.mjs|\.sh)?|vite(\.js)?\s+(build|dev)|tsc(\.js)?\s+--noemit/i.test(line);
  });
  const turnActive = lines.some((line) => /^(consilium|codex|claude|agy)$/.test(executable(line)));
  return { buildActive, turnActive };
}

function inspectPath(path) {
  let bytes = 0;
  let newestMtimeMs = 0;
  try {
    const st = lstatSync(path);
    if (st.isSymbolicLink()) return { bytes: 0, newestMtimeMs: st.mtimeMs };
    newestMtimeMs = st.mtimeMs;
    if (!st.isDirectory()) return { bytes: st.size, newestMtimeMs };
    bytes += st.size;
    const entries = readdirSync(path, { withFileTypes: true });
    for (const entry of entries) {
      if (entry.isSymbolicLink()) continue;
      const full = join(path, entry.name);
      try {
        if (entry.isDirectory()) {
          const child = inspectPath(full);
          bytes += child.bytes;
          newestMtimeMs = Math.max(newestMtimeMs, child.newestMtimeMs);
        } else {
          const child = lstatSync(full);
          if (child.isSymbolicLink()) continue;
          bytes += child.size;
          newestMtimeMs = Math.max(newestMtimeMs, child.mtimeMs);
        }
      } catch {}
    }
  } catch {}
  return { bytes, newestMtimeMs };
}

function formatBytes(bytes) {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

export function findCleanableTargets(opts = {}) {
  const {
    all = false,
    target = false,
    repos = false,
    force = false,
    minAgeMs = DEFAULT_MIN_AGE_MS,
    customTmpDirs = null,
    customDataDir = null,
    customRootDir = null,
    nowMs = Date.now(),
  } = opts;

  const rootDir = customRootDir || ROOT;
  const dataDir = customDataDir || DATA;
  const targets = [];
  const detected = activity(opts.customProcessList);
  const buildActive = opts.buildActive ?? detected.buildActive;
  const turnActive = opts.turnActive ?? detected.turnActive;

  const add = (path, category, { aged = false, buildSensitive = false, turnSensitive = false } = {}) => {
    try { if (lstatSync(path).isSymbolicLink()) return; } catch { return; }
    const info = inspectPath(path);
    if (aged && !force && minAgeMs > 0 && nowMs - info.newestMtimeMs < minAgeMs) return;
    if (buildSensitive && buildActive) return;
    if (turnSensitive && turnActive) return;
    targets.push({ path, category, bytes: info.bytes });
  };

  // 1. Temporary directories (system tmpdir and /tmp on Unix).
  const tmpDirs = customTmpDirs || (OS === "win32" ? [tmpdir()] : Array.from(new Set([tmpdir(), "/tmp"])));
  for (const dir of tmpDirs) {
    if (!existsSync(dir)) continue;
    try {
      const entries = readdirSync(dir);
      for (const name of entries) {
        if (isOwnedTemp(join(dir, name))) {
          const full = join(dir, name);
          add(full, "temporary artifact", {
            aged: true,
            buildSensitive: true,
            turnSensitive: true,
          });
        }
      }
    } catch {}
  }

  // 2. Data directory leftovers (staged update temp folders, stale agent-slot locks, repo caches).
  if (existsSync(dataDir)) {
    // update/*.tmp
    const updateDir = join(dataDir, "update");
    if (existsSync(updateDir)) {
      try {
        for (const name of readdirSync(updateDir)) {
          if (name.endsWith(".tmp")) {
            const full = join(updateDir, name);
            add(full, "interrupted update", { aged: true, buildSensitive: true });
          }
        }
      } catch {}
    }

    // Never clean agent-slots here. Their numeric names are capacity slot IDs,
    // not PIDs, and the engine already recovers abandoned file locks safely.

    // Cached ingested repositories (cleaned when --all or --repos is requested)
    if (all || repos) {
      const reposDir = join(dataDir, "repos");
      if (existsSync(reposDir)) {
        try {
          for (const name of readdirSync(reposDir)) {
            const full = join(reposDir, name);
            add(full, "cached ingested repo", { aged: true, turnSensitive: true });
          }
        } catch {}
      }
    }
  }

  // 3. Project repository artifacts.
  const distDir = join(rootDir, "dist");
  if (existsSync(distDir)) {
    add(distDir, "frontend build (dist/)", { buildSensitive: true });
  }

  const viteCache = join(rootDir, "node_modules", ".vite");
  if (existsSync(viteCache)) {
    add(viteCache, "vite cache", { buildSensitive: true });
  }

  // Cargo target directory (cleaned when --all or --target is specified).
  if (all || target) {
    const targetDir = join(rootDir, "src-tauri", "target");
    if (existsSync(targetDir)) {
      add(targetDir, "Rust build cache (target/)", { buildSensitive: true });
    }
  }

  return targets;
}

export function runClean(opts = {}) {
  const { dryRun = false } = opts;
  const targets = findCleanableTargets(opts);
  const cleanableBytes = targets.reduce((sum, item) => sum + item.bytes, 0);
  let reclaimedBytes = 0;
  let cleanedCount = 0;

  for (const item of targets) {
    // Recheck ownership immediately before acting, including when --force is used.
    if (item.category === "temporary artifact" && !isOwnedTemp(item.path)) {
      console.warn("Skipped temporary artifact whose ownership changed: " + item.path);
      continue;
    }
    if (dryRun) {
      console.log(`[dry-run] Would remove: ${item.path} (${item.category}, ${formatBytes(item.bytes)})`);
    } else {
      try {
        (opts.remove || rmSync)(item.path, { recursive: true, force: true });
        if (existsSync(item.path)) throw new Error("path still exists after removal");
        cleanedCount++;
        reclaimedBytes += item.bytes;
        console.log(`Removed: ${item.path} (${item.category}, ${formatBytes(item.bytes)})`);
      } catch (err) {
        console.warn(`Could not remove ${item.path}: ${err.message}`);
      }
    }
  }

  if (dryRun) {
    console.log(`[dry-run] Total cleanable: ${targets.length} items, ${formatBytes(cleanableBytes)}.`);
    return { count: targets.length, totalBytes: cleanableBytes, targets };
  }
  console.log(`Clean complete: ${cleanedCount} items, reclaimed ${formatBytes(reclaimedBytes)}.`);
  return { count: cleanedCount, totalBytes: reclaimedBytes, targets };
}

// Direct CLI invocation
if (process.argv[1] && resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url))) {
  const args = new Set(process.argv.slice(2));
  if (args.has("--help") || args.has("-h")) {
    console.log(`Usage: node scripts/clean.mjs [options]

Options:
  --dry-run   Show items and estimated reclaimed bytes without deleting
  --all       Deep clean: temp artifacts, dist/, cached repos, and src-tauri/target
  --target    Clean src-tauri/target Rust build directory
  --repos     Clean cached shallow repo clones in Consilium data folder
  --force     Ignore the one-hour age check (active work is still preserved)
  -h, --help  Show this help message

Active AI turns, builds, staged updates, and agent capacity locks are always
preserved. If the process table cannot be read, cleanup fails safe.
`);
    process.exit(0);
  }

  const dryRun = args.has("--dry-run");
  const all = args.has("--all");
  const target = args.has("--target");
  const repos = args.has("--repos");
  const force = args.has("--force");

  runClean({ dryRun, all, target, repos, force });
}
