import { createHash, timingSafeEqual } from "node:crypto";
import { readFileSync, statSync } from "node:fs";
import { createConnection, createServer, isIP } from "node:net";
import { resolve } from "node:path";
import { performance } from "node:perf_hooks";
import { connect as connectTls, createSecureContext, createServer as createTlsServer } from "node:tls";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";

// An explicit, temporary TLS stream for llama.cpp RPC trials. This does not use
// PeerLink's durable message queue, copy its pairing keys, or replay RPC bytes.
const LOOPBACK = "127.0.0.1";
const MAX_ROUTES = 4;

function privateAddress(host) {
  if (isIP(host) !== 4) return false;
  const [a, b] = host.split(".").map(Number);
  return a === 127 || a === 10 || (a === 172 && b >= 16 && b <= 31) ||
    (a === 192 && b === 168) || (a === 169 && b === 254) ||
    (a === 100 && b >= 64 && b <= 127);
}

function endpoint(value, listening = false) {
  if (!value || !privateAddress(value.host) || !Number.isInteger(value.port) ||
    value.port < (listening ? 0 : 1) || value.port > 65535) {
    throw new Error("Use an explicit private IPv4 address and valid port.");
  }
  return { host: value.host, port: value.port };
}

function fingerprint(value) {
  if (typeof value !== "string" || !/^(?:[a-f\d]{64}|(?:[a-f\d]{2}:){31}[a-f\d]{2})$/i.test(value)) {
    throw new Error("Supply the expected peer certificate SHA256 fingerprint.");
  }
  return Buffer.from(value.replaceAll(":", ""), "hex");
}

function pinned(socket, expected) {
  const raw = socket.getPeerCertificate()?.raw;
  return socket.authorized && Buffer.isBuffer(raw) &&
    timingSafeEqual(createHash("sha256").update(raw).digest(), expected);
}

function positive(value, fallback, limit) {
  const result = value ?? fallback;
  if (!Number.isInteger(result) || result <= 0 || result > limit) {
    throw new Error("Timeouts and connection limits must be positive bounded integers.");
  }
  return result;
}

function serverName(value) {
  if (typeof value !== "string" || value.length > 253 ||
    !/^[a-z\d](?:[a-z\d.-]*[a-z\d])?$/i.test(value) || isIP(value)) {
    throw new Error("Supply the worker certificate DNS name with server-name.");
  }
  return value;
}

function clientRoutes(options, listen) {
  if (options.routes !== undefined && [options.target, options.source, options.serverName, options.peerFingerprint]
    .some((value) => value !== undefined)) {
    throw new Error("Use routes or the single-target options, not both.");
  }
  const rows = options.routes ?? [{ target: options.target, source: options.source,
    serverName: options.serverName, peerFingerprint: options.peerFingerprint }];
  if (!Array.isArray(rows) || rows.length < 1 || rows.length > MAX_ROUTES) {
    throw new Error("Supply one to four explicit worker routes.");
  }
  const seen = new Set();
  return rows.map((row, index) => {
    const target = endpoint(row?.target);
    if (listen.host === target.host && listen.port === target.port) {
      throw new Error("Listener and target endpoints must be different.");
    }
    const source = row.source;
    if (source !== undefined && !privateAddress(source)) {
      throw new Error("Use an explicit private IPv4 source address.");
    }
    const identity = `${source ?? ""}|${target.host}:${target.port}`;
    if (seen.has(identity)) throw new Error("Worker routes must have distinct source and target pairs.");
    seen.add(identity);
    return { target, source, serverName: serverName(row.serverName), expected: fingerprint(row.peerFingerprint),
      index, active: 0, retryAfter: 0 };
  });
}

