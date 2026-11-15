// Builds the window's page and stages it for Consilium's instant "Apply Now" (no restart).
// Only works when the engine (src-tauri) is unchanged since the installed build; otherwise it says
// so and the change has to go through `npm run update:stage` and Update Now.
// Usage: npm run update:ui   (Mac, Windows, Linux)
import { execSync } from "node:child_process";
import { cpSync, existsSync, readFileSync, renameSync, rmSync, writeFileSync, mkdirSync } from "node:fs";
import { homedir, platform } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { fingerprint, pageFingerprint } from "./backend-fingerprint.mjs";

const ROOT = join(fileURLToPath(new URL(".", import.meta.url)), "..");
const ID = "io.github.ag3nt47.consilium";
const DATA =
  process.env.CONSILIUM_DATA ||
  (platform() === "darwin"
    ? join(homedir(), "Library", "Application Support", ID)
    : platform() === "win32"
      ? join(process.env.APPDATA || join(homedir(), "AppData", "Roaming"), ID)
      : join(process.env.XDG_DATA_HOME || join(homedir(), ".local", "share"), ID));
const UI = join(DATA, "ui");

const backend = fingerprint();
const installedFile = join(UI, "backend.txt");
const installed = existsSync(installedFile) ? readFileSync(installedFile, "utf8").trim() : null;
if (installed && installed !== backend) {
  console.error(
    "The engine (src-tauri) has changed since the installed build, so this can't be applied instantly.\n" +
      "Run `npm run update:stage` and use Update Now instead.",
  );
  process.exit(2);
}

execSync("npm run build", { cwd: ROOT, stdio: "inherit" });

let commit = "local";
try {
  commit = execSync("git rev-parse --short HEAD", { cwd: ROOT }).toString().trim();
  if (execSync("git status --porcelain -- src index.html", { cwd: ROOT }).toString().trim()) commit += "+";
} catch {}
const pkg = JSON.parse(readFileSync(join(ROOT, "package.json"), "utf8"));
// The page's own content decides whether it's new: an unchanged rebuild is never offered.
const page = pageFingerprint();
const version = `${pkg.version}-${commit}`;

mkdirSync(UI, { recursive: true });
const tmp = join(UI, "staged.tmp");
rmSync(tmp, { recursive: true, force: true });
cpSync(join(ROOT, "dist"), tmp, { recursive: true });
writeFileSync(join(tmp, "ui.json"), JSON.stringify({ version, backend, page }));
rmSync(join(UI, "staged"), { recursive: true, force: true });
renameSync(tmp, join(UI, "staged"));
console.log(
  `Staged screen update ${version}.` +
    (installed ? " Click “Apply Now · instant” in Consilium." : " (Consilium hasn't run this engine yet; it will be offered once it has.)"),
);
