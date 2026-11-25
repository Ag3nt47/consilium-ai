// Explicit browser fixture: npm ci, then CONSILIUM_TEST_BROWSER=<Chromium executable>
// node scripts/check-bluetooth-pairing-ui.mjs. No Tauri, radios, or app data are used.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdir, mkdtemp, writeFile } from "node:fs/promises";
import { resolve, join } from "node:path";
import { createServer } from "vite";

const folder = resolve(".build-tmp/bluetooth-pairing-ui");
await mkdir(folder, { recursive: true });
const profile = await mkdtemp(join(folder, "profile-"));
const html = `<!doctype html><html><body><div id="root"></div>
<script>
const snapshot = (state, extra = {}) => ({attempt_id:"fixture-attempt",node_id:"saved-mac",peer_name:"Saved Mac",challenge_id:null,
 state,code:null,remaining_ms:90000,authenticated:false,remembered:false,detail:"Fixture " + state,...extra});
window.fixture = {calls:[],snapshot,reply:snapshot("starting"),deferBegin:false,blockPoll:false};
window.__TAURI_INTERNALS__ = { invoke: async (cmd,args) => {
 const f=window.fixture; f.calls.push({cmd,args});
 if(cmd==="bluetooth_pairing_begin") {if(f.deferBegin) await new Promise(r=>f.releaseBegin=r);return snapshot("starting");}
 if(cmd==="bluetooth_pairing_status") {if(f.blockPoll) await new Promise(()=>{});return structuredClone(f.reply);}
 if(cmd==="bluetooth_pairing_confirm") return f.reply=snapshot("confirming");
 if(cmd==="bluetooth_pairing_cancel") return f.reply=snapshot("cancelled",{remaining_ms:0});
 throw Error("Unexpected fixture command");
}};
</script>
<script type="module">
import React from "react";
import {createRoot} from "react-dom/client";
import {NativeBluetoothPairing} from "/src/NativeBluetoothPairing.tsx";
import "/src/styles.css";
const root=createRoot(document.getElementById("root"));let key=0;
window.fixture.mount=()=>root.render(React.createElement(NativeBluetoothPairing,{nodeId:"saved-mac",key:++key}));
window.fixture.hide=()=>root.render(null);
window.fixture.mount();
</script></body></html>`;
await writeFile(join(folder, "index.html"), html);

function connect(url) {
  return new Promise((resolveConnection, rejectConnection) => {
    const socket = new WebSocket(url);
    const waiting = new Map();
    let nextId = 0;
    socket.onmessage = ({ data }) => {
      const message = JSON.parse(data);
      if (!message.id) return;
      const request = waiting.get(message.id);
      waiting.delete(message.id);
      clearTimeout(request?.timer);
      if (message.error) request?.reject(Error(message.error.message));
      else request?.resolve(message.result);
    };
    socket.onerror = rejectConnection;
    socket.onopen = () => resolveConnection({
      call(method, params = {}) {
        return new Promise((resolveCall, rejectCall) => {
          const id = ++nextId;
          const timer = setTimeout(() => { waiting.delete(id); rejectCall(Error(`Browser call timed out: ${method}`)); }, 10_000);
          waiting.set(id, { resolve: resolveCall, reject: rejectCall, timer });
          socket.send(JSON.stringify({ id, method, params }));
        });
      },
      close() { socket.close(); for (const request of waiting.values()) clearTimeout(request.timer); },
    });
  });
}

const server = await createServer({ server: { host: "127.0.0.1", port: 0, strictPort: false,
  watch: { ignored: ["**/.build-tmp/**"] } }, logLevel: "error" });
