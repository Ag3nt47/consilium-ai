#!/usr/bin/env node

import { spawn } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { pathToFileURL } from "node:url";

const APP_ID = "io.github.ag3nt47.consilium";
const PORT = 8470;

export function defaultDataDir(platform = process.platform, env = process.env, home = os.homedir()) {
  if (env.CONSILIUM_DATA_DIR) return path.resolve(env.CONSILIUM_DATA_DIR);
  if (platform === "darwin") return path.posix.join(home, "Library", "Application Support", APP_ID);
  if (platform === "win32") {
    return path.win32.join(env.APPDATA || path.win32.join(home, "AppData", "Roaming"), APP_ID);
  }
  return path.posix.join(env.XDG_DATA_HOME || path.posix.join(home, ".local", "share"), APP_ID);
}

function jsonFile(file, fallback, required = false) {
  try {
    return { value: JSON.parse(readFileSync(file, "utf8")), error: null };
  } catch (error) {
    if (!existsSync(file)) {
      return { value: fallback, error: required ? `missing ${path.basename(file)}` : null };
    }
    return { value: fallback, error: `${path.basename(file)} is not valid JSON: ${error.message}` };
  }
}

export function inspectDurableState(dataDir) {
  const linkDir = path.join(dataDir, "link");
  const config = jsonFile(path.join(linkDir, "link.json"), {}, true);
  const outbox = jsonFile(path.join(linkDir, "outbox.json"), {});
  const mirrors = jsonFile(path.join(linkDir, "mirrors.json"), {});
  const problems = [config.error, outbox.error, mirrors.error].filter(Boolean);
  const warnings = [];
  const peers = Array.isArray(config.value?.peers) ? config.value.peers : [];
  const queues = outbox.value && typeof outbox.value === "object" && !Array.isArray(outbox.value)
    ? Object.values(outbox.value)
    : [];
  const queued = queues.reduce((total, queue) => total + (Array.isArray(queue) ? queue.length : 0), 0);
  if (queues.some((queue) => !Array.isArray(queue))) problems.push("outbox.json has an invalid peer queue");
  if (queued > 0) warnings.push(`${queued} packet(s) are queued for authenticated retry`);

  let inboxRecords = 0;
  let invalidInboxRecords = 0;
  const inboxPath = path.join(linkDir, "inbox.jsonl");
  if (existsSync(inboxPath)) {
    for (const line of readFileSync(inboxPath, "utf8").split(/\r?\n/).filter(Boolean)) {
      try {
        JSON.parse(line);
        inboxRecords += 1;
      } catch {
        invalidInboxRecords += 1;
      }
    }
    if (invalidInboxRecords > 0) {
      warnings.push(`${invalidInboxRecords} interrupted inbox record(s) will be repaired by the engine before the next append`);
    }
  }
  const map = mirrors.value?.map;
  const mirrorCount = map && typeof map === "object" && !Array.isArray(map) ? Object.keys(map).length : 0;

  return {
    dataDir,
    enabled: config.value?.enabled === true,
    localName: typeof config.value?.name === "string" ? config.value.name : null,
    peerCount: peers.length,
    peers: peers.map((peer) => ({
      name: typeof peer?.name === "string" ? peer.name : "linked computer",
      routeCount: Array.isArray(peer?.addrs) ? peer.addrs.length : 0,
    })),
    queued,
    inboxRecords,
    invalidInboxRecords,
    mirrorCount,
    problems,
    warnings,
  };
}

export function listenerUp(port = PORT, timeoutMs = 750) {
  return new Promise((resolve) => {
    const socket = net.createConnection({ host: "127.0.0.1", port });
    const finish = (up) => {
      socket.removeAllListeners();
      socket.destroy();
      resolve(up);
    };
    socket.setTimeout(timeoutMs, () => finish(false));
    socket.once("connect", () => finish(true));
    socket.once("error", () => finish(false));
  });
}

export function launchCommand(platform = process.platform, env = process.env) {
  if (platform === "darwin") return { command: "/usr/bin/open", args: ["-a", "Consilium"] };
  if (platform === "win32") {
    const candidates = [
      env.LOCALAPPDATA && path.win32.join(env.LOCALAPPDATA, "Consilium", "Consilium.exe"),
      env.LOCALAPPDATA && path.win32.join(env.LOCALAPPDATA, "Programs", "Consilium", "Consilium.exe"),
      env.ProgramFiles && path.win32.join(env.ProgramFiles, "Consilium", "Consilium.exe"),
    ].filter(Boolean);
    const command = candidates.find(existsSync);
    return command ? { command, args: [] } : null;
  }
  const candidates = [env.APPIMAGE, "/usr/bin/consilium", "/usr/local/bin/consilium"].filter(Boolean);
  const command = candidates.find(existsSync);
  return command ? { command, args: [] } : null;
}

export async function startIfNeeded(report, { platform = process.platform, env = process.env } = {}) {
  if (await listenerUp()) return { attempted: false, started: true, detail: "PeerLink listener is already running" };
  if (!report.enabled || report.peerCount === 0) {
    return { attempted: false, started: false, detail: "Sync is not enabled with a saved peer, so no app was started" };
  }
  const launch = launchCommand(platform, env);
  if (!launch) return { attempted: false, started: false, detail: "Could not locate the installed Consilium app" };
  const child = spawn(launch.command, launch.args, { detached: true, stdio: "ignore", windowsHide: true });
  child.unref();
  for (let attempt = 0; attempt < 20; attempt += 1) {
    await new Promise((resolve) => setTimeout(resolve, 500));
    if (await listenerUp()) return { attempted: true, started: true, detail: "Started Consilium and restored the PeerLink listener" };
  }
  return { attempted: true, started: false, detail: "Consilium was launched, but PeerLink did not start within 10 seconds" };
}

function args(argv) {
  const options = { dataDir: null, json: false, repair: false };
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === "--json") options.json = true;
    else if (arg === "--repair") options.repair = true;
    else if (arg === "--data") options.dataDir = argv[++index];
    else throw new Error(`Unknown option: ${arg}`);
  }
  return options;
}

async function main() {
  const options = args(process.argv.slice(2));
  const report = inspectDurableState(path.resolve(options.dataDir || defaultDataDir()));
  const listeningBefore = await listenerUp();
  const repair = options.repair ? await startIfNeeded(report) : null;
  const listening = repair?.started ?? listeningBefore;
  if (report.enabled && report.peerCount > 0 && !listening) {
    report.problems.push("Consilium is not listening on PeerLink port 8470");
  }
  const result = { ...report, listening, repair };
  if (options.json) {
    console.log(JSON.stringify(result, null, 2));
  } else {
    console.log(`Peer sync: ${result.problems.length === 0 ? "ready" : "needs attention"}`);
    console.log(`App listener: ${listening ? "running" : "not reachable"}`);
    console.log(`Saved peers: ${report.peerCount}; mirrored chats: ${report.mirrorCount}; queued packets: ${report.queued}`);
    for (const warning of report.warnings) console.log(`Warning: ${warning}`);
    for (const problem of report.problems) console.error(`Problem: ${problem}`);
    if (repair) console.log(`Repair: ${repair.detail}`);
  }
  process.exitCode = result.problems.length === 0 ? 0 : 1;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().catch((error) => {
    console.error(`peer-sync-doctor: ${error.message}`);
    process.exitCode = 2;
  });
}
