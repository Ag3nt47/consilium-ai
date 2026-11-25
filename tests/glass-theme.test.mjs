import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import test from "node:test";

const css = readFileSync(resolve(import.meta.dirname, "../src/styles.css"), "utf8").replace(/\r\n/g, "\n");

function ruleBlock(selectorPrefix) {
  const escaped = selectorPrefix.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const match = css.match(new RegExp(`${escaped}\\s*\\{([^}]*)\\}`));
  assert.ok(match, `Missing CSS rule starting with: ${selectorPrefix}`);
  return match[1];
}

test("visionOS Glass keeps wallpaper and live backdrop containers transparent behind messages", () => {
  const messagesBlock = ruleBlock(
    '[data-theme="glass"] .main.has-wallpaper .messages,\n[data-theme="glass"] .app[data-backdrop]:not([data-backdrop="stealth"]) .messages'
  );
  assert.match(messagesBlock, /background:\s*transparent;/);
  assert.match(messagesBlock, /backdrop-filter:\s*none;/);
  assert.match(messagesBlock, /border-color:\s*transparent;/);

  const mainBackdropBlock = ruleBlock(
    '[data-theme="glass"] .app[data-backdrop]:not([data-backdrop="stealth"]) > .main'
  );
  assert.match(mainBackdropBlock, /background:\s*transparent;/);
  assert.match(mainBackdropBlock, /background-color:\s*transparent;/);

  const chromeBlock = ruleBlock(
    '[data-theme="glass"] .main.has-wallpaper :is(.topbar, .composer),\n[data-theme="glass"] .app[data-backdrop]:not([data-backdrop="stealth"]) :is(.topbar, .composer)'
  );
  assert.match(chromeBlock, /background:\s*transparent;/);
  assert.match(chromeBlock, /backdrop-filter:\s*none;/);
  assert.match(chromeBlock, /box-shadow:\s*none;/);
});
