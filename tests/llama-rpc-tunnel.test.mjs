import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { createHash, randomBytes, X509Certificate } from "node:crypto";
import { once } from "node:events";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import net from "node:net";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import test, { after, before } from "node:test";
import tls from "node:tls";
import { startRpcTunnel } from "../scripts/llama-rpc-tunnel.mjs";

const loopback = "127.0.0.1";
let credentials;
let credentialDirectory;

function findOpenSsl() {
  const candidates = [process.env.CONSILIUM_TEST_OPENSSL, "openssl"];
  if (process.platform === "win32") {
    const gitPaths = spawnSync("where.exe", ["git"], { encoding: "utf8" });
    for (const executable of (gitPaths.stdout ?? "").trim().split(/\r?\n/).filter(Boolean)) {
      candidates.push(join(dirname(executable), "..", "usr", "bin", "openssl.exe"));
      candidates.push(join(dirname(executable), "..", "mingw64", "bin", "openssl.exe"));
      candidates.push(join(dirname(executable), "openssl.exe"));
    }
  }
  for (const candidate of candidates.filter(Boolean)) {
    if (spawnSync(candidate, ["version"], { stdio: "ignore" }).status === 0) return candidate;
  }
  throw new Error("Offline tunnel tests require installed OpenSSL; set CONSILIUM_TEST_OPENSSL to its executable.");
}

function makeCredentials() {
  const openssl = findOpenSsl();
  credentialDirectory = mkdtempSync(join(tmpdir(), "consilium-rpc-tls-"));
  const path = (name) => join(credentialDirectory, name);
  const run = (...args) => {
    const result = spawnSync(openssl, args, { stdio: "ignore", timeout: 15_000 });
    assert.equal(result.status, 0, "OpenSSL could not generate temporary test credentials.");
  };
  const keyArgs = ["-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:P-256", "-nodes"];
  run("req", "-x509", ...keyArgs, "-keyout", path("ca.key"), "-out", path("ca.crt"),
    "-sha256", "-days", "2", "-subj", "/CN=Consilium offline test CA",
    "-addext", "basicConstraints=critical,CA:TRUE", "-addext", "keyUsage=critical,keyCertSign,cRLSign");
  const ca = readFileSync(path("ca.crt"));
  const leaves = {};
  for (const [index, name] of ["worker", "client", "other"].entries()) {
    run("req", "-new", ...keyArgs, "-keyout", path(`${name}.key`), "-out", path(`${name}.csr`),
      "-subj", `/CN=${name}.test`);
    writeFileSync(path(`${name}.cnf`), [
      "[extensions]", "basicConstraints=critical,CA:FALSE", "keyUsage=critical,digitalSignature",
      `extendedKeyUsage=${name === "worker" ? "serverAuth" : "clientAuth"}`,
      `subjectAltName=DNS:${name === "worker" ? "tunnel.test" : `${name}.test`}`,
    ].join("\n"));
    run("x509", "-req", "-in", path(`${name}.csr`), "-CA", path("ca.crt"), "-CAkey", path("ca.key"),
      "-set_serial", String(index + 1), "-out", path(`${name}.crt`), "-sha256", "-days", "2",
      "-extfile", path(`${name}.cnf`), "-extensions", "extensions");
    const cert = readFileSync(path(`${name}.crt`));
    leaves[name] = { cert, key: readFileSync(path(`${name}.key`)),
      fingerprint: new X509Certificate(cert).fingerprint256 };
  }
  run("req", "-x509", ...keyArgs, "-keyout", path("untrusted.key"), "-out", path("untrusted.crt"),
    "-sha256", "-days", "2", "-subj", "/CN=untrusted.test",
    "-addext", "basicConstraints=critical,CA:FALSE", "-addext", "extendedKeyUsage=clientAuth");
  leaves.untrusted = { cert: readFileSync(path("untrusted.crt")), key: readFileSync(path("untrusted.key")) };
  return { ca, ...leaves };
}

before(() => { credentials = makeCredentials(); });
after(() => {
  if (credentialDirectory) rmSync(credentialDirectory, { recursive: true, force: true });
});

function deadline(promise, milliseconds = 5000, message = "Socket operation timed out.") {
  let timer;
  const timeout = new Promise((_, reject) => {
    timer = setTimeout(() => reject(new Error(message)), milliseconds);
  });
  return Promise.race([promise, timeout]).finally(() => clearTimeout(timer));
}

function closed(socket) {
  if (socket.closed) return Promise.resolve();
  return deadline(new Promise((resolve) => socket.once("close", resolve)));
}

async function rawConnection(port, allowHalfOpen = false) {
  const socket = net.connect({ host: loopback, port, allowHalfOpen });
  socket.on("error", () => {});
  await deadline(once(socket, "connect"));
  return socket;
}

