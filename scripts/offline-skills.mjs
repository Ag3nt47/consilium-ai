import { constants, closeSync, fstatSync, openSync, readdirSync, readFileSync, readSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

const manifest = JSON.parse(readFileSync(new URL("../docs/offline-skills.json", import.meta.url), "utf8"));
const runbookDir = join(homedir(), ".consilium", "skills");
const runbookName = /^[a-z0-9][a-z0-9-]{0,63}\.md$/;
const limit = 16 * 1024;

function listRunbooks() {
  try {
    return readdirSync(runbookDir, { withFileTypes: true })
      .filter((entry) => entry.isFile() && runbookName.test(entry.name))
      .map((entry) => entry.name)
      .sort();
  } catch (error) {
    if (error.code === "ENOENT") return [];
    throw error;
  }
}

function readRunbook(name) {
  const fd = openSync(join(runbookDir, name), constants.O_RDONLY | constants.O_NONBLOCK);
  try {
    const info = fstatSync(fd);
    if (!info.isFile() || info.size > limit) throw new Error("Runbook must be a regular Markdown file up to 16 KiB.");
    const buffer = Buffer.alloc(limit + 1);
    let offset = 0;
    while (offset < buffer.length) {
      const read = readSync(fd, buffer, offset, buffer.length - offset, null);
      if (read === 0) break;
      offset += read;
    }
    if (offset > limit) throw new Error("Runbook exceeds 16 KiB.");
    return buffer.toString("utf8", 0, offset).trim();
  } finally {
    closeSync(fd);
  }
}

try {
  const [id, ...extra] = process.argv.slice(2);
  const exporting = id === "--export";
  const selected = exporting ? [...new Set(extra)] : id ? [id] : [];
  if (exporting ? !selected.length : extra.length) {
    throw new Error("Usage: node scripts/offline-skills.mjs [skill-id | --export skill-id ...]");
  }

  const runbooks = new Map(listRunbooks().map((name) => [`runbook:${name.slice(0, -3)}`, name]));
  if (selected.length) {
    const entries = selected.map((name) => {
      if (Object.hasOwn(manifest, name)) {
        const { summary, command, guidance } = manifest[name];
        return [`## ${name}`, summary, command, guidance].join("\n");
      }
      const runbook = runbooks.get(name);
      if (!runbook) throw new Error(`Unknown offline skill: ${JSON.stringify(name)}`);
      return [`## ${name}`, readRunbook(runbook)].join("\n");
    });
    console.log(entries.join("\n\n"));
  } else {
    for (const [name, { summary }] of Object.entries(manifest)) console.log(`${name}  ${summary}`);
    for (const name of runbooks.keys()) console.log(`${name}  Local Markdown runbook.`);
  }
} catch (error) {
  console.error(error.message);
  process.exitCode = 1;
}
