import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { sourceParser } from "./parse-source.mjs";
import { createOutlineSession, outlineSourceText, SOURCE_LIMIT } from "./source-outline.mjs";

test("outlines show binding identifiers and omit initializer and computed-name expressions", () => {
  const source = [
    'const { original: renamed = "OMIT_DEFAULT", nested: [first, , ...rest] } = obj;',
    'class Entry { [makeName("OMIT_EXPRESSION")]() {} ["fixed"]() {} }',
  ].join("\n");
  const { rows } = outlineSourceText(sourceParser(), "example.ts", source);
  assert.deepEqual(rows, [
    "1 binding renamed",
    "1 binding first",
    "1 binding rest",
    "2 class Entry",
    "2 member Entry.[computed]",
    '2 member Entry.["fixed"]',
  ]);
});

test("qualified TypeScript outlines include nested declarations and bound output", () => {
  const dir = mkdtempSync(join(tmpdir(), "consilium-symbol-map-"));
  try {
    const file = join(dir, "example.ts");
    writeFileSync(file, [
      "namespace Outer.Inner {",
      "  export class Entry { getValue() { return 1; } }",
      ...Array.from({ length: 105 }, (_, index) => `  export const value${index} = ${index};`),
      "}",
    ].join("\n"));
    const outline = (filter) => {
      const result = spawnSync(process.execPath, ["scripts/symbol-map.mjs", file, "--filter", filter], { encoding: "utf8" });
      assert.equal(result.status, 0, result.stderr);
      return result.stdout;
    };
    assert.match(outline("Outer.Inner.Entry"), /2 member Outer\.Inner\.Entry\.getValue/);
    const bounded = outline("Outer.Inner.value");
    assert.match(bounded, /100 rows/);
    assert.match(bounded, /5 additional rows omitted\./);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("outline filters and function scopes reuse one parsed source with unchanged output", async () => {
  const ts = sourceParser();
  let parses = 0;
  let reads = 0;
  const outline = createOutlineSession({ ...ts, createSourceFile(...args) {
    parses++;
    return ts.createSourceFile(...args);
  } });
  const text = "const first = 1;\nfunction task() { const inner = 2; function nested() {} }";
  const file = { name: "example.ts", size: text.length, text: async () => { reads++; return text; } };
  assert.deepEqual(await outline({ file }), outlineSourceText(ts, file.name, text));
  for (const options of [{ filter: "FIRST" }, { functionName: "task" }, { functionName: "task", filter: "inner" }]) {
    assert.deepEqual(await outline(options), outlineSourceText(ts, file.name, text, options.functionName, options.filter));
  }
  assert.equal(parses, 1);
  assert.equal(reads, 1);
});

test("a new file with identical metadata replaces the cached outline", async () => {
  const outline = createOutlineSession(sourceParser());
  const file = (text) => new File([text], "example.ts", { lastModified: 1 });
  const first = file("const first = 1;");
  const other = file("const other = 1;");
  assert.equal(first.size, other.size);
  assert.deepEqual((await outline({ file: first })).rows, ["1 binding first"]);
  assert.deepEqual((await outline({ file: other })).rows, ["1 binding other"]);
  assert.deepEqual((await outline({ filter: "first" })).rows, []);
});

test("invalid replacement files clear the old source and allow a clean retry", async () => {
  const badFiles = [
    { name: "example.txt", size: 10, text: async () => "const bad = 1;" },
    { name: "example.ts", size: SOURCE_LIMIT + 1, text: async () => { throw new Error("Must not read oversized source"); } },
    { name: "example.ts", size: 10, text: async () => { throw new Error("Read failed"); } },
    new File(["const broken = ;"], "example.ts"),
  ];
  for (const [index, file] of badFiles.entries()) {
    const outline = createOutlineSession(sourceParser());
    await outline({ file: new File(["const previous = 1;"], "example.ts") });
    await assert.rejects(outline({ file }), [
      /JavaScript or TypeScript/, /up to 1 MiB/, /Read failed/, /Parse error at/,
    ][index]);
    await assert.rejects(outline({}), /Choose a source file/);
    assert.deepEqual((await outline({ file: new File(["const next = 2;"], "example.ts") })).rows, ["1 binding next"]);
  }
});

test("cached outlines keep the row cap and can recover from an unknown function", async () => {
  const outline = createOutlineSession(sourceParser());
  const text = Array.from({ length: 105 }, (_, index) => `const value${index} = ${index};`).join("\n");
  const first = await outline({ file: new File([text], "example.ts") });
  assert.equal(first.rows.length, 100);
  assert.equal(first.omitted, 5);
  await assert.rejects(outline({ functionName: "missing" }), /Top-level function/);
  assert.deepEqual(await outline({ filter: "value" }), first);
});
