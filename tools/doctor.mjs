#!/usr/bin/env node
// Agent Setup Kit: checks which AI command-line tools are installed and signed in, and says exactly
// what to run for anything missing. It never reads, prints or sends credentials; it only runs each
// tool's own `--version` and sign-in status commands.
//
//   node tools/doctor.mjs           report
//   node tools/doctor.mjs --json    machine-readable report (for an AI to read and act on)
//
// Usable by a person, or by one AI helping to set up another: it reports the next step as a
// command for the person to approve and run. It doesn't install anything itself.

import { spawnSync } from "node:child_process";
import { existsSync, accessSync, constants, readFileSync } from "node:fs";
import { homedir, platform } from "node:os";
import { join, delimiter, dirname } from "node:path";

const WIN = platform() === "win32";
const home = homedir();

function findExe(name, extra) {
  const dirs = (process.env.PATH || "").split(delimiter).concat(
    [".local/bin", ".local/node/bin", ".npm-global/bin", ".volta/bin"].map((d) => join(home, d)),
    ["/usr/local/bin", "/opt/homebrew/bin"],
    WIN && process.env.APPDATA ? [join(process.env.APPDATA, "npm")] : [],
  );
  const names = WIN ? [`${name}.exe`, `${name}.cmd`] : [name];
  for (const p of [...extra, ...dirs.flatMap((d) => names.map((n) => join(d, n)))]) {
    try {
      if (existsSync(p)) {
        accessSync(p, constants.X_OK);
        return p;
      }
    } catch {
      /* not executable */
    }
  }
  return null;
}

// Some tools print status on stderr, so both streams are read.
// The tool's own folder goes first on PATH, so Node-based CLIs (`#!/usr/bin/env node`) find Node.
function run(exe, args) {
  const env = { ...process.env, PATH: [dirname(exe), process.env.PATH || ""].join(delimiter) };
  const r = spawnSync(exe, args, { encoding: "utf8", timeout: 20000, stdio: ["ignore", "pipe", "pipe"], env });
  return `${r.stdout || ""}${r.stderr || ""}`.trim();
}

const agents = [
  {
    id: "gpt",
    name: "ChatGPT",
    tool: "Codex CLI",
    exe: findExe("codex", [
      "/Applications/ChatGPT.app/Contents/Resources/codex-cli/bin/codex",
      join(home, "Applications/ChatGPT.app/Contents/Resources/codex-cli/bin/codex"),
    ]),
    signedIn: (exe) => /Logged in/.test(run(exe, ["login", "status"])),
    install: "npm install -g @openai/codex   (or install the ChatGPT desktop app, which includes it on macOS)",
    signin: (exe) => `"${exe}" login`,
  },
  {
    id: "claude",
    name: "Claude",
    tool: "Claude Code CLI",
    exe: findExe("claude", [join(home, ".claude/local/claude")]),
    signedIn: (exe) => {
      try {
        return JSON.parse(run(exe, ["auth", "status"])).loggedIn === true;
      } catch {
        return false;
      }
    },
    install: WIN ? "irm https://claude.ai/install.ps1 | iex" : "curl -fsSL https://claude.ai/install.sh | bash",
    signin: (exe) => `"${exe}" auth login`,
  },
  {
    id: "gemini",
    name: "Gemini",
    tool: "Gemini CLI",
    exe: findExe("gemini", []),
    // The sign-in method lives in ~/.gemini/settings.json; only the presence of the credentials file
    // is checked, never its contents.
    signedIn: () => {
      try {
        const s = JSON.parse(readFileSync(join(home, ".gemini", "settings.json"), "utf8"));
        const method = s?.security?.auth?.selectedType ?? s?.selectedAuthType ?? "";
        if (method === "oauth-personal") return existsSync(join(home, ".gemini", "oauth_creds.json"));
        if (method === "gemini-api-key") return !!process.env.GEMINI_API_KEY;
        return !!method;
      } catch {
        return false;
      }
    },
    install: "npm install -g @google/gemini-cli",
    signin: (exe) => `"${exe}"   (choose "Sign in with Google", then type /quit)`,
  },
];

const report = agents.map((a) => {
  const r = { id: a.id, name: a.name, tool: a.tool, installed: !!a.exe, version: null, signedIn: false, next: null };
  if (!a.exe) {
    r.next = a.install;
    return r;
  }
  r.version = run(a.exe, ["--version"]).split("\n")[0] || null;
  r.signedIn = a.signedIn(a.exe);
  if (!r.signedIn) r.next = a.signin(a.exe);
  return r;
});

if (process.argv.includes("--json")) {
  console.log(JSON.stringify({ agents: report }, null, 2));
} else {
  console.log("Consilium doctor\n");
  for (const r of report) {
    const status = !r.installed ? "not installed" : r.signedIn ? "ready" : "installed, not signed in";
    console.log(`  ${r.signedIn ? "✓" : "✗"} ${r.name.padEnd(8)} ${r.tool.padEnd(16)} ${status}${r.version ? `  (${r.version})` : ""}`);
    if (r.next) console.log(`      next: ${r.next}`);
  }
  const ready = report.filter((r) => r.signedIn).length;
  console.log(`\n${ready} of ${report.length} AIs ready.${ready ? "" : " Consilium needs at least one."}`);
}
process.exitCode = report.some((r) => r.signedIn) ? 0 : 1;