async function targetServer(t, handler = (socket) => socket.resume()) {
  const sockets = new Set();
  let accepts = 0;
  let bytes = 0;
  const server = net.createServer({ allowHalfOpen: true }, (socket) => {
    accepts++;
    sockets.add(socket);
    socket.on("error", () => {});
    socket.on("data", (chunk) => { bytes += chunk.length; });
    socket.on("close", () => sockets.delete(socket));
    handler(socket);
  });
  server.listen({ host: loopback, port: 0 });
  await deadline(once(server, "listening"));
  const port = server.address().port;
  let closing;
  const close = () => closing ??= new Promise((resolve, reject) => {
    for (const socket of sockets) socket.destroy();
    server.close((error) => error ? reject(error) : resolve());
  });
  t.after(close);
  return { port, sockets, close, get accepts() { return accepts; }, get bytes() { return bytes; } };
}

function workerOptions(targetPort, overrides = {}) {
  return {
    mode: "worker", listen: { host: loopback, port: 0 }, target: { host: loopback, port: targetPort },
    cert: credentials.worker.cert, key: credentials.worker.key, ca: credentials.ca,
    peerFingerprint: credentials.client.fingerprint, handshakeMs: 1000, connectMs: 1000,
    maxConnections: 8, ...overrides,
  };
}

function clientOptions(workerPort, overrides = {}) {
  return {
    mode: "client", listen: { host: loopback, port: 0 }, target: { host: loopback, port: workerPort },
    cert: credentials.client.cert, key: credentials.client.key, ca: credentials.ca,
    peerFingerprint: credentials.worker.fingerprint.replaceAll(":", "").toLowerCase(),
    serverName: "tunnel.test", handshakeMs: 1000, connectMs: 1000, maxConnections: 8, ...overrides,
  };
}

async function tunnel(t, options) {
  const running = await startRpcTunnel(options);
  t.after(() => running.close());
  assert.equal(running.address.address, loopback);
  assert.ok(running.address.port > 0);
  return running;
}

async function pair(t, targetPort, workerOverrides = {}, clientOverrides = {}) {
  const worker = await tunnel(t, workerOptions(targetPort, workerOverrides));
  const client = await tunnel(t, clientOptions(worker.address.port, clientOverrides));
  return { worker, client };
}

async function rejectedConnection(t, clientPort) {
  const socket = await rawConnection(clientPort);
  t.after(() => socket.destroy());
  socket.resume();
  socket.write(Buffer.from("synthetic request"));
  await closed(socket);
}

function digest(buffer) {
  return createHash("sha256").update(buffer).digest("hex");
}

async function exchange(t, port, payload) {
  const socket = await rawConnection(port, true);
  t.after(() => socket.destroy());
  const response = deadline(new Promise((resolve, reject) => {
    const chunks = [];
    socket.on("data", (chunk) => chunks.push(chunk));
    socket.once("error", reject);
    socket.once("end", () => resolve(Buffer.concat(chunks)));
  }), 15_000);
  const send = async () => {
    for (let offset = 0; offset < payload.length; offset += 16_381) {
      if (!socket.write(payload.subarray(offset, offset + 16_381))) await deadline(once(socket, "drain"));
    }
    socket.end();
  };
  return Promise.all([send(), response]).then(([, received]) => received);
}

test("authenticated TLS streams preserve concurrent fragmented binary requests and half-close responses", { timeout: 30_000 }, async (t) => {
  const target = await targetServer(t, (socket) => {
    const chunks = [];
    socket.on("data", (chunk) => chunks.push(chunk));
    socket.on("end", () => socket.end(Buffer.concat(chunks).reverse()));
  });
  const { client } = await pair(t, target.port);
  const payloads = [randomBytes(3 * 1024 * 1024 + 137), randomBytes(3 * 1024 * 1024 + 251)];
  payloads[0].fill(0, 0, 16);
  payloads[0].fill(10, 16, 32);
  await Promise.all(payloads.map(async (payload) => {
    const response = await exchange(t, client.address.port, payload);
    assert.equal(response.length, payload.length);
    assert.equal(digest(response), digest(Buffer.from(payload).reverse()));
  }));
  assert.equal(target.accepts, 2);
});

test("worker rejects a valid same-CA client whose certificate pin differs before opening RPC", async (t) => {
  const target = await targetServer(t);
  const { client } = await pair(t, target.port, { peerFingerprint: credentials.other.fingerprint });
  await rejectedConnection(t, client.address.port);
  assert.equal(target.accepts, 0);
});

test("worker rejects a client certificate outside its supplied CA before opening RPC", async (t) => {
  const target = await targetServer(t);
  const { client } = await pair(t, target.port, {}, {
    cert: credentials.untrusted.cert, key: credentials.untrusted.key,
  });
  await rejectedConnection(t, client.address.port);
  assert.equal(target.accepts, 0);
});

