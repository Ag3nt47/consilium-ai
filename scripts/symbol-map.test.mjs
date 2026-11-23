import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

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
