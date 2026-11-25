export const SOURCE_LIMIT = 1024 * 1024;
export const supportedSourceExtensions = new Set([".js", ".jsx", ".ts", ".tsx", ".mjs", ".cjs", ".mts", ".cts"]);

export function parseSourceText(ts, file, text) {
  const source = ts.createSourceFile(file, text, ts.ScriptTarget.Latest, true);
  const diagnostic = source.parseDiagnostics[0];
  if (diagnostic) {
    const { line, character } = source.getLineAndCharacterOfPosition(diagnostic.start ?? 0);
    const message = ts.flattenDiagnosticMessageText(diagnostic.messageText, " ");
    throw new Error(`Parse error at ${line + 1}:${character + 1}: ${message}`);
  }
  return source;
}

const ROW_LIMIT = 100;
const clip = (text) => {
  const line = text.replace(/\s+/g, " ");
  return line.length > 140 ? line.slice(0, 139) + "…" : line;
};

function outline(ts, source, statements, filter) {
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
    function addBindings(declaration) {
      if (ts.isIdentifier(declaration.name)) {
        addSymbol("binding", declaration.name.text, declaration);
      } else {
        for (const entry of declaration.name.elements) {
          if (ts.isBindingElement(entry)) addBindings(entry);
        }
      }
    }
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
        if (member.name) {
          const computed = ts.isComputedPropertyName(member.name);
          const literal = computed && (ts.isStringLiteral(member.name.expression) || ts.isNumericLiteral(member.name.expression));
          const memberName = computed && !literal ? "[computed]" : member.name.getText(source);
          addSymbol("member", `${name}.${memberName}`, member);
        }
        else if (ts.isConstructorDeclaration(member)) addSymbol("constructor", name, member);
      }
    } else if (ts.isTypeAliasDeclaration(node) || ts.isEnumDeclaration(node)) {
      addSymbol(ts.isTypeAliasDeclaration(node) ? "type" : "enum", node.name.text);
    } else if (ts.isVariableStatement(node)) {
      for (const declaration of node.declarationList.declarations) addBindings(declaration);
    }
  }
  for (const node of statements) visit(node);
  return { rows, omitted: total - rows.length };
}

export function outlineSource(ts, source, functionName, filter) {
  const scope = functionName
    ? source.statements.find((node) => ts.isFunctionDeclaration(node) && node.name?.text === functionName && node.body)
    : undefined;
  if (functionName && !scope) throw new Error("Top-level function with a body not found.");
  return outline(ts, source, scope?.body.statements ?? source.statements, filter?.toLowerCase());
}

export function outlineSourceText(ts, file, text, functionName, filter) {
  return outlineSource(ts, parseSourceText(ts, file, text), functionName, filter);
}

// One selected file per worker. Filter-only requests reuse its parsed source.
export function createOutlineSession(ts) {
  let source;
  return async ({ file, functionName, filter }) => {
    if (file) {
      source = undefined;
      const extension = /\.[^.]+$/.exec(file.name)?.[0]?.toLowerCase() ?? "";
      if (!supportedSourceExtensions.has(extension)) throw new Error("Choose a JavaScript or TypeScript file.");
      if (file.size > SOURCE_LIMIT) throw new Error("Choose a source file up to 1 MiB.");
      source = parseSourceText(ts, file.name, await file.text());
    }
    if (!source) throw new Error("Choose a source file before building an outline.");
    return outlineSource(ts, source, functionName || undefined, filter || undefined);
  };
}
