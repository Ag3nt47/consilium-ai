import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const css = readFileSync(new URL("../src/styles.css", import.meta.url), "utf8");

test("council member boxes get the shared hover glow, but not while a control inside is hovered", () => {
  assert.match(css, /:is\(\.msg, \.note, \.thinking, \.request, \.agent\):hover:not\(:has\(/);
});

test("council member boxes animate the glow like other boxes and respect reduced motion", () => {
  assert.match(css, /\.thinking, \.request, \.agent, textarea, select,\s+input:not/);
  assert.match(css, /prefers-reduced-motion: reduce\) \{\s+:is\([^)]*\.agent[^)]*\) \{\s+transition: none;/);
});