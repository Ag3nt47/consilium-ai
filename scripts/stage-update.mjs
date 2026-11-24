// Builds Consilium and stages it for the app's Update Now button, on macOS, Windows and Linux.
// The staged build appears with one rename, so Update Now never offers a half-copied build.
//   macOS:   <data>/update/Consilium.app           (app bundle only, no DMG window)
//   Windows: <data>/update/next/consilium.exe + VERSION
//   Linux:   <data>/update/next/consilium + Consilium.AppImage + VERSION
// Usage: npm run update:stage   (from the consilium-ai folder)
import { execFileSync, execSync } from "node:child_process";
import { randomBytes } from "node:crypto";
import { chmodSync, cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, renameSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
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
// Which build this is (see src-tauri/src/build_id.rs): the build number only grows along a branch,
// so linked computers can tell two builds of one version apart and never go backwards.
const gitOut = (args) => {
  try {
    return execFileSync("git", args, { cwd: ROOT, encoding: "utf8" }).trim();
  } catch {
    return "";
  }
};
const BUILD = {
  version: VERSION,
  number: Number(process.env.CONSILIUM_BUILD_NUMBER || gitOut(["rev-list", "--count", "HEAD"]) || 0),
  commit:
    process.env.CONSILIUM_COMMIT ||
    (() => {
      const sha = gitOut(["rev-parse", "--short=12", "HEAD"]);
      return sha && gitOut(["status", "--porcelain", "--untracked-files=no"]) ? `${sha}+dirty` : sha;
    })(),
};
const parseVersion = (v) => v.split(/[.+-]/).map(Number).filter((n) => Number.isInteger(n));
const newer = (a, b) => {
  const [x, y] = [parseVersion(a.version), parseVersion(b.version)];
  for (let i = 0; i < Math.max(x.length, y.length); i++) {
    if ((x[i] ?? -1) !== (y[i] ?? -1)) return (x[i] ?? -1) > (y[i] ?? -1);
  }
  return (a.number || 0) > (b.number || 0);
};
const label = (b) => (b.number ? `${b.version} #${b.number}${b.commit ? ` (${b.commit})` : ""}` : b.version);

// This Mac keeps Node and Rust outside the default PATH.
process.env.PATH = [join(homedir(), ".local", "node", "bin"), join(homedir(), ".cargo", "bin"), process.env.PATH].join(delimiter);

const start = Date.now();
const fresh = (p) => existsSync(p) && statSync(p).mtimeMs >= start - 2000;
const build = (args) => execSync(`npx tauri build ${args}`, { cwd: ROOT, stdio: "inherit", env: process.env });
const fail = (msg) => {
  console.error(msg);
  process.exit(1);
};

// macOS ties granted permissions (microphone, speech, automation) to an app's signature. An
// ad-hoc signature changes with every build, so each update asked for them all again. Signing
// with one self-signed certificate that lives in this computer's own keychain keeps the identity
// the same from one update to the next. The certificate never leaves this computer and is not
// trusted for anything else; its keychain is joined to the search list only while signing.
const SIGN_NAME = "Consilium Local Signing";
const SIGN_DIR = join(DATA, "signing");
const KEYCHAIN = join(SIGN_DIR, "consilium-signing.keychain-db");
const PASSFILE = join(SIGN_DIR, "keychain-password");
const sec = (args) => execFileSync("/usr/bin/security", args, { encoding: "utf8" });
const searchList = () =>
  sec(["list-keychains", "-d", "user"]).split("\n").map((l) => l.trim().replace(/^"|"$/g, "")).filter(Boolean);

function ensureSigningKeychain() {
  if (existsSync(KEYCHAIN) && existsSync(PASSFILE)) return readFileSync(PASSFILE, "utf8").trim();
  mkdirSync(SIGN_DIR, { recursive: true, mode: 0o700 });
  rmSync(KEYCHAIN, { force: true });
  const pass = randomBytes(24).toString("hex");
  const work = mkdtempSync(join(tmpdir(), "consilium-sign-"));
  try {
    const cnf = join(work, "c.cnf");
    writeFileSync(cnf, `[req]\ndistinguished_name=dn\nx509_extensions=ext\nprompt=no\n[dn]\nCN=${SIGN_NAME}\n[ext]\nbasicConstraints=critical,CA:false\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=critical,codeSigning\n`);
    const key = join(work, "k.pem"), crt = join(work, "c.pem"), p12 = join(work, "c.p12");
    execFileSync("/usr/bin/openssl", ["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-keyout", key, "-out", crt, "-days", "3650", "-config", cnf], { stdio: "ignore" });
    execFileSync("/usr/bin/openssl", ["pkcs12", "-export", "-inkey", key, "-in", crt, "-out", p12, "-passout", `pass:${pass}`], { stdio: "ignore" });
    sec(["create-keychain", "-p", pass, KEYCHAIN]);
    sec(["set-keychain-settings", KEYCHAIN]); // no auto-lock timeout
    sec(["unlock-keychain", "-p", pass, KEYCHAIN]);
    sec(["import", p12, "-k", KEYCHAIN, "-P", pass, "-T", "/usr/bin/codesign"]);
    sec(["set-key-partition-list", "-S", "apple-tool:,apple:", "-s", "-k", pass, KEYCHAIN]);
  } finally {
    rmSync(work, { recursive: true, force: true }); // the private key now lives only in the keychain
  }
  writeFileSync(PASSFILE, `${pass}\n`, { mode: 0o600 });
  chmodSync(PASSFILE, 0o600);
  return pass;
}

// execFileSync's error message repeats the whole command line, which for `security` can include
// the keychain password, so only the exit status or signal is ever shown.
const why = (e) => (e && typeof e.status === "number" ? `exit ${e.status}` : e?.signal ? `signal ${e.signal}` : "failed to run");

// The user's keychain search list is shared by every process and macOS has no lock for it, so
// the restore re-reads it and removes only the keychain this run added; a change made by another
// process meanwhile (such as an in-app update staging) is kept. Consilium's own staging also
// serialises its steps in src-tauri/src/mac_signing.rs.
function signStably(app) {
  let pass;
  try {
    pass = ensureSigningKeychain();
  } catch (e) {
    console.warn(`Couldn't set up stable signing (${why(e)}); keeping the ad-hoc signature.`);
    return;
  }
  let added = false;
  try {
    sec(["unlock-keychain", "-p", pass, KEYCHAIN]);
    const before = searchList();
    if (!before.includes(KEYCHAIN)) {
      sec(["list-keychains", "-d", "user", "-s", ...before, KEYCHAIN]);
      added = true;
    }
    execFileSync("/usr/bin/codesign", ["--force", "--deep", "--sign", SIGN_NAME, app], { stdio: "inherit" });
    console.log(`Signed with "${SIGN_NAME}", so macOS keeps this app's permissions across updates.`);
  } catch (e) {
    console.warn(`Stable signing failed (${why(e)}); keeping the ad-hoc signature.`);
    execFileSync("/usr/bin/codesign", ["--force", "--deep", "--sign", "-", app], { stdio: "inherit" });
  } finally {
    if (added) {
      try {
        sec(["list-keychains", "-d", "user", "-s", ...searchList().filter((k) => k !== KEYCHAIN)]);
      } catch (e) {
        console.warn(`Couldn't take the signing keychain off the search list (${why(e)}); remove it in Keychain Access.`);
      }
    }
    try { sec(["lock-keychain", KEYCHAIN]); } catch { /* already locked */ }
  }
}

mkdirSync(UPDATE, { recursive: true });

// Never replace a staged build with an older one: that is how an update gets overridden.
const STAMP = join(UPDATE, "BUILD.json");
try {
  const staged = JSON.parse(readFileSync(STAMP, "utf8"));
  const there = existsSync(join(UPDATE, OS === "darwin" ? "Consilium.app" : "next"));
  if (there && !newer(BUILD, staged) && !process.env.CONSILIUM_FORCE_STAGE) {
    fail(`A newer or equal build (${label(staged)}) is already staged; not replacing it with ${label(BUILD)}. Set CONSILIUM_FORCE_STAGE=1 to replace it anyway.`);
  }
} catch {
  /* nothing staged, or no stamp */
}
rmSync(STAMP, { force: true });

if (OS === "darwin") {
  build("--bundles app");
  const app = join(RELEASE, "bundle", "macos", "Consilium.app");
  if (!fresh(join(app, "Contents", "MacOS", "consilium"))) fail("The build produced no new app.");
  signStably(app);
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

// Written last, once the build is in place.
writeFileSync(STAMP, `${JSON.stringify(BUILD)}\n`);
console.log(`Staged ${label(BUILD)} for ${OS}. Click Update Now in Consilium.`);
