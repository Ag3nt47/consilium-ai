// Rendered regression checks with mocked Tauri calls; no repositories or app data are changed.
// npm ci; CONSILIUM_TEST_BROWSER=<Chromium executable> node scripts/check-project-status-ui.mjs
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdir, mkdtemp, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { createServer } from "vite";

const folder = resolve(".build-tmp/project-status-ui");
await mkdir(folder, { recursive: true });
const profile = await mkdtemp(join(folder, "profile-"));
await writeFile(join(folder, "index.html"), `<!doctype html><html data-theme="glass"><body>
<div id="root"></div><button id="outside">Outside</button>
<script>
window.fixture = { list: [], hold: false };
window.__TAURI_INTERNALS__ = { invoke: async (command) => {
  if (command !== "project_versions") throw Error("Unexpected fixture command: " + command);
  if (window.fixture.hold) return new Promise(resolve => window.fixture.release = resolve);
  return structuredClone(window.fixture.list);
}};
</script>
<script type="module">
import React from "react";
import { createRoot } from "react-dom/client";
import { ProjectVersions } from "/src/projects.tsx";
import "/src/styles.css";
const root = createRoot(document.getElementById("root"));
window.fixture.mount = (workspace = "/fixtures") => root.render(React.createElement(ProjectVersions, { workspace, key: workspace }));
window.fixture.show = (list) => { window.fixture.list = list; window.dispatchEvent(new Event("focus")); };
window.fixture.mount();
</script></body></html>`);

function connect(url) {
  return new Promise((resolveConnection, rejectConnection) => {
    const socket = new WebSocket(url);
    const pending = new Map();
    let nextId = 0;
    socket.onmessage = ({ data }) => {
      const message = JSON.parse(data);
      const request = pending.get(message.id);
      if (!request) return;
      pending.delete(message.id);
      clearTimeout(request.timer);
      if (message.error) request.reject(Error(message.error.message));
      else request.resolve(message.result);
    };
    socket.onerror = rejectConnection;
    socket.onopen = () => resolveConnection({
      call(method, params = {}) {
        return new Promise((resolveCall, rejectCall) => {
          const id = ++nextId;
          const timer = setTimeout(() => { pending.delete(id); rejectCall(Error("Browser timeout: " + method)); }, 10_000);
          pending.set(id, { resolve: resolveCall, reject: rejectCall, timer });
          socket.send(JSON.stringify({ id, method, params }));
        });
      },
      close() {
        socket.close();
        for (const request of pending.values()) clearTimeout(request.timer);
      },
    });
  });
}

const server = await createServer({ server: { host: "127.0.0.1", port: 0,
  watch: { ignored: ["**/.build-tmp/**"] } }, logLevel: "error" });
