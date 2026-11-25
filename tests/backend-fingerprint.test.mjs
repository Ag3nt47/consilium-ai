import test from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fingerprint, isText, lfOnly, pageFingerprint } from "../scripts/backend-fingerprint.mjs";

function tree(files) {
  const dir = mkdtempSync(join(tmpdir(), "consilium-fp-"));
  for (const [name, body] of Object.entries(files)) {
    mkdirSync(join(dir, name, ".."), { recursive: true });
    writeFileSync(join(dir, name), body);
  }
  return dir;
}

function withTrees(list, check) {
  const dirs = list.map(tree);
  try {
    check(...dirs);
  } finally {
    for (const d of dirs) rmSync(d, { recursive: true, force: true });
  }
}

// A PNG-like file: NUL bytes make it binary, and it holds a CRLF pair that must never be touched.
const png = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0x0d, 0x0d, 0x0a]);

test("lfOnly turns CRLF into LF and leaves lone CR alone", () => {
  assert.equal(lfOnly(Buffer.from("a\r\nb\rc\n\r\n")).toString(), "a\nb\rc\n\n");
});

test("isText follows git: a NUL byte in the first 8000 bytes means binary", () => {
  assert.equal(isText(Buffer.from("a\r\nb")), true);
  assert.equal(isText(png), false);
});

test("the engine fingerprint ignores CRLF in text files", () => {
  withTrees(
    [
      { "src/main.rs": "fn main() {\n}\n", "Cargo.toml": "[package]\n" },
      { "src/main.rs": "fn main() {\r\n}\r\n", "Cargo.toml": "[package]\r\n" },
      { "src/main.rs": "fn main() {\n  1;\n}\n", "Cargo.toml": "[package]\n" },
    ],
    (lf, crlf, other) => {
      assert.equal(fingerprint(lf), fingerprint(crlf));
      assert.notEqual(fingerprint(lf), fingerprint(other));
    },
  );
});

test("the engine fingerprint sees every byte of a binary file", () => {
  withTrees(
    [{ "src/icon.png": png }, { "src/icon.png": Buffer.concat([png.subarray(0, 4), png.subarray(5)]) }],
    (a, b) => assert.notEqual(fingerprint(a), fingerprint(b)),
  );
});

test("the page fingerprint is exact: removing one byte from an image or a CR changes it", () => {
  withTrees(
    [
      { "index.html": "<p>\r\n", "icon.png": png },
      { "index.html": "<p>\r\n", "icon.png": Buffer.concat([png.subarray(0, 4), png.subarray(5)]) },
      { "index.html": "<p>\n", "icon.png": png },
    ],
    (base, image, text) => {
      assert.notEqual(pageFingerprint(base), pageFingerprint(image));
      assert.notEqual(pageFingerprint(base), pageFingerprint(text));
    },
  );
});