export async function startRpcTunnel(options) {
  const { mode, cert, key, ca } = options;
  if (mode !== "worker" && mode !== "client") throw new Error("Select worker or client mode.");
  const listen = endpoint(options.listen, true);
  if (mode === "worker" && (options.routes !== undefined || options.source !== undefined)) {
    throw new Error("Source binding and alternative routes are client options.");
  }
  const target = mode === "worker" ? endpoint(options.target) : null;
  const routes = mode === "client" ? clientRoutes(options, listen) : [];
  if (target && listen.host === target.host && listen.port === target.port) {
    throw new Error("Listener and target endpoints must be different.");
  }
  if ((mode === "client" ? listen.host : target.host) !== LOOPBACK) {
    throw new Error("The unencrypted RPC endpoint must be exactly 127.0.0.1.");
  }
  if ([cert, key, ca].some((value) => !Buffer.isBuffer(value) || !value.length || value.length > 65536)) {
    throw new Error("Supply certificate, private key and CA buffers, each up to 64 KiB.");
  }
  const expected = mode === "worker" ? fingerprint(options.peerFingerprint) : null;
  const handshakeMs = positive(options.handshakeMs, 5000, 60000);
  const connectMs = positive(options.connectMs, 5000, 60000);
  const maxConnections = positive(options.maxConnections, 8, 64);
  const routeCooldownMs = positive(options.routeCooldownMs, 1000, 60000);
  const sockets = new Set();
  let closed = false;
  let closing;
  const report = (type, error, details = {}) => {
    const code = /^E[A-Z\d_]{1,60}$/.test(error?.code ?? "") ? error.code : "TUNNEL_IO";
    // Do not print error messages, certificate contents, file paths or RPC data.
    try { options.onEvent?.({ type, ...(error ? { code } : {}), ...details }); } catch {}
  };
  const track = (socket) => {
    if (sockets.has(socket)) return socket;
    sockets.add(socket);
    socket.once("close", () => sockets.delete(socket));
    socket.on("error", () => {});
    socket.setNoDelay(true);
    return socket;
  };
  const bridge = (incoming, outgoing, ready) => {
    track(outgoing);
    let timer;
    const fail = (error) => {
      clearTimeout(timer);
      if (error) report("connection-error", error);
      incoming.destroy();
      outgoing.destroy();
    };
    incoming.once("error", fail);
    outgoing.once("error", fail);
    const onClose = (socket) => {
      clearTimeout(timer);
      // A clean source close can precede the destination's final write flush.
      // Destroying that destination here would truncate a backpressured tail.
      if (!socket.readableEnded || !socket.writableFinished) fail();
    };
    incoming.once("close", () => onClose(incoming));
    outgoing.once("close", () => onClose(outgoing));
    const connected = () => {
      clearTimeout(timer);
      if (closed || incoming.destroyed || outgoing.destroyed) return fail();
      // pipe applies backpressure; allowHalfOpen preserves a delayed response to
      // a request that ended its write side. Never retry a partial RPC request.
      incoming.pipe(outgoing);
      outgoing.pipe(incoming);
      incoming.resume();
      report("connected");
    };
    if (ready) {
      timer = setTimeout(() => fail(Object.assign(new Error(), { code: "ETIMEDOUT" })), connectMs);
      timer.unref();
      outgoing.once(ready, connected);
    } else connected();
  };
  const tls = { cert, key, ca, minVersion: "TLSv1.3", rejectUnauthorized: true, allowHalfOpen: true };
  // Validate once before listening, and reuse the client context. A malformed
  // identity must not throw from a later connection callback.
  const secureContext = createSecureContext(tls);
  const dial = (incoming, route) => new Promise((accept, reject) => {
    const outgoing = track(connectTls({ ...tls, secureContext, ...route.target,
      servername: route.serverName, ...(route.source ? { localAddress: route.source } : {}) }));
    let settled = false;
    let timer;
    const cleanup = () => {
      clearTimeout(timer);
      incoming.removeListener("close", cancelled);
      outgoing.removeListener("error", failed);
      outgoing.removeListener("close", ended);
      outgoing.removeListener("secureConnect", authenticated);
    };
    const finish = (error) => {
      if (settled) return;
      settled = true;
      cleanup();
      if (error) { outgoing.destroy(); reject(error); }
      else accept(outgoing);
    };
    const failed = (error) => finish(error);
    const cancelled = () => finish(Object.assign(new Error(), { code: "ECANCELED" }));
    const ended = () => finish(Object.assign(new Error(), { code: "ECONNRESET" }));
    const authenticated = () => {
      if (!pinned(outgoing, route.expected)) {
        report("peer-rejected", undefined, { route: route.index });
        return finish(Object.assign(new Error(), { code: "EPEERREJECTED" }));
      }
      const address = (value) => value?.replace(/^::ffff:/, "");
      if (address(outgoing.remoteAddress) !== route.target.host || outgoing.remotePort !== route.target.port ||
        (route.source && address(outgoing.localAddress) !== route.source)) {
        return finish(Object.assign(new Error(), { code: "EROUTEMISMATCH" }));
      }
      finish();
    };
    incoming.once("close", cancelled);
    outgoing.once("error", failed);
    outgoing.once("close", ended);
    outgoing.once("secureConnect", authenticated);
    timer = setTimeout(() => finish(Object.assign(new Error(), { code: "ETIMEDOUT" })), handshakeMs);
    timer.unref();
  });
  const connectClient = async (incoming) => {
    const tried = new Set();
    while (!closed && !incoming.destroyed) {
      const route = routes.filter((row) => !tried.has(row.index) && row.retryAfter <= performance.now())
        .sort((left, right) => left.active - right.active || left.index - right.index)[0];
      if (!route) break;
      tried.add(route.index);
      route.active++;
      let outgoing;
      try { outgoing = await dial(incoming, route); }
      catch (error) {
        route.active--;
        if (closed || incoming.destroyed) return;
        // Preserve the existing single-target contract: each new caller gets
        // a fresh attempt. Cooldowns only balance multiple explicit paths.
        route.retryAfter = performance.now() + (routes.length > 1 ? routeCooldownMs : 0);
        report("route-attempt-failed", error, { route: route.index });
        continue;
      }
      // TLS identity and actual endpoints are now verified. No later failure
      // may select another route, even if no application bytes have flowed yet.
      route.retryAfter = 0;
      outgoing.once("close", () => { route.active--; });
      if (closed || incoming.destroyed) { outgoing.destroy(); return; }
      report("route-selected", undefined, { route: route.index, source_bound: Boolean(route.source) });
      bridge(incoming, outgoing, null);
      return;
    }
    incoming.destroy();
  };
  const server = mode === "worker"
    ? createTlsServer({ ...tls, requestCert: true, handshakeTimeout: handshakeMs }, (secure) => {
      track(secure);
      secure.pause();
      if (closed || !pinned(secure, expected)) {
        report("peer-rejected");
        secure.destroy();
        return;
      }
      bridge(secure, createConnection({ ...target, allowHalfOpen: true }), "connect");
    })
    : createServer({ allowHalfOpen: true, pauseOnConnect: true }, (local) => {
      if (closed) return local.destroy();
      void connectClient(local).catch((error) => { report("connection-error", error); local.destroy(); });
    });
  server.maxConnections = maxConnections;
  server.on("connection", track);
  server.on("tlsClientError", (error, socket) => {
    report("handshake-rejected", error);
    socket.destroy();
  });
  server.on("error", (error) => report("listener-error", error));
  await new Promise((accept, reject) => {
    server.once("error", reject);
    server.listen(listen, () => { server.removeListener("error", reject); accept(); });
  });
  return {
    address: server.address(),
    close() {
      closing ??= new Promise((accept) => {
        closed = true;
        server.close(() => accept());
        for (const socket of sockets) socket.destroy();
      });
      return closing;
    },
  };
}