test("worker requires a client certificate and TLS 1.3", async (t) => {
  for (const [label, overrides] of [
    ["missing client certificate", {}],
    ["TLS 1.2 client", { cert: credentials.client.cert, key: credentials.client.key, minVersion: "TLSv1.2", maxVersion: "TLSv1.2" }],
  ]) {
    await t.test(label, async (child) => {
      const target = await targetServer(child);
      const worker = await tunnel(child, workerOptions(target.port));
      const socket = tls.connect({ host: loopback, port: worker.address.port, ca: credentials.ca,
        servername: "tunnel.test", rejectUnauthorized: true, minVersion: "TLSv1.3", ...overrides });
      socket.on("error", () => {});
      child.after(() => socket.destroy());
      socket.resume();
      await closed(socket);
      assert.equal(target.accepts, 0);
    });
  }
});

test("client validates worker certificate pin, hostname and supplied CA before forwarding RPC bytes", async (t) => {
  for (const [label, overrides] of [
    ["valid CA and wrong server pin", { peerFingerprint: credentials.other.fingerprint }],
    ["valid pin and wrong server hostname", { serverName: "different.test" }],
    ["untrusted server chain", { ca: credentials.untrusted.cert }],
  ]) {
    await t.test(label, async (child) => {
      const target = await targetServer(child);
      const { client } = await pair(child, target.port, {}, overrides);
      await rejectedConnection(child, client.address.port);
      assert.equal(target.bytes, 0);
    });
  }
});

test("plaintext and unfinished TLS handshakes cannot open RPC", async (t) => {
  for (const [label, payload] of [
    ["idle handshake times out", null],
    ["actual plaintext bytes are rejected", Buffer.from("synthetic plaintext request\n")],
  ]) {
    await t.test(label, async (child) => {
      const target = await targetServer(child);
      const worker = await tunnel(child, workerOptions(target.port, { handshakeMs: 100 }));
      const socket = await rawConnection(worker.address.port);
      child.after(() => socket.destroy());
      socket.resume();
      if (payload) socket.write(payload);
      await closed(socket);
      assert.equal(target.accepts, 0);
    });
  }
});

test("connection limit rejects extra pending handshakes and shutdown releases the accepted one", async (t) => {
  let receiveHello;
  const helloReceived = new Promise((resolve) => { receiveHello = resolve; });
  const capture = await targetServer(t, (socket) => {
    const chunks = [];
    let bytes = 0;
    socket.on("data", (chunk) => {
      chunks.push(chunk);
      bytes += chunk.length;
      if (bytes < 5) return;
      const record = Buffer.concat(chunks);
      const length = 5 + record.readUInt16BE(3);
      if (record.length >= length) receiveHello(Buffer.from(record.subarray(0, length)));
    });
  });
  const helloClient = tls.connect({ host: loopback, port: capture.port, ca: credentials.ca,
    cert: credentials.client.cert, key: credentials.client.key, servername: "tunnel.test", minVersion: "TLSv1.3" });
  helloClient.on("error", () => {});
  t.after(() => helloClient.destroy());
  const clientHello = await deadline(helloReceived);
  helloClient.destroy();
  await capture.close();

  const target = await targetServer(t);
  const worker = await tunnel(t, workerOptions(target.port, { maxConnections: 1, handshakeMs: 5000 }));
  const first = await rawConnection(worker.address.port);
  t.after(() => first.destroy());
  const response = deadline(once(first, "data"));
  first.write(clientHello);
  await response;
  first.resume();
  await Promise.all([1, 2].map(async () => {
    const extra = net.connect({ host: loopback, port: worker.address.port });
    extra.on("error", () => {});
    t.after(() => extra.destroy());
    extra.resume();
    await closed(extra);
  }));
  assert.equal(first.destroyed, false);
  assert.equal(target.accepts, 0);
  await deadline(worker.close(), 1000, "Saturated worker shutdown retained a pending TLS socket.");
  await closed(first);
  assert.equal(target.accepts, 0);
});