let chrome;
let browser;
let page;
try {
  await server.listen();
  const executable = process.env.CONSILIUM_TEST_BROWSER || join(process.env.PROGRAMFILES || "C:/Program Files", "Google/Chrome/Application/chrome.exe");
  chrome = spawn(executable, ["--headless=new", "--disable-gpu", "--disable-extensions", "--remote-debugging-port=0",
    `--user-data-dir=${profile}`, "--no-first-run", "--no-default-browser-check", "about:blank"],
  { windowsHide: true, stdio: ["ignore", "ignore", "pipe"] });
  const endpoint = await new Promise((resolveEndpoint, rejectEndpoint) => {
    let output = "";
    const timer = setTimeout(() => rejectEndpoint(Error("Browser startup timed out")), 15_000);
    chrome.once("error", error => { clearTimeout(timer); rejectEndpoint(error); });
    chrome.stderr.on("data", chunk => {
      output = (output + chunk).slice(-16_384);
      const found = output.match(/DevTools listening on (ws:\/\/[^\s]+)/);
      if (found) { clearTimeout(timer); resolveEndpoint(found[1]); }
    });
  });
  browser = await connect(endpoint);
  const { targetId } = await browser.call("Target.createTarget", { url: "about:blank" });
  const base = endpoint.replace(/^ws:/, "http:").replace(/\/devtools\/browser\/.*/, "");
  const targets = await (await fetch(base + "/json/list")).json();
  page = await connect(targets.find(target => target.id === targetId).webSocketDebuggerUrl);
  await page.call("Page.enable");
  await page.call("Emulation.setDeviceMetricsOverride", { width: 1000, height: 700, deviceScaleFactor: 1, mobile: false });
  await page.call("Page.navigate", { url: `http://127.0.0.1:${server.httpServer.address().port}/.build-tmp/project-status-ui/index.html` });
  const evaluate = async expression => {
    const result = await page.call("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true });
    if (result.exceptionDetails) throw Error(result.exceptionDetails.exception?.description || result.exceptionDetails.text);
    return result.result.value;
  };
  const until = async expression => {
    for (let step = 0; step < 150; step++) {
      if (await evaluate(`!!(${expression})`)) return;
      await new Promise(resolveDelay => setTimeout(resolveDelay, 40));
    }
    throw Error("Fixture condition failed: " + expression);
  };
  await until("window.fixture?.mount");
  assert.equal(await evaluate("!!document.querySelector('.pv-tab')"), false, "Empty workspaces hide source status");
  await evaluate(`window.fixture.project = { name: "example", path: "/fixtures/example", local_version: "1.2.3", remote_version: "1.2.3",
    local_commit: "abc1234", remote_commit: "abc1234", ahead: 0, behind: 0, dirty: 0, remote_error: null };
    window.fixture.show([window.fixture.project]);`);
  await until("document.querySelector('.pv-tab')?.textContent === 'Source synced'");
  await evaluate("window.fixture.show([window.fixture.project, {...window.fixture.project, name:'checkout', path:'/fixtures/checkout'}, {...window.fixture.project, name:'service', path:'/fixtures/service'}])");
  await until("document.querySelector('.pv-tab')?.textContent === 'Source synced'");
  assert.equal(await evaluate("document.querySelector('.pv-tab').textContent.includes('projects')"), false, "Multiple checkouts never add a project/version count to the header");
  await evaluate("document.querySelector('.pv-tab').click()");
  await until("document.querySelectorAll('.pv-row').length === 3");
  assert.equal(await evaluate("document.querySelector('[role=dialog]').getAttribute('aria-label')"), "Project source versions");
  assert.match(await evaluate("document.querySelector('.pv-pop').textContent"), /Local.*v1\.2\.3.*GitHub.*v1\.2\.3/);
  await evaluate("document.dispatchEvent(new KeyboardEvent('keydown', {key:'Escape', bubbles:true}))");
  await until("!document.querySelector('.pv-pop')");
  assert.equal(await evaluate("document.activeElement === document.querySelector('.pv-tab')"), true, "Escape returns focus to the trigger");
  await evaluate("document.querySelector('.pv-tab').click()");
  await until("document.querySelector('.pv-pop')");
  await evaluate("document.getElementById('outside').dispatchEvent(new PointerEvent('pointerdown', {bubbles:true}))");
  await until("!document.querySelector('.pv-pop')");
  await evaluate("window.fixture.show([{...window.fixture.project,dirty:1}])");
  await until("document.querySelector('.pv-tab')?.textContent === 'Source changes'");
  assert.equal(await evaluate("document.querySelector('.pv-tab').classList.contains('pv-diff')"), true, "Uncommitted files must not show green");
  await evaluate("window.fixture.show([{...window.fixture.project,local_commit:'def5678'}])");
  await until("document.querySelector('.pv-tab')?.textContent === 'Source changes'");
  await evaluate("document.querySelector('.pv-tab').click()");
  await until("document.querySelector('.pv-pop')?.textContent.includes('Local commit differs from GitHub')");
  await evaluate("window.fixture.show([{...window.fixture.project,remote_error:'No GitHub remote'}])");
  await until("document.querySelector('.pv-tab')?.textContent === 'Source unavailable'");
  await evaluate("window.fixture.hold=true; window.fixture.mount('/next-folder')");
  await until("typeof window.fixture.release === 'function'");
  assert.equal(await evaluate("!!document.querySelector('.pv-tab')"), false, "Folder changes clear stale status immediately");
  await evaluate("window.fixture.mount(null)");
  await until("!document.querySelector('.pv-tab')");
  await evaluate("window.fixture.release([window.fixture.project])");
  await new Promise(resolveDelay => setTimeout(resolveDelay, 100));
  assert.equal(await evaluate("!!document.querySelector('.pv-tab')"), false, "Late replies from an old workspace are discarded");
  console.log("Project status browser checks passed: compact 1/3-checkout summary, details, dirty/commit/error states, Escape/focus, outside click, workspace changes and late replies.");
} finally {
  page?.close();
  if (browser) { await browser.call("Browser.close").catch(() => {}); browser.close(); }
  if (chrome && chrome.exitCode === null) chrome.kill();
  await server.close();
}
