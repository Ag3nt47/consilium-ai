import test from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { lfOnly, pageFingerprint } from "../scripts/backend-fingerprint.mjs";

function tree(files) {
  const dir = mkdtempSync(join(tmpdir(), "consilium-fp-"));
  for (const [name, body] of Object.entries(files)) {
    mkdirSync(join(dir, name, ".."), { recursive: true });
    writeFileSync(join(dir, name), body);
  }
  return dir;
}

test("lfOnly turns CRLF into LF and leaves lone CR alone", () => {
  assert.equal(lfOnly(Buffer.from("a\r\nb\rc\n\r\n")).toString(), "a\nb\rc\n\n");
});

test("the same files fingerprint the same with LF or CRLF endings", () => {
  const lf = tree({ "src/main.rs": "fn main() {\n}\n", "a.txt": "x\ny\n" });
  const crlf = tree({ "src/main.rs": "fn main() {\r\n}\r\n", "a.txt": "x\r\ny\r\n" });
  const other = tree({ "src/main.rs": "fn main() {\n  1;\n}\n", "a.txt": "x\ny\n" });
  try {
    assert.equal(pageFingerprint(lf), pageFingerprint(crlf));
    assert.notEqual(pageFingerprint(lf), pageFingerprint(other));
  } finally {
    for (const d of [lf, crlf, other]) rmSync(d, { recursive: true, force: true });
  }
});
