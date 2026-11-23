import { constants, openSync, closeSync, fstatSync, readSync } from "node:fs";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);

const SOURCE_LIMIT = 1024 * 1024;
export const supportedSourceExtensions = new Set([".js", ".jsx", ".ts", ".tsx", ".mjs", ".cjs", ".mts", ".cts"]);

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
  const ts = require("typescript-ast");
  const text = readSource(file);
  const source = ts.createSourceFile(file, text, ts.ScriptTarget.Latest, true);
  const diagnostic = source.parseDiagnostics[0];
  if (diagnostic) {
    const { line, character } = source.getLineAndCharacterOfPosition(diagnostic.start ?? 0);
    const message = ts.flattenDiagnosticMessageText(diagnostic.messageText, " ");
    throw new Error(`Parse error at ${line + 1}:${character + 1}: ${message}`);
  }
  return source;
}
