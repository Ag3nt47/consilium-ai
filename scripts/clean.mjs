#!/usr/bin/env node
// Cleans up temporary files, build workspaces, staging logs, and leftover artifacts
// so the host computer does not get bogged down or run out of disk space.
//
// Usage:
//   node scripts/clean.mjs            Safe clean: idle temp workspaces, logs, test artifacts, dist/
//   node scripts/clean.mjs --all      Deep clean: includes src-tauri/target build cache and cached repos
//   node scripts/clean.mjs --target   Cleans src-tauri/target Rust build cache
//   node scripts/clean.mjs --repos    Cleans cached GitHub repo shallow clones
//   node scripts/clean.mjs --force    Clean immediately without requiring minimum idle age
//   node scripts/clean.mjs --dry-run  Lists files and bytes to clean without deleting

import { existsSync, readdirSync, rmSync, statSync } from "node:fs";
import { homedir, platform, tmpdir } from "node:os";
import { isAbsolute, join, resolve } from "node:path";
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

// Patterns in temporary directories that belong to Consilium builds, tests, and stages.
const TEMP_FILE_PATTERNS = [
  /^c0\d+/, // e.g. /tmp/c0311, /tmp/c0310 staging workspaces
  /^stage.*\.log$/,
  /^stage.*\.sh$/,
  /^stage-ui-.*\.log$/,
  /^stage-ui-.*\.sh$/,
  /^consilium-/,
  /^reaper-\d+/,
];

// Default idle age threshold (30 minutes) to avoid deleting active in-flight builds or tests.
const DEFAULT_MIN_AGE_MS = 30 * 60 * 1000;

function isPidAlive(pid) {
  if (typeof pid !== "number" || Number.isNaN(pid) || pid <= 0) return false;
  try {
    process.kill(pid, 0);
    return true;
  } catch (e) {
    return e && e.code === "EPERM";
  }
}

function getDirSize(path) {
  let bytes = 0;
  try {
    const st = statSync(path);
    if (!st.isDirectory()) return st.size;
    bytes += st.size;
    const entries = readdirSync(path, { withFileTypes: true });
    for (const entry of entries) {
      const full = join(path, entry.name);
      try {
        if (entry.isDirectory()) {
          bytes += getDirSize(full);
        } else {
          bytes += statSync(full).size;
        }
      } catch {}
    }
  } catch {}
  return bytes;
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
  } = opts;

  const rootDir = customRootDir || ROOT;
  const dataDir = customDataDir || DATA;
  const targets = [];
  const now = Date.now();

  const isIdle = (fullPath) => {
    if (force || minAgeMs <= 0) return true;
    try {
      const st = statSync(fullPath);
      return now - st.mtimeMs >= minAgeMs;
    } catch {
      return false;
    }
  };

  // 1. Temporary directories (system tmpdir and /tmp on Unix).
  const tmpDirs = customTmpDirs || (OS === "win32" ? [tmpdir()] : Array.from(new Set([tmpdir(), "/tmp"])));
  for (const dir of tmpDirs) {
    if (!existsSync(dir)) continue;
    try {
      const entries = readdirSync(dir);
      for (const name of entries) {
        if (TEMP_FILE_PATTERNS.some((re) => re.test(name))) {
          const full = join(dir, name);
          if (isIdle(full)) {
            targets.push({ path: full, category: "temporary artifact", bytes: getDirSize(full) });
          }
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
            if (isIdle(full)) {
              targets.push({ path: full, category: "interrupted update", bytes: getDirSize(full) });
            }
          }
        }
      } catch {}
    }

    // Stale agent slots (dead PIDs)
    const slotsDir = join(dataDir, "agent-slots");
    if (existsSync(slotsDir)) {
      try {
        for (const name of readdirSync(slotsDir)) {
          const match = name.match(/(\d+)/);
          const pid = match ? Number.parseInt(match[1], 10) : null;
          if (pid && !isPidAlive(pid)) {
            const full = join(slotsDir, name);
            targets.push({ path: full, category: "stale slot lock", bytes: getDirSize(full) });
          }
        }
      } catch {}
    }

    // Cached ingested repositories (cleaned when --all or --repos is requested)
    if (all || repos) {
      const reposDir = join(dataDir, "repos");
      if (existsSync(reposDir)) {
        try {
          for (const name of readdirSync(reposDir)) {
            const full = join(reposDir, name);
            if (isIdle(full)) {
              targets.push({ path: full, category: "cached ingested repo", bytes: getDirSize(full) });
            }
          }
        } catch {}
      }
    }
  }

  // 3. Project repository artifacts.
  const distDir = join(rootDir, "dist");
  if (existsSync(distDir)) {
    targets.push({ path: distDir, category: "frontend build (dist/)", bytes: getDirSize(distDir) });
  }

  const viteCache = join(rootDir, "node_modules", ".vite");
  if (existsSync(viteCache)) {
    targets.push({ path: viteCache, category: "vite cache", bytes: getDirSize(viteCache) });
  }

  // Cargo target directory (cleaned when --all or --target is specified).
  if (all || target) {
    const targetDir = join(rootDir, "src-tauri", "target");
    if (existsSync(targetDir)) {
      targets.push({ path: targetDir, category: "Rust build cache (target/)", bytes: getDirSize(targetDir) });
    }
  }

  return targets;
}

export function runClean(opts = {}) {
  const { dryRun = false } = opts;
  const targets = findCleanableTargets(opts);
  let totalBytes = 0;
  let cleanedCount = 0;

  for (const item of targets) {
    totalBytes += item.bytes;
    if (dryRun) {
      console.log(`[dry-run] Would remove: ${item.path} (${item.category}, ${formatBytes(item.bytes)})`);
    } else {
      try {
        rmSync(item.path, { recursive: true, force: true });
        cleanedCount++;
        console.log(`Removed: ${item.path} (${item.category}, ${formatBytes(item.bytes)})`);
      } catch (err) {
        console.warn(`Could not remove ${item.path}: ${err.message}`);
      }
    }
  }

  const prefix = dryRun ? "[dry-run] Total cleanable:" : "Clean complete:";
  console.log(`${prefix} ${cleanedCount || targets.length} items, reclaimed ${formatBytes(totalBytes)}.`);
  return { count: cleanedCount, totalBytes, targets };
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
  --force     Clean immediately without requiring 30-minute idle age
  -h, --help  Show this help message
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
