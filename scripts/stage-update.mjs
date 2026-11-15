// Builds Consilium and stages it for the app's Update Now button, on macOS, Windows and Linux.
// The staged build appears with one rename, so Update Now never offers a half-copied build.
//   macOS:   <data>/update/Consilium.app           (app bundle only, no DMG window)
//   Windows: <data>/update/next/consilium.exe + VERSION
//   Linux:   <data>/update/next/consilium + Consilium.AppImage + VERSION
// Usage: npm run update:stage   (from the consilium-ai folder)
import { execFileSync, execSync } from "node:child_process";
import { cpSync, existsSync, mkdirSync, readFileSync, readdirSync, renameSync, rmSync, statSync, writeFileSync } from "node:fs";
import { homedir, platform } from "node:os";
import { delimiter, join } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = join(fileURLToPath(new URL(".", import.meta.url)), "..");
const ID = "io.github.ag3nt47.consilium";
const OS = platform();
const DATA =
  process.env.CONSILIUM_DATA ||
  (OS === "darwin"
    ? join(homedir(), "Library", "Application Support", ID)
    : OS === "win32"
      ? join(process.env.APPDATA || join(homedir(), "AppData", "Roaming"), ID)
      : join(process.env.XDG_DATA_HOME || join(homedir(), ".local", "share"), ID));
const UPDATE = join(DATA, "update");
const RELEASE = join(process.env.CARGO_TARGET_DIR || join(ROOT, "src-tauri", "target"), "release");
const VERSION = JSON.parse(readFileSync(join(ROOT, "package.json"), "utf8")).version;

// This Mac keeps Node and Rust outside the default PATH.
process.env.PATH = [join(homedir(), ".local", "node", "bin"), join(homedir(), ".cargo", "bin"), process.env.PATH].join(delimiter);

const start = Date.now();
const fresh = (p) => existsSync(p) && statSync(p).mtimeMs >= start - 2000;
const build = (args) => execSync(`npx tauri build ${args}`, { cwd: ROOT, stdio: "inherit", env: process.env });
const fail = (msg) => {
  console.error(msg);
  process.exit(1);
};

mkdirSync(UPDATE, { recursive: true });

if (OS === "darwin") {
  build("--bundles app");
  const app = join(RELEASE, "bundle", "macos", "Consilium.app");
  if (!fresh(join(app, "Contents", "MacOS", "consilium"))) fail("The build produced no new app.");
  const tmp = join(UPDATE, "Consilium.app.tmp");
  rmSync(tmp, { recursive: true, force: true });
  execFileSync("ditto", [app, tmp]);
  rmSync(join(UPDATE, "Consilium.app"), { recursive: true, force: true });
  renameSync(tmp, join(UPDATE, "Consilium.app"));
} else {
  const exe = join(RELEASE, OS === "win32" ? "consilium.exe" : "consilium");
  let appimage = null;
  if (OS === "win32") {
    build("--no-bundle");
  } else {
    build("--bundles appimage");
    const dir = join(RELEASE, "bundle", "appimage");
    const found = existsSync(dir) ? readdirSync(dir).find((f) => f.endsWith(".AppImage") && f.includes(VERSION)) : null;
    if (found && fresh(join(dir, found))) appimage = join(dir, found);
  }
  if (!fresh(exe)) fail("The build produced no new program.");
  const tmp = join(UPDATE, "next.tmp");
  rmSync(tmp, { recursive: true, force: true });
  mkdirSync(tmp);
  cpSync(exe, join(tmp, OS === "win32" ? "consilium.exe" : "consilium"));
  if (appimage) cpSync(appimage, join(tmp, "Consilium.AppImage"));
  writeFileSync(join(tmp, "VERSION"), `${VERSION}\n`); // written last: the build is complete
  rmSync(join(UPDATE, "next"), { recursive: true, force: true });
  renameSync(tmp, join(UPDATE, "next"));
}

console.log(`Staged ${VERSION} for ${OS}. Click Update Now in Consilium.`);
