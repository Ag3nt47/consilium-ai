import { spawnSync } from "node:child_process";
import { extname, isAbsolute, relative, resolve } from "node:path";
import { existsSync, realpathSync, statSync } from "node:fs";
import { parseSource, supportedSourceExtensions } from "./parse-source.mjs";

function run(program, args) {
  return spawnSync(program, args, { encoding: "utf8", timeout: 10_000, maxBuffer: 64 * 1024 });
}

const args = process.argv.slice(2);
const json = args[0] === "--json";
const files = [...new Set(json ? args.slice(1) : args)];
if (files.length === 0 || files.length > 16) {
  console.error("Usage: node scripts/local-preflight.mjs [--json] <file> [up to 16 files]");
  process.exit(2);
}

const rootResult = run("git", ["rev-parse", "--show-toplevel"]);
if (rootResult.status !== 0) {
  console.error("Run preflight from a Git worktree.");
  process.exit(2);
}

const root = realpathSync(rootResult.stdout.trim());
const paths = files.map((file) => {
  const absolute = resolve(file);
  const path = relative(root, existsSync(absolute) ? realpathSync(absolute) : absolute);
  if (!path || path === ".." || /^\.\.[\\/]/.test(path) || isAbsolute(path)) {
    console.error("Every file must be inside the current Git worktree.");
    process.exit(2);
  }
  if (existsSync(absolute) ? !statSync(absolute).isFile() :
    run("git", ["-C", root, "cat-file", "-e", "HEAD:" + path.split("\\").join("/")]).status !== 0) {
    console.error(`Expected a worktree file or a tracked deletion: ${JSON.stringify(file)}`);
    process.exit(2);
  }
  return { absolute, path };
});

const scope = paths.map(({ path }) => path);
const patchDiagnostics = [];
function recordPatchFailure(result) {
  const lines = (result.stdout ?? "").split(/\r?\n/).filter((line) => !line.startsWith("+") && line.trim());
  patchDiagnostics.push(...(lines.length ? lines.slice(0, 6) : ["Git could not complete the patch check."]));
}
const diff = run("git", ["--literal-pathspecs", "-C", root, "-c", "core.whitespace=cr-at-eol", "diff", "--no-ext-diff", "--check", "HEAD", "--", ...scope]);
if (diff.status !== 0) recordPatchFailure(diff);
const untracked = run("git", ["--literal-pathspecs", "-C", root, "ls-files", "--others", "-z", "--", ...scope]);
if (untracked.status !== 0) recordPatchFailure(untracked);
for (const path of (untracked.stdout ?? "").split("\0").filter(Boolean)) {
  const result = run("git", ["-C", root, "-c", "core.whitespace=cr-at-eol", "diff", "--no-index", "--no-ext-diff", "--check", "--", "/dev/null", path]);
  // A no-index comparison returns 1 for a clean difference.
  if (result.status !== 0 && result.status !== 1) recordPatchFailure(result);
}
const syntaxDiagnostics = [];
let checked = 0;
for (const { absolute, path } of paths) {
  if (!supportedSourceExtensions.has(extname(path).toLowerCase()) || !existsSync(absolute)) continue;
  checked++;
  try {
    parseSource(absolute);
  } catch (error) {
    const diagnostic = error.message.split(/\r?\n/).find((line) => line.trim());
    syntaxDiagnostics.push({ path, message: (diagnostic ?? "Syntax check failed.").slice(0, 240) });
  }
}
const failed = patchDiagnostics.length > 0 || syntaxDiagnostics.length > 0;
const patchErrors = patchDiagnostics.slice(0, 6).map((line) => line.slice(0, 240));
if (json) {
  console.log(JSON.stringify({ ok: !failed, syntaxChecked: checked, patchErrors, syntaxErrors: syntaxDiagnostics }));
} else {
  if (patchErrors.length) {
    console.error("Patch check failed.");
    for (const line of patchErrors) console.error(line);
  }
  for (const { path, message } of syntaxDiagnostics) console.error(`${JSON.stringify(path)}: ${message}`);
  console.log(`Patch check and ${checked} JavaScript or TypeScript syntax checks complete.`);
}
process.exitCode = failed ? 1 : 0;
