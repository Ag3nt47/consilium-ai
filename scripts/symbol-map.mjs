import { openSync, closeSync, fstatSync, readSync } from "node:fs";
import { extname } from "node:path";
import ts from "typescript-ast";

const SOURCE_LIMIT = 1024 * 1024;
const ROW_LIMIT = 100;
const supported = new Set([".js", ".jsx", ".ts", ".tsx", ".mjs", ".cjs", ".mts", ".cts"]);
const clip = (text) => {
  const line = text.replace(/\s+/g, " ");
  return line.length > 140 ? line.slice(0, 139) + "…" : line;
};

function readSource(file) {
  const fd = openSync(file, "r");
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

function outline(source, statements, filter) {
  let total = 0;
  const rows = [];
  const add = (node, kind, name) => {
    if (filter && !name.toLowerCase().includes(filter)) return;
    total++;
    if (rows.length >= ROW_LIMIT) return;
    const first = source.getLineAndCharacterOfPosition(node.getStart(source)).line + 1;
    const last = source.getLineAndCharacterOfPosition(node.getEnd()).line + 1;
    rows.push(`${first === last ? first : `${first}-${last}`} ${kind} ${clip(name)}`);
  };
  for (const node of statements) {
    if ((ts.isImportDeclaration(node) || ts.isExportDeclaration(node)) && node.moduleSpecifier && ts.isStringLiteral(node.moduleSpecifier)) {
      add(node, ts.isImportDeclaration(node) ? "import" : "re-export", node.moduleSpecifier.getText(source));
    } else if (ts.isFunctionDeclaration(node)) {
      add(node, "function", node.name?.text ?? "(anonymous)");
    } else if (ts.isClassDeclaration(node) || ts.isInterfaceDeclaration(node)) {
      const name = node.name?.text ?? "(anonymous)";
      add(node, ts.isClassDeclaration(node) ? "class" : "interface", name);
      for (const member of node.members) {
        if (member.name) add(member, "member", `${name}.${member.name.getText(source)}`);
        else if (ts.isConstructorDeclaration(member)) add(member, "constructor", name);
      }
    } else if (ts.isTypeAliasDeclaration(node) || ts.isEnumDeclaration(node)) {
      add(node, ts.isTypeAliasDeclaration(node) ? "type" : "enum", node.name.text);
    } else if (ts.isVariableStatement(node)) {
      for (const declaration of node.declarationList.declarations) {
        add(declaration, "binding", declaration.name.getText(source));
      }
    }
  }
  return { rows, omitted: total - rows.length };
}

try {
  const [file, functionName, filter, ...extra] = process.argv.slice(2);
  if (!file || extra.length || !supported.has(extname(file).toLowerCase())) {
    throw new Error("Usage: node scripts/symbol-map.mjs <JS or TS file> [top-level function] [symbol filter]");
  }
  const text = readSource(file);
  const source = ts.createSourceFile(file, text, ts.ScriptTarget.Latest, true);
  const scope = functionName
    ? source.statements.find((node) => ts.isFunctionDeclaration(node) && node.name?.text === functionName && node.body)
    : undefined;
  if (functionName && !scope) throw new Error("Top-level function with a body not found.");
  const { rows, omitted } = outline(source, scope?.body.statements ?? source.statements, filter?.toLowerCase());
  console.log(`${JSON.stringify(file)} — ${rows.length} rows`);
  for (const row of rows) console.log(row);
  if (omitted) console.log(`${omitted} additional rows omitted.`);
} catch (error) {
  console.error(error.message);
  process.exitCode = 1;
}
