import { spawnSync } from "node:child_process";
import { extname, isAbsolute, relative, resolve } from "node:path";
import { existsSync, realpathSync, statSync } from "node:fs";
import { fileURLToPath } from "node:url";

const symbolMap = fileURLToPath(new URL("./symbol-map.mjs", import.meta.url));
const supported = new Set([".js", ".jsx", ".ts", ".tsx", ".mjs", ".cjs", ".mts", ".cts"]);

function run(program, args) {
  return spawnSync(program, args, { encoding: "utf8", timeout: 10_000, maxBuffer: 64 * 1024 });
}

const files = [...new Set(process.argv.slice(2))];
if (files.length === 0 || files.length > 16) {
  console.error("Usage: node scripts/local-preflight.mjs <file> [up to 16 files]");
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

const diff = run("git", ["--literal-pathspecs", "-C", root, "diff", "--no-ext-diff", "--check", "HEAD", "--", ...paths.map(({ path }) => path)]);
let failed = diff.status !== 0;
if (failed) {
  const diagnostics = (diff.stdout ?? "").split(/\r?\n/).filter((line) => !line.startsWith("+") && line.trim());
  console.error("Patch check failed.");
  for (const line of diagnostics.slice(0, 6)) console.error(line.slice(0, 240));
}
let checked = 0;
for (const { absolute, path } of paths) {
  if (!supported.has(extname(path).toLowerCase()) || !existsSync(absolute)) continue;
  checked++;
  const result = run(process.execPath, [symbolMap, absolute]);
  if (result.status !== 0) {
    failed = true;
    const diagnostic = result.stderr?.split(/\r?\n/).find((line) => line.trim());
    console.error(`${JSON.stringify(path)}: ${(diagnostic ?? result.error?.message ?? "Syntax check failed.").slice(0, 240)}`);
  }
}
console.log(`Patch check and ${checked} JavaScript or TypeScript syntax checks complete.`);
process.exitCode = failed ? 1 : 0;
