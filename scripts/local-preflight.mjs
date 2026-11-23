import { spawnSync } from "node:child_process";
import { extname, isAbsolute, relative, resolve } from "node:path";
import { existsSync, lstatSync, realpathSync, statSync } from "node:fs";
import { parseSource, supportedSourceExtensions } from "./parse-source.mjs";

const MAX_FILES = 16;

function run(program, args) {
  return spawnSync(program, args, { encoding: "utf8", timeout: 10_000, maxBuffer: 64 * 1024 });
}

function stop(message) {
  console.error(message);
  process.exit(2);
}

function gitPaths(result) {
  if (result.status !== 0) stop("Git could not list the changed files in this worktree.");
  return result.stdout.split("\0").filter(Boolean);
}

function changedSources(root, diffBases) {
  const scope = [".", ":(exclude,attr:linguist-generated)"];
  const tracked = diffBases.flatMap((base) => gitPaths(run("git", [
    "-C", root, "diff", "--name-only", "--no-renames", "--no-ext-diff", "--no-textconv",
    "--diff-filter=ACMRT", "-z", ...base, "--", ...scope,
  ])));
  const untracked = gitPaths(run("git", [
    "-C", root, "ls-files", "--others", "--exclude-standard", "-z", "--", ...scope,
  ]));
  return [...new Set([...tracked, ...untracked])]
    .filter((path) => supportedSourceExtensions.has(extname(path).toLowerCase()))
    .map((path) => resolve(root, path))
    .filter((path) => lstatSync(path, { throwIfNoEntry: false })?.isFile());
}

const args = process.argv.slice(2);
const json = args[0] === "--json";
const selection = json ? args.slice(1) : args;
const auto = selection[0] === "--changed";
const explicitFiles = [...new Set(selection)];
if (!selection.length || (auto && selection.length !== 1) || explicitFiles.length > MAX_FILES) {
  stop("Usage: node scripts/local-preflight.mjs [--json] (--changed | <file> [up to 16 files])");
}

const rootResult = run("git", ["rev-parse", "--show-toplevel"]);
if (rootResult.status !== 0) stop("Run preflight from a Git worktree.");

const root = realpathSync(rootResult.stdout.trim());
const head = run("git", ["-C", root, "rev-parse", "--verify", "--quiet", "HEAD"]);
if (head.status !== 0 && head.status !== 1) stop("Git could not inspect HEAD.");
const diffBases = head.status === 0 ? [["HEAD"]] : [["--cached"], []];
const files = auto ? changedSources(root, diffBases) : explicitFiles;
if (files.length > MAX_FILES) {
  stop(`Found ${files.length} changed source files. Pass up to ${MAX_FILES} files from the current task explicitly.`);
}
const paths = files.map((file) => {
  const absolute = resolve(file);
  const path = relative(root, existsSync(absolute) ? realpathSync(absolute) : absolute);
  if (!path || path === ".." || /^\.\.[\\/]/.test(path) || isAbsolute(path)) {
    stop("Every file must be inside the current Git worktree.");
  }
  if (existsSync(absolute) ? !statSync(absolute).isFile() :
    run("git", ["-C", root, "cat-file", "-e", "HEAD:" + path.split("\\").join("/")]).status !== 0) {
    stop(`Expected a worktree file or a tracked deletion: ${JSON.stringify(file)}`);
  }
  return { absolute, path };
});

const scope = paths.map(({ path }) => path);
const patchDiagnostics = [];
function recordPatchFailure(result) {
  const lines = (result.stdout ?? "").split(/\r?\n/).filter((line) => !line.startsWith("+") && line.trim());
  patchDiagnostics.push(...(lines.length ? lines.slice(0, 6) : ["Git could not complete the patch check."]));
}
if (scope.length) {
  for (const base of diffBases) {
    const diff = run("git", [
      "--literal-pathspecs", "-C", root, "-c", "core.whitespace=cr-at-eol", "diff",
      "--no-ext-diff", "--no-textconv", "--check", ...base, "--", ...scope,
    ]);
    if (diff.status !== 0) recordPatchFailure(diff);
  }
  const untracked = run("git", ["--literal-pathspecs", "-C", root, "ls-files", "--others", "-z", "--", ...scope]);
  if (untracked.status !== 0) recordPatchFailure(untracked);
  for (const path of (untracked.stdout ?? "").split("\0").filter(Boolean)) {
    const result = run("git", [
      "-C", root, "-c", "core.whitespace=cr-at-eol", "diff", "--no-index",
      "--no-ext-diff", "--no-textconv", "--check", "--", "/dev/null", `./${path}`,
    ]);
    // A no-index comparison returns 1 for a clean difference.
    if (result.status !== 0 && result.status !== 1) recordPatchFailure(result);
  }
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
const patchErrors = [...new Set(patchDiagnostics)].slice(0, 6).map((line) => line.slice(0, 240));
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
