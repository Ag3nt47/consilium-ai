import { readFileSync } from "node:fs";

const manifest = JSON.parse(readFileSync(new URL("../docs/offline-skills.json", import.meta.url), "utf8"));
const [id, ...extra] = process.argv.slice(2);
const exporting = id === "--export";
const selected = exporting ? [...new Set(extra)] : id ? [id] : [];
const validSelection = selected.every((name) => Object.hasOwn(manifest, name));

if (!validSelection || (exporting ? !selected.length : extra.length)) {
  console.error("Usage: node scripts/offline-skills.mjs [skill-id | --export skill-id ...]");
  process.exitCode = 1;
} else if (selected.length) {
  const entries = selected.map((name) => {
    const { summary, command, guidance } = manifest[name];
    return [`## ${name}`, summary, command, guidance].join("\n");
  });
  console.log(entries.join("\n\n"));
} else {
  for (const [name, { summary }] of Object.entries(manifest)) console.log(`${name}  ${summary}`);
}
