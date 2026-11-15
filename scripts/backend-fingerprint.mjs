// The engine fingerprint, computed exactly as src-tauri/build.rs does (keep the two in step).
// Usage: node scripts/backend-fingerprint.mjs   (prints it)
import { readFileSync, readdirSync, statSync, existsSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = join(fileURLToPath(new URL(".", import.meta.url)), "..", "src-tauri");
const PARTS = ["src", "capabilities", "tauri.conf.json"];

function files(rel, out) {
  const abs = join(ROOT, rel);
  if (!existsSync(abs)) return;
  if (statSync(abs).isDirectory()) {
    for (const e of readdirSync(abs)) files(`${rel}/${e}`, out);
  } else out.push(rel);
}

export function fingerprint() {
  const list = [];
  for (const p of PARTS) files(p, list);
  // Rust sorts strings by bytes; so does this for the ASCII paths used here.
  list.sort((a, b) => (Buffer.compare(Buffer.from(a), Buffer.from(b))));
  let h = 0xcbf29ce484222325n;
  const feed = (buf) => {
    for (const b of buf) h = ((h ^ BigInt(b)) * 0x100000001b3n) & 0xffffffffffffffffn;
  };
  for (const name of list) {
    let body = readFileSync(join(ROOT, name));
    if (name === "tauri.conf.json") {
      const text = body.toString("utf8").replace(/\r\n/g, "\n");
      body = Buffer.from(text.split("\n").filter((l) => !l.trimStart().startsWith('"version"')).join("\n"));
    }
    feed(Buffer.from(name));
    feed([0]);
    feed(body);
    feed([0]);
  }
  return h.toString(16).padStart(16, "0");
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) console.log(fingerprint());
