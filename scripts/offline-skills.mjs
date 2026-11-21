import { readFileSync } from "node:fs";

const manifest = JSON.parse(readFileSync(new URL("../docs/offline-skills.json", import.meta.url), "utf8"));
const [id, ...extra] = process.argv.slice(2);
if (extra.length || (id && !Object.hasOwn(manifest, id))) {
  console.error("Unknown skill. Run node scripts/offline-skills.mjs to list available skills.");
  process.exitCode = 1;
} else if (id) {
  const { summary, command, guidance } = manifest[id];
  console.log([summary, command, guidance].join("\n"));
} else {
  for (const [name, { summary }] of Object.entries(manifest)) console.log(`${name}  ${summary}`);
}