test("a response larger than stream buffers drains completely to a slow half-closed caller", { timeout: 30_000 }, async (t) => {
  const payload = randomBytes(16 * 1024 * 1024 + 37);
  let queueResponse;
  let finishResponse;
  const queued = new Promise((resolve) => { queueResponse = resolve; });
  const finished = new Promise((resolve) => { finishResponse = resolve; });
  const target = await targetServer(t, (socket) => {
    socket.resume();
    socket.once("end", () => {
      socket.once("finish", finishResponse);
      socket.end(payload);
      queueResponse();
    });
  });
  const { client } = await pair(t, target.port);
  const socket = await rawConnection(client.address.port, true);
  t.after(() => socket.destroy());
  socket.pause();
  const response = deadline(new Promise((resolve, reject) => {
    const hash = createHash("sha256");
    let bytes = 0;
    socket.on("data", (chunk) => {
      bytes += chunk.length;
      hash.update(chunk);
      socket.pause();
      setTimeout(() => socket.resume(), 1);
    });
    socket.once("error", reject);
    socket.once("end", () => resolve({ bytes, hash: hash.digest("hex") }));
  }), 20_000, "Slow response did not finish.");
  const start = async () => {
    socket.end(Buffer.from("synthetic slow-response request"));
    await deadline(queued);
    socket.resume();
  };
  const [received] = await Promise.all([response, start(), deadline(finished, 20_000)]);
  assert.equal(received.bytes, payload.length);
  assert.equal(received.hash, digest(payload));
});

test("client closes its raw leg when a synthetic remote TLS handshake stalls", async (t) => {
  const blackhole = await targetServer(t);
  const client = await tunnel(t, clientOptions(blackhole.port, { handshakeMs: 100 }));
  await rejectedConnection(t, client.address.port);
  assert.equal(blackhole.accepts, 1);
});

test("an unavailable RPC target closes the connection instead of retaining queued bytes", async (t) => {
  const unavailable = await targetServer(t);
  await unavailable.close();
  const { client } = await pair(t, unavailable.port);
  await rejectedConnection(t, client.address.port);
});

test("a target reset ends one stream and the next connection starts fresh without replay", async (t) => {
  let requests = 0;
  const observed = [];
  const target = await targetServer(t, (socket) => {
    requests++;
    if (requests === 1) {
      socket.once("data", () => socket.destroy());
      return;
    }
    const chunks = [];
    socket.on("data", (chunk) => chunks.push(chunk));
    socket.on("end", () => {
      const payload = Buffer.concat(chunks);
      observed.push(digest(payload));
      socket.end(payload);
    });
  });
  const { client } = await pair(t, target.port);
  await rejectedConnection(t, client.address.port);
  const payload = Buffer.from("second synthetic request");
  const response = await exchange(t, client.address.port, payload);
  assert.equal(digest(response), digest(payload));
  assert.deepEqual(observed, [digest(payload)]);
  assert.equal(target.accepts, 2);
});

test("shutdown tears down incomplete handshakes and active socket legs", async (t) => {
  await t.test("worker with unfinished TLS handshake", async (child) => {
    const target = await targetServer(child);
    const worker = await tunnel(child, workerOptions(target.port));
    const socket = await rawConnection(worker.address.port);
    child.after(() => socket.destroy());
    socket.resume();
    await deadline(worker.close(), 1000, "Worker shutdown retained an unfinished TLS socket.");
    await closed(socket);
    assert.equal(target.accepts, 0);
  });
  await t.test("client with pending remote TLS handshake", async (child) => {
    let accept;
    const accepted = new Promise((resolve) => { accept = resolve; });
    const blackhole = await targetServer(child, (socket) => { socket.resume(); accept(socket); });
    const client = await tunnel(child, clientOptions(blackhole.port));
    const socket = await rawConnection(client.address.port);
    child.after(() => socket.destroy());
    socket.resume();
    socket.write(Buffer.from("synthetic request"));
    await deadline(accepted);
    await deadline(client.close(), 1000, "Client shutdown retained socket legs.");
    await closed(socket);
  });
  await t.test("worker with authenticated active streams", async (child) => {
    let accept;
    const accepted = new Promise((resolve) => { accept = resolve; });
    const target = await targetServer(child, (socket) => {
      socket.once("end", () => socket.end());
      socket.resume();
      accept(socket);
    });
    const { worker, client } = await pair(child, target.port);
    const socket = await rawConnection(client.address.port);
    child.after(() => socket.destroy());
    socket.resume();
    socket.write(Buffer.from("synthetic request"));
    const targetSocket = await deadline(accepted);
    await deadline(worker.close(), 1000, "Worker shutdown retained active socket legs.");
    await Promise.all([closed(socket), closed(targetSocket)]);
  });
});

test("configuration restricts plaintext endpoints and requires explicit usable ports and pins", async () => {
  for (const options of [
    workerOptions(12345, { target: { host: "192.168.1.2", port: 12345 } }),
    clientOptions(12345, { listen: { host: "0.0.0.0", port: 0 } }),
    clientOptions(12345, { target: { host: "localhost", port: 12345 } }),
    clientOptions(12345, { target: { host: "0.0.0.0", port: 12345 } }),
    workerOptions(0),
    clientOptions(65536),
    clientOptions(12345, { peerFingerprint: "abcd" }),
    clientOptions(12345, { serverName: "" }),
  ]) {
    await assert.rejects(startRpcTunnel(options));
  }
});
