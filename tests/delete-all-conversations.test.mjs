import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const app = readFileSync(new URL("../src/App.tsx", import.meta.url), "utf8");
const api = readFileSync(new URL("../src/api.ts", import.meta.url), "utf8");
const backend = readFileSync(new URL("../src-tauri/src/lib.rs", import.meta.url), "utf8");

test("delete-all requires two distinct confirmations before invoking the backend", () => {
  assert.match(app, /deleteAllStep === 1/);
  assert.match(app, /deleteAllStep === 2/);
  assert.match(app, /A second confirmation is required before anything is deleted/);
  assert.match(app, /Final confirmation:<\/strong> this can't be undone\. Type <strong>DELETE<\/strong> to confirm/);
  assert.match(app, /deleteAllText\.trim\(\)\.toUpperCase\(\) !== "DELETE"/);
  assert.equal(app.match(/api\.removeAll\(\)/g)?.length, 1);
  const finalDialog = app.slice(app.indexOf("{deleteAllStep === 2"), app.indexOf("{showRestartModal"));
  assert.match(finalDialog, /onClick=\{deleteAllConfirmed\}/);
});

test("delete-all is a dedicated registered command with a typed API", () => {
  assert.match(api, /removeAll: \(\) => invoke<number>\("delete_all_conversations"\)/);
  assert.match(backend, /fn delete_all_conversations\(state: State<AppState>\) -> Result<usize, String>/);
  assert.match(backend, /delete_all_conversations,\n/);
  assert.match(backend, /stop every running conversation before deleting them all/);
});
