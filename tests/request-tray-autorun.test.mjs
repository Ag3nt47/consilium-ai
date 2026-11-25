import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const app = readFileSync(new URL("../src/App.tsx", import.meta.url), "utf8");

test("the request tray starts minimized and follows auto-run", () => {
  assert.match(app, /const \[collapsed, setCollapsed\] = useState\(autoRun\);/);
  assert.match(app, /useEffect\(\(\) => setCollapsed\(autoRun\), \[autoRun\]\);/);
  assert.match(app, /autoRun=\{!!council\?\.auto_run_commands \|\| chatAutoRun\}/);
});
