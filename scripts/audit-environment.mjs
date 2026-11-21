import { spawnSync } from "node:child_process";
import { constants, accessSync, readFileSync, readdirSync, statSync } from "node:fs";
import { homedir } from "node:os";
import { delimiter, isAbsolute, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../", import.meta.url));
const windows = process.platform === "win32";
const extensions = windows ? (process.env.PATHEXT || ".EXE;.CMD;.BAT;.COM").split(";") : [""];
const searchPaths = (process.env.PATH || "").split(delimiter).filter(isAbsolute);

function onPath(name) {
  return searchPaths.some((directory) => extensions.some((extension) => {
    const candidate = join(directory, name + extension);
    try {
      accessSync(candidate, windows ? constants.F_OK : constants.X_OK);
      return statSync(candidate).isFile();
    } catch {
      return false;
    }
  }));
}

function readJson(name) {
  return JSON.parse(readFileSync(join(root, name), "utf8"));
}

const git = spawnSync("git", ["--no-optional-locks", "status", "--porcelain=v1", "--branch", "--untracked-files=no"], {
  cwd: root,
  encoding: "utf8",
  timeout: 5000,
  maxBuffer: 128 * 1024,
  windowsHide: true,
});
if (git.status === 0) {
  const [branch, ...changes] = git.stdout.trimEnd().split("\n");
  console.log(`Repository: ${branch.replace(/^## /, "").trim()} (${changes.length} tracked changes)`);
} else {
  console.log(`Repository: status unavailable (${git.error?.code || `exit ${git.status}`})`);
}

try {
  const manifest = readJson("package.json");
  const lock = readJson("package-lock.json");
  const dependencies = { ...manifest.dependencies, ...manifest.devDependencies };
  for (const [name, requested] of Object.entries(dependencies)) {
    const declared = lock.packages?.[""]?.dependencies?.[name] ?? lock.packages?.[""]?.devDependencies?.[name];
    const locked = lock.packages?.[`node_modules/${name}`]?.version;
    let installed;
    try { installed = readJson(`node_modules/${name}/package.json`).version; } catch {}
    const status = declared !== requested || !locked ? "lockfile mismatch"
      : installed === locked ? `installed ${installed}`
        : installed ? `installed ${installed}, lockfile ${locked}` : `missing locally, lockfile ${locked}`;
    console.log(`Dependency ${name}: ${status}`);
  }
} catch {
  console.log("Dependencies: package manifest or lockfile could not be read.");
}

for (const name of ["git", "npm", "cargo", "rustc", "claude", "codex", "gemini", "ollama", "llama-server"]) {
  console.log(`Tool ${name}: ${onPath(name) ? "on PATH" : "not found on PATH"}`);
}

const manifests = join(process.env.OLLAMA_MODELS || join(homedir(), ".ollama", "models"), "manifests");
try {
  const registries = readdirSync(manifests, { withFileTypes: true }).filter((entry) => entry.isDirectory()).length;
  console.log(`Ollama inventory: local manifest directory found (${registries} registry directories).`);
} catch (error) {
  console.log(`Ollama inventory: ${error.code === "ENOENT" ? "no local manifest directory found" : "manifest directory unreadable"}.`);
}
