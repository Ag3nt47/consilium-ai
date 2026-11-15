#!/usr/bin/env node
// Blocks secrets and personal data from being committed or published.
//
// Scans every file git would publish (tracked plus staged/untracked-but-not-ignored) for API keys,
// tokens, private keys, credential files, personal home-directory paths and email addresses.
// Exits 1 and lists every finding, so it can gate a commit hook, a push, and CI.
//
//   node scripts/check-secrets.mjs            scan what git would publish
//   node scripts/check-secrets.mjs --staged   scan only staged changes (for a pre-commit hook)
//
// A line can be exempted by ending it with:  consilium-allow-secret-scan (<reason>)

import { execFileSync } from "node:child_process";
import { readFileSync, statSync } from "node:fs";

const RULES = [
  ["OpenAI API key", /\bsk-(?:proj-|svcacct-|admin-)?[A-Za-z0-9_-]{20,}/],
  ["Anthropic API key", /\bsk-ant-[A-Za-z0-9_-]{20,}/],
  ["GitHub token", /\b(?:gh[pousr]_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{40,})/],
  ["AWS access key", /\b(?:AKIA|ASIA)[A-Z0-9]{16}\b/],
  ["Google API key", /\bAIza[0-9A-Za-z_-]{35}\b/],
  ["Slack token", /\bxox[abprs]-[A-Za-z0-9-]{10,}/],
  ["Stripe live key", /\b(?:sk|rk)_live_[A-Za-z0-9]{20,}/],
  ["Private key block", /-----BEGIN (?:[A-Z]+ )?PRIVATE KEY-----/],
  ["JSON Web Token", /\beyJ[A-Za-z0-9_-]{10,}\.eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}/],
  ["Bearer token", /\bBearer\s+[A-Za-z0-9._~+/-]{30,}/],
  ["Password assignment", /\b(?:password|passwd|secret|api_?key|access_?token)\s*[:=]\s*["'][^"'\s]{8,}["']/i],
  ["Personal macOS home path", /\/Users\/(?!runner\b|Shared\b|you\b|me\b|example\b|username\b|USER\b|<)[A-Za-z0-9._-]+\//],
  ["Personal Linux home path", /\/home\/(?!runner\b|user\b|you\b|example\b|username\b|<)[A-Za-z0-9._-]+\//],
  ["Personal Windows home path", /[A-Za-z]:\\Users\\(?!Public\b|runneradmin\b|you\b|example\b|username\b|<)[A-Za-z0-9._-]+\\/],
  [
    "Email address",
    // (file names like icon@2x.png look like addresses; a "TLD" that is a file extension is skipped)
    /\b[A-Za-z0-9._%+-]+@(?!(?:example\.(?:com|org)|users\.noreply\.github\.com|anthropic\.com|openai\.com)\b)[A-Za-z0-9.-]+\.(?!(?:png|jpe?g|gif|svg|webp|ico|icns|js|mjs|ts|tsx|css|json|md|rs|html)\b)[A-Za-z]{2,}\b/,
  ],
];

// Files that must never be published, whatever their content.
const FORBIDDEN_FILES = [
  /(^|\/)\.env(\..+)?$/,
  /\.(pem|key|p12|p8|pfx|keystore|jks|mobileprovision)$/i,
  /(^|\/)(auth|credentials?|messaging|signal|keys|api-keys)[^/]*\.json$/i,
  /(^|\/)id_(rsa|ed25519|ecdsa)(\.pub)?$/,
  /\.jsonl$/, // conversation logs
];
const ALLOWED_FORBIDDEN = [/^\.env\.example$/];

// Generated or binary files that can't contain secrets in text form.
const SKIP = [/^package-lock\.json$/, /^src-tauri\/Cargo\.lock$/, /\.(png|ico|icns|gif|jpg|jpeg|webp|woff2?|ttf)$/i];
const ALLOW_MARK = "consilium-allow-secret-scan";

function git(args) {
  return execFileSync("git", args, { encoding: "utf8" }).split("\0").filter(Boolean);
}

const staged = process.argv.includes("--staged");
const files = staged
  ? git(["diff", "--cached", "--name-only", "--diff-filter=ACMR", "-z"])
  : [...new Set([...git(["ls-files", "-z"]), ...git(["ls-files", "--others", "--exclude-standard", "-z"])])];

const findings = [];
for (const file of files) {
  if (FORBIDDEN_FILES.some((r) => r.test(file)) && !ALLOWED_FORBIDDEN.some((r) => r.test(file))) {
    findings.push(`${file}: file type must never be committed`);
    continue;
  }
  if (SKIP.some((r) => r.test(file))) continue;
  let text;
  try {
    if (statSync(file).size > 2_000_000) continue;
    text = staged ? execFileSync("git", ["show", `:${file}`], { encoding: "utf8" }) : readFileSync(file, "utf8");
  } catch {
    continue; // deleted or unreadable
  }
  if (text.includes("\u0000")) continue; // binary
  text.split("\n").forEach((line, i) => {
    if (line.includes(ALLOW_MARK)) return;
    for (const [name, re] of RULES) {
      if (re.test(line)) findings.push(`${file}:${i + 1}: ${name}`);
    }
  });
}

if (findings.length) {
  console.error(`check-secrets: ${findings.length} problem(s) found. Nothing should be published until they are fixed:\n`);
  for (const f of findings) console.error("  " + f);
  console.error(`\nRemove the value (keep secrets in the OS keychain or the provider's own CLI), or, if it is a`);
  console.error(`harmless example, end the line with "${ALLOW_MARK} (reason)".`);
  process.exit(1);
}
console.log(`check-secrets: ${files.length} file(s) scanned, nothing found.`);
