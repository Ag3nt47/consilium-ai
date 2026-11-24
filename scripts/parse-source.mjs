import { constants, openSync, closeSync, fstatSync, readSync } from "node:fs";
import { createRequire } from "node:module";
import { parseSourceText, SOURCE_LIMIT } from "./source-outline.mjs";
export { supportedSourceExtensions } from "./source-outline.mjs";
const require = createRequire(import.meta.url);

export function sourceParser() {
  return require("typescript-ast");
}

function readSource(file) {
  const fd = openSync(file, constants.O_RDONLY | constants.O_NONBLOCK);
  try {
    const stat = fstatSync(fd);
    if (!stat.isFile() || stat.size > SOURCE_LIMIT) throw new Error("Use a source file up to 1 MiB.");
    const buffer = Buffer.alloc(SOURCE_LIMIT + 1);
    let length = 0;
    while (length < buffer.length) {
      const count = readSync(fd, buffer, length, buffer.length - length, null);
      if (count === 0) break;
      length += count;
    }
    if (length > SOURCE_LIMIT) throw new Error("The source exceeds 1 MiB.");
    return buffer.toString("utf8", 0, length);
  } finally {
    closeSync(fd);
  }
}

export function parseSource(file) {
  return parseSourceText(sourceParser(), file, readSource(file));
}
