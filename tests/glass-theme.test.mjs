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

test("visionOS Glass keeps wallpaper and live backdrop containers transparent behind frosted cards", () => {
  const messagesBlock = ruleBlock(
    '[data-theme="glass"] .main.has-wallpaper .messages,\n[data-theme="glass"] .app[data-backdrop]:not([data-backdrop="stealth"]) .messages'
  );
  assert.match(messagesBlock, /background:\s*transparent;/);
  assert.match(messagesBlock, /backdrop-filter:\s*none;/);
  assert.match(messagesBlock, /border-color:\s*transparent;/);
  assert.match(messagesBlock, /box-shadow:\s*none;/);

  const mainBackdropBlock = ruleBlock(
    '[data-theme="glass"] .app[data-backdrop]:not([data-backdrop="stealth"]) > .main'
  );
  assert.match(mainBackdropBlock, /background:\s*transparent;/);

  const chromeBlock = ruleBlock(
    '[data-theme="glass"] .main.has-wallpaper :is(.topbar, .composer),\n[data-theme="glass"] .app[data-backdrop]:not([data-backdrop="stealth"]) :is(.topbar, .composer)'
  );
  assert.match(chromeBlock, /background:\s*transparent;/);
  assert.match(chromeBlock, /backdrop-filter:\s*none;/);
  assert.match(chromeBlock, /box-shadow:\s*none;/);

  const messageCardsBlock = ruleBlock(
    '[data-theme="glass"] .main.has-wallpaper :is(.msg, .thinking),\n[data-theme="glass"] .app[data-backdrop]:not([data-backdrop="stealth"]) :is(.msg, .thinking)'
  );
  assert.match(messageCardsBlock, /backdrop-filter:\s*blur\(14px\)/);

  const requestBlock = ruleBlock(
    '[data-theme="glass"] .main.has-wallpaper .request,\n[data-theme="glass"] .app[data-backdrop]:not([data-backdrop="stealth"]) .request'
  );
  assert.match(requestBlock, /backdrop-filter:\s*none;/);

  const textareaBlock = ruleBlock(
    '[data-theme="glass"] .main.has-wallpaper .composer textarea,\n[data-theme="glass"] .app[data-backdrop]:not([data-backdrop="stealth"]) .composer textarea'
  );
  assert.match(textareaBlock, /backdrop-filter:\s*blur\(20px\);/);

  const chipOffBlock = ruleBlock(
    '[data-theme="glass"] .main.has-wallpaper .composer .chip:not(.on),\n[data-theme="glass"] .app[data-backdrop]:not([data-backdrop="stealth"]) .composer .chip:not(.on)'
  );
  assert.match(chipOffBlock, /backdrop-filter:\s*blur\(12px\);/);

  const chipOnBlock = ruleBlock(
    '[data-theme="glass"] .main.has-wallpaper .composer .chip.on,\n[data-theme="glass"] .app[data-backdrop]:not([data-backdrop="stealth"]) .composer .chip.on'
  );
  assert.match(chipOnBlock, /backdrop-filter:\s*blur\(12px\);/);

  const noteBlock = ruleBlock('[data-theme="glass"] .note');
  assert.match(noteBlock, /backdrop-filter:\s*blur\(20px\);/);

  const noteBorderBlock = ruleBlock('[data-theme="glass"] .note:not(.note-error)');
  assert.match(noteBorderBlock, /border:\s*1px solid var\(--border\);/);
});
