// Runs the secret scanner against a throwaway git repository full of planted secrets.
import { test } from "node:test";
import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { mkdtempSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const SCANNER = resolve("scripts/check-secrets.mjs");
// Run inside a git hook, GIT_DIR and GIT_INDEX_FILE point at the real repository: `git init` here
// would then re-initialise that repository from outside its working tree and switch core.bare on.
const env = Object.fromEntries(Object.entries(process.env).filter(([k]) => !k.startsWith("GIT_")));

function repoWith(files) {
  const dir = mkdtempSync(join(tmpdir(), "consilium-scan-"));
  execFileSync("git", ["init", "-q"], { cwd: dir, env });
  for (const [name, body] of Object.entries(files)) writeFileSync(join(dir, name), body);
  return dir;
}

function scan(dir) {
  return spawnSync(process.execPath, [SCANNER], { cwd: dir, env, encoding: "utf8" });
}

// Test values are assembled at runtime so this file itself never contains a real-looking secret.
const fake = {
  openai: "sk-" + "proj-" + "A".repeat(30),
  anthropic: "sk-" + "ant-" + "B".repeat(30),
  github: "gh" + "p_" + "C".repeat(36),
  home: "/Us" + "ers/somebody/secret.txt",
  email: "someone" + "@" + "gmail.com",
  pem: "-----BEGIN " + "PRIVATE KEY-----",
};

test("finds planted keys, tokens, personal paths and emails", () => {
  const dir = repoWith({
    "a.txt": `key=${fake.openai}\n${fake.anthropic}\n${fake.github}\n`,
    "b.md": `see ${fake.home}\ncontact ${fake.email}\n${fake.pem}\n`,
  });
  const r = scan(dir);
  rmSync(dir, { recursive: true, force: true });
  assert.equal(r.status, 1);
  for (const what of ["OpenAI API key", "Anthropic API key", "GitHub token", "Personal macOS home path", "Email address", "Private key block"]) {
    assert.match(r.stderr, new RegExp(what), what);
  }
});

test("blocks credential and conversation files by name", () => {
  const dir = repoWith({ ".env": "X=1\n", "auth.json": "{}\n", "chat.jsonl": "{}\n", ".env.example": "X=\n" });
  const r = scan(dir);
  rmSync(dir, { recursive: true, force: true });
  assert.equal(r.status, 1);
  assert.match(r.stderr, /\.env: file type/);
  assert.match(r.stderr, /auth\.json: file type/);
  assert.match(r.stderr, /chat\.jsonl: file type/);
  assert.doesNotMatch(r.stderr, /\.env\.example/);
});

test("clean files pass, and allowed examples are respected", () => {
  const dir = repoWith({
    "ok.md": "Use the sign-in from the CLI. Contact security@example.com.\nCI runs in /Users/runner/work.\nicons/128x128@2x.png\n",
    "ex.md": `Example: ${fake.openai} consilium-allow-secret-scan (docs example)\n`,
  });
  const r = scan(dir);
  rmSync(dir, { recursive: true, force: true });
  assert.equal(r.status, 0, r.stderr);
});
