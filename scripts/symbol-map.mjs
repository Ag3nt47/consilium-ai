import { constants, openSync, closeSync, fstatSync, readSync } from "node:fs";
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
  function visit(node, prefix = "") {
    const addSymbol = (kind, name, declaration = node) => add(declaration, kind, prefix + name);
    if (ts.isModuleDeclaration(node)) {
      const name = node.name.getText(source);
      addSymbol(ts.isStringLiteral(node.name) ? "module" : "namespace", name);
      if (node.body && ts.isModuleBlock(node.body)) {
        for (const statement of node.body.statements) visit(statement, prefix + name + ".");
      } else if (node.body && ts.isModuleDeclaration(node.body)) {
        visit(node.body, prefix + name + ".");
      }
    } else if ((ts.isImportDeclaration(node) || ts.isExportDeclaration(node)) && node.moduleSpecifier && ts.isStringLiteral(node.moduleSpecifier)) {
      addSymbol(ts.isImportDeclaration(node) ? "import" : "re-export", node.moduleSpecifier.getText(source));
    } else if (ts.isFunctionDeclaration(node)) {
      addSymbol("function", node.name?.text ?? "(anonymous)");
    } else if (ts.isClassDeclaration(node) || ts.isInterfaceDeclaration(node)) {
      const name = node.name?.text ?? "(anonymous)";
      addSymbol(ts.isClassDeclaration(node) ? "class" : "interface", name);
      for (const member of node.members) {
        if (member.name) addSymbol("member", `${name}.${member.name.getText(source)}`, member);
        else if (ts.isConstructorDeclaration(member)) addSymbol("constructor", name, member);
      }
    } else if (ts.isTypeAliasDeclaration(node) || ts.isEnumDeclaration(node)) {
      addSymbol(ts.isTypeAliasDeclaration(node) ? "type" : "enum", node.name.text);
    } else if (ts.isVariableStatement(node)) {
      for (const declaration of node.declarationList.declarations) {
        addSymbol("binding", declaration.name.getText(source), declaration);
      }
    }
  }
  for (const node of statements) visit(node);
  return { rows, omitted: total - rows.length };
}

try {
  const [file, scopeArg, filter, ...extra] = process.argv.slice(2);
  const filterOnly = scopeArg === "--filter";
  if (!file || extra.length || (filterOnly && !filter?.trim()) || !supported.has(extname(file).toLowerCase())) {
    throw new Error("Usage: node scripts/symbol-map.mjs <JS or TS file> [top-level function | --filter] [symbol filter]");
  }
  const functionName = filterOnly ? undefined : scopeArg;
  const text = readSource(file);
  const source = ts.createSourceFile(file, text, ts.ScriptTarget.Latest, true);
  const diagnostic = source.parseDiagnostics[0];
  if (diagnostic) {
    const { line, character } = source.getLineAndCharacterOfPosition(diagnostic.start ?? 0);
    const message = ts.flattenDiagnosticMessageText(diagnostic.messageText, " ");
    throw new Error(`Parse error at ${line + 1}:${character + 1}: ${message}`);
  }
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
