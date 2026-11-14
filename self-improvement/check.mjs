#!/usr/bin/env node
// Cheap, local self-check for Consilium. Never calls an AI. See README.md in this folder.
//
//   node self-improvement/check.mjs          run checks, add new findings to ledger.json
//   node self-improvement/check.mjs --dry    run checks, change nothing
//
// Exit code: 0 when there's nothing new, 2 when new findings were logged, 1 on an internal error.

import { spawnSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = join(HERE, "..");
const LEDGER = join(HERE, "ledger.json");
const dry = process.argv.includes("--dry");
const today = new Date().toISOString().slice(0, 10);

function run(cmd, args) {
  const r = spawnSync(cmd, args, { cwd: ROOT, encoding: "utf8", timeout: 120000, shell: false });
  return { ok: r.status === 0, out: r.stdout || "", err: r.stderr || "", missing: r.error?.code === "ENOENT" };
}

// Strip anything that looks personal or secret before it reaches the ledger.
function redact(s) {
  return String(s)
    .replace(/\/(Users|home)\/[^/\s]+/g, "/$1/<user>")
    .replace(/[A-Za-z]:\\Users\\[^\\\s]+/g, "C:\\Users\\<user>")
    .replace(/\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}\b/g, "<email>")
    .replace(/\b(sk-[A-Za-z0-9_-]{8})[A-Za-z0-9_-]+/g, "$1…")
    .slice(0, 500);
}

const findings = [];
const add = (f) => findings.push({ status: "open", found: today, fix: null, ...f, evidence: redact(f.evidence) });

// 1. Privacy gate.
const gate = run(process.execPath, ["scripts/check-secrets.mjs"]);
if (!gate.ok) {
  for (const line of gate.err.split("\n").filter((l) => /^\s+\S+:\d+: |file type must never/.test(l))) {
    const where = line.trim();
    add({ id: `privacy-${where.replace(/[^A-Za-z0-9]+/g, "-").slice(0, 80)}`, severity: "critical", source: "privacy gate", title: "Secret or personal data in the repository", evidence: where });
  }
}

// 2. npm audit (production dependencies).
const audit = run(process.platform === "win32" ? "npm.cmd" : "npm", ["audit", "--omit=dev", "--json"]);
if (!audit.missing) {
  try {
    const vulns = JSON.parse(audit.out || "{}").vulnerabilities || {};
    for (const [name, v] of Object.entries(vulns)) {
      if (!["high", "critical", "moderate"].includes(v.severity)) continue;
      const advisory = (v.via || []).find((x) => typeof x === "object");
      add({
        id: `npm-audit-${name}-${advisory?.source ?? v.severity}`,
        severity: v.severity,
        source: "npm audit",
        title: `${name}: ${advisory?.title ?? "vulnerable dependency"}`,
        evidence: `${name} ${v.range ?? ""}; fix available: ${v.fixAvailable ? "yes" : "no"}`,
      });
    }
  } catch {
    /* audit output unreadable (offline): skip this check */
  }
}

// 3. GitHub security alerts, if the GitHub CLI is installed and signed in.
const repo = "Ag3nt47/consilium-ai";
const alertSources = [
  ["dependabot", `repos/${repo}/dependabot/alerts?state=open`, (a) => ({ id: `dependabot-${a.number}`, severity: a.security_advisory?.severity ?? "unknown", title: a.security_advisory?.summary ?? "Dependabot alert", evidence: `${a.dependency?.package?.name ?? "?"} (${a.html_url ?? ""})` })],
  ["code scanning", `repos/${repo}/code-scanning/alerts?state=open`, (a) => ({ id: `codeql-${a.number}`, severity: a.rule?.security_severity_level ?? a.rule?.severity ?? "unknown", title: a.rule?.description ?? "Code scanning alert", evidence: `${a.most_recent_instance?.location?.path ?? "?"}:${a.most_recent_instance?.location?.start_line ?? "?"}` })],
  ["secret scanning", `repos/${repo}/secret-scanning/alerts?state=open`, (a) => ({ id: `secret-${a.number}`, severity: "critical", title: `Exposed secret: ${a.secret_type_display_name ?? a.secret_type}`, evidence: a.html_url ?? "" })],
];
for (const [source, path, map] of alertSources) {
  const r = run("gh", ["api", path]);
  if (r.missing || !r.ok) continue; // not installed, not signed in, or feature off
  try {
    for (const a of JSON.parse(r.out)) add({ source, ...map(a) });
  } catch {
    /* unexpected output: skip */
  }
}

// Merge into the ledger, skipping anything already logged.
let ledger = [];
try {
  ledger = JSON.parse(readFileSync(LEDGER, "utf8"));
} catch {
  ledger = [];
}
const known = new Set(ledger.map((e) => e.id));
const fresh = findings.filter((f) => !known.has(f.id) && (known.add(f.id), true));

if (!fresh.length) {
  console.log("self-check: nothing new. No AI needed.");
  process.exit(0);
}
console.log(`self-check: ${fresh.length} new finding(s):`);
for (const f of fresh) console.log(`  [${f.severity}] ${f.id}: ${f.title}`);
if (!dry) {
  writeFileSync(LEDGER, JSON.stringify([...ledger, ...fresh], null, 2) + "\n");
  console.log("Logged in self-improvement/ledger.json. Propose a fix on a branch named fix/<id>.");
}
process.exit(2);