let chrome;
let browser;
let page;
try {
  await server.listen();
  const port = server.httpServer.address().port;
  const executable = process.env.CONSILIUM_TEST_BROWSER || join(process.env.PROGRAMFILES || "C:/Program Files", "Google/Chrome/Application/chrome.exe");
  chrome = spawn(executable, ["--headless=new", "--disable-gpu", "--disable-extensions", "--remote-debugging-port=0",
    `--user-data-dir=${profile}`, "--no-first-run", "--no-default-browser-check", "about:blank"], { windowsHide: true, stdio: ["ignore", "ignore", "pipe"] });
  const endpoint = await new Promise((resolveEndpoint, rejectEndpoint) => {
    let output = "";
    const timer = setTimeout(() => rejectEndpoint(Error("Headless browser startup timed out")), 15_000);
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
  await page.call("Runtime.enable");
  await page.call("Page.enable");
  await page.call("Page.navigate", { url: `http://127.0.0.1:${port}/.build-tmp/bluetooth-pairing-ui/index.html` });
  const evaluate = async expression => {
    const result = await page.call("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true });
    if (result.exceptionDetails) throw Error(result.exceptionDetails.exception?.description || result.exceptionDetails.text);
    return result.result.value;
  };
  const until = async expression => {
    for (let step = 0; step < 100; step++) {
      if (await evaluate(`!!(${expression})`)) return;
      await new Promise(resolveDelay => setTimeout(resolveDelay, 40));
    }
    throw Error("Browser fixture condition failed: " + expression);
  };
  const click = text => evaluate(`Array.from(document.querySelectorAll('button')).find(button=>button.textContent.trim()===${JSON.stringify(text)}).click()`);
  const fill = code => evaluate(`(()=>{const input=document.querySelector('input');Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value').set.call(input,${JSON.stringify(code)});input.dispatchEvent(new Event('input',{bubbles:true}));})()`);
  const announce = (id = "challenge-one", code = "012345", remaining = 30_000) => evaluate(`window.fixture.reply=window.fixture.snapshot("awaiting_code",{challenge_id:${JSON.stringify(id)},code:${JSON.stringify(code)},remaining_ms:${remaining}})`);
  const mount = async () => { await evaluate("window.fixture.mount()"); await until("document.querySelector('button')?.textContent==='Show Bluetooth code'"); };
  const begin = async () => { await click("Show Bluetooth code"); await until("window.fixture.calls.some(call=>call.cmd==='bluetooth_pairing_begin')"); };

  await until("window.fixture?.mount && document.querySelector('button')");
  assert.equal(await evaluate("window.fixture.calls.length"), 0, "Mount must not start pairing");
  await announce();
  await begin();
  await until("document.querySelector('input')");
  assert.equal(await evaluate("window.fixture.calls.find(call=>call.cmd==='bluetooth_pairing_begin').args.nodeId"), "saved-mac");
  assert.equal(await evaluate("document.querySelector('code').textContent"), "012345");
  await fill("012345");
  await until("!Array.from(document.querySelectorAll('button')).find(button=>button.textContent==='Confirm match').disabled");
  assert.equal(await evaluate("window.fixture.calls.filter(call=>call.cmd==='bluetooth_pairing_confirm').length"), 0, "Typing must not confirm");
  await click("Confirm match");
  await until("window.fixture.calls.some(call=>call.cmd==='bluetooth_pairing_confirm')");
  assert.deepEqual(await evaluate("window.fixture.calls.find(call=>call.cmd==='bluetooth_pairing_confirm').args"),
    { attemptId: "fixture-attempt", challengeId: "challenge-one", code: "012345" });
  await until("!document.querySelector('code')");
  await click("Cancel Bluetooth pairing");
  await until("document.body.textContent.includes('Fixture cancelled') && !document.querySelector('input')");

  await mount();
  await announce();
  await begin();
  await until("document.querySelector('input')");
  await fill("012345");
  await announce("challenge-two", "654321");
  await until("document.querySelector('code')?.textContent==='654321'");
  assert.equal(await evaluate("document.querySelector('input').value"), "", "A changed challenge must discard typed digits");
  assert.equal(await evaluate("Array.from(document.querySelectorAll('button')).find(button=>button.textContent==='Confirm match').disabled"), true);
  const beforeClose = await evaluate("window.fixture.calls.filter(call=>call.cmd==='bluetooth_pairing_cancel').length");
  await evaluate("window.fixture.hide()");
  await until(`window.fixture.calls.filter(call=>call.cmd==='bluetooth_pairing_cancel').length===${beforeClose + 1}`);

  await mount();
  await evaluate("window.fixture.deferBegin=true");
  await begin();
  await until("typeof window.fixture.releaseBegin==='function'");
  await evaluate("window.fixture.hide()");
  const beforeLate = await evaluate("window.fixture.calls.filter(call=>call.cmd==='bluetooth_pairing_cancel').length");
  await evaluate("window.fixture.releaseBegin()");
  await until(`window.fixture.calls.filter(call=>call.cmd==='bluetooth_pairing_cancel').length===${beforeLate + 1}`);
  await evaluate("window.fixture.deferBegin=false");

  await mount();
  await announce("short-lived", "012345", 1200);
  await begin();
  await until("document.querySelector('input')");
  await fill("012345");
  await evaluate("window.fixture.blockPoll=true");
  await new Promise(resolveDelay => setTimeout(resolveDelay, 1600));
  assert.equal(await evaluate("!!document.querySelector('code')"), false, "An expired local lease must hide digits even when polling is blocked");
  assert.equal(await evaluate("Array.from(document.querySelectorAll('button')).some(button=>button.textContent==='Confirm match' && !button.disabled)"), false);
  console.log("Pairing browser fixture passed: explicit start, typed code without approval, exact confirmation, cancel, close, late-start cleanup, challenge replacement, and blocked-poll expiry.");
} finally {
  page?.close();
  if (browser) { await browser.call("Browser.close").catch(() => {}); browser.close(); }
  if (chrome && chrome.exitCode === null) chrome.kill();
  await server.close();
}