function file(path) {
  if (!path || !statSync(path).isFile() || statSync(path).size > 65536) {
    throw new Error("Select a regular TLS credential file up to 64 KiB.");
  }
  return readFileSync(path);
}

function address(value) {
  const match = /^(\d+\.\d+\.\d+\.\d+):(\d{1,5})$/.exec(value ?? "");
  if (!match) throw new Error("Use an explicit IPv4:port endpoint.");
  return { host: match[1], port: Number(match[2]) };
}

function routesFile(path) {
  if (!statSync(path).isFile() || statSync(path).size > 65536) throw new Error("Choose a route JSON file up to 64 KiB.");
  const bytes = readFileSync(path);
  if (bytes.length > 65536) throw new Error("Choose a route JSON file up to 64 KiB.");
  return JSON.parse(bytes.toString("utf8"));
}

async function main() {
  const { values } = parseArgs({ options: {
    help: { type: "boolean" }, mode: { type: "string" }, listen: { type: "string" },
    target: { type: "string" }, cert: { type: "string" }, key: { type: "string" },
    ca: { type: "string" }, "peer-fingerprint": { type: "string" }, "server-name": { type: "string" },
    source: { type: "string" }, routes: { type: "string" },
  } });
  if (values.help) {
    console.log("Temporary llama.cpp RPC TLS tunnel; Node.js only, no model or app changes.");
    console.log("Required: --mode worker|client --listen IPv4:port --target IPv4:port --cert FILE --key FILE --ca FILE --peer-fingerprint SHA256");
    console.log("Client also requires --server-name DNS_NAME. Client listen and worker target must be 127.0.0.1.");
    console.log("Client --source IPv4 pins one local source. Alternatively --routes FILE supplies 1–4 individually pinned route objects, replacing target/source/server-name/peer-fingerprint.");
    console.log("Routes must reach one logical worker; use separate client tunnels for separate workers. A NEW stream may retry a listed route before TLS authentication. Established streams never fail over or replay.");
    console.log("Use separately provisioned TLS credentials and verify both certificate fingerprints out of band. No PeerLink integration.");
    return;
  }
  if (values.routes && [values.target, values.source, values["peer-fingerprint"], values["server-name"]].some((value) => value !== undefined)) {
    throw new Error("Use routes or the single-target options, not both.");
  }
  const routeOptions = values.routes ? { routes: routesFile(values.routes) } : {
    target: address(values.target), source: values.source,
    peerFingerprint: values["peer-fingerprint"], serverName: values["server-name"],
  };
  const tunnel = await startRpcTunnel({
    mode: values.mode, listen: address(values.listen), ...routeOptions,
    cert: file(values.cert), key: file(values.key), ca: file(values.ca),
    onEvent: (event) => console.error(JSON.stringify(event)),
  });
  console.log(JSON.stringify({ type: "ready", mode: values.mode, listen: tunnel.address }));
  for (const signal of ["SIGINT", "SIGTERM"]) process.once(signal, () => { void tunnel.close(); });
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch((error) => {
    // TLS parser errors and filesystem errors can include sensitive input.
    const code = /^E[A-Z\d_]{1,60}$/.test(error?.code ?? "") ? error.code : "INVALID_CONFIGURATION";
    console.error(`RPC tunnel could not start (${code}); check --help and the TLS files.`);
    process.exitCode = 1;
  });
}
