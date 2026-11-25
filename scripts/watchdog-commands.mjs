#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { join } from "node:path";

const fromWindows = process.platform === "win32";
const executable = process.argv[2] ??
  (fromWindows && process.env.LOCALAPPDATA
    ? join(process.env.LOCALAPPDATA, "Consilium", "consilium.exe")
    : "consilium");

if (fromWindows) {
  try {
    if (!readFileSync(executable).includes("watchdog-signal commands")) {
      throw new Error();
    }
  } catch {
    console.error("Install a Consilium build that supports the Watchdog command list, or pass its executable path.");
    process.exit(1);
  }
}

const sent = spawnSync(executable, ["watchdog-signal", "commands"], {
  stdio: "inherit",
  windowsHide: true,
});
if (sent.error) console.error("Couldn't run the Consilium Watchdog command-list sender.");
process.exitCode = sent.status ?? 1;
