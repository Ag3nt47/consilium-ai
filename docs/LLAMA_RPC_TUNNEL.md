# Temporary RPC tunnel for split-model trials

`scripts/llama-rpc-tunnel.mjs` carries llama.cpp RPC bytes over a separate,
mutually authenticated TLS connection. It uses Node.js built-ins and does not
install a runner, load weights, register a council model or change app settings.
It does not use PeerLink's persistent message queue or its pairing keys. A
standalone stream avoids replaying stateful RPC commands after reconnection.

The [upstream RPC backend](https://github.com/ggml-org/llama.cpp/blob/b12681/tools/rpc/README.md)
is experimental. Keep the raw worker bound to `127.0.0.1`; only the authenticated
TLS listener can bind a specific private IPv4 address. On the inference host,
the helper also exposes raw RPC only at `127.0.0.1`. Other processes on either
computer can access its local loopback endpoint.

## Credentials and endpoints

Provision a separate TLS identity on each computer before a physical trial.
Keep every private key on the computer where it was generated. Exchange only
the public CA/certificates and verify each leaf certificate's SHA256 fingerprint
through a trusted channel. This helper requires explicit CA, certificate, key
and peer fingerprint files/values; it neither generates credentials nor copies
Signal or PeerLink secrets. Certificate provisioning on the two physical hosts
has not been performed by this change.

The worker checks both the client's CA chain and its exact leaf fingerprint
before opening its fixed loopback RPC target. The client checks the worker's CA
chain, DNS certificate name and fingerprint before forwarding RPC data. TLS 1.3
is required. These options use the standard [Node.js TLS API](https://nodejs.org/api/tls.html).

The following are argument templates, not ready-to-run host commands. Replace
all endpoints and credential paths only after coordinating the physical trial.

```text
# Worker computer: ggml-rpc-server already listens on 127.0.0.1:50052.
node scripts/llama-rpc-tunnel.mjs --mode worker --listen WORKER_PRIVATE_IP:50053 --target 127.0.0.1:50052 --cert WORKER_CERT --key WORKER_KEY --ca CA_CERT --peer-fingerprint INFERENCE_CERT_SHA256

# Inference computer: expose the remote worker as a local RPC endpoint.
node scripts/llama-rpc-tunnel.mjs --mode client --listen 127.0.0.1:50054 --target WORKER_PRIVATE_IP:50053 --server-name WORKER_CERT_DNS_NAME --cert INFERENCE_CERT --key INFERENCE_KEY --ca CA_CERT --peer-fingerprint WORKER_CERT_SHA256
```

For a single client route, optional `--source LOCAL_PRIVATE_IP` binds the TLS
socket to that local address. An unavailable source fails that connection; the
helper does not silently use the default interface instead. The worker listener
still binds its explicit `--listen` address. Neither option changes OS routes,
interface metrics, Wi-Fi settings or firewall rules.

For independently selected Ethernet and Wi-Fi paths, use client `--routes FILE`
instead of `--target`, `--source`, `--server-name` and `--peer-fingerprint`.
The JSON file contains one to four objects with this shape (replace the
placeholders with coordinated, measured routes and verified identities):

```json
[
  {
    "target": { "host": "10.1.0.2", "port": 50053 },
    "source": "10.1.0.1",
    "serverName": "worker.example",
    "peerFingerprint": "WORKER_CERT_SHA256"
  },
  {
    "target": { "host": "192.168.2.2", "port": 50053 },
    "source": "192.168.2.1",
    "serverName": "worker.example",
    "peerFingerprint": "WORKER_CERT_SHA256"
  }
]
```

Each route pins its literal private IPv4 endpoint, certificate DNS name and leaf
fingerprint. The client certificate and CA remain shared listener options. The
entire list is validated before listening. `source` is optional, but a physical
interface claim requires an explicit source and separate measured receiver
evidence; a target IP alone does not prove which adapter carried the traffic.
The programmatic API accepts this array as `routes`, or `source` with the
existing single-target options.

All alternatives in one client tunnel must reach the **same logical llama.cpp
worker instance** and its runtime object state. A certificate pin establishes
identity, not interchangeable RPC handles or contexts. Use separate client
tunnels and separate loopback RPC endpoints for separate workers; register each
with the runner independently. Do not put distinct worker processes behind one
route list, even if they use the same certificate.

Each new raw RPC stream chooses the route with the fewest active attempts or
streams, breaking ties by configured order. Input remains paused until TLS, the
selected route's pin, actual remote endpoint and explicit source are verified.
Only an attempt that fails before authenticated TLS readiness may try another
configured route. Each candidate is tried at most once per new stream. A failed
candidate cools down for one monotonic second (`routeCooldownMs` in the API is
bounded to 1–60,000 ms). If all candidates are cooling down, the caller closes
promptly. There is no discovery, unlisted fallback or persistent health cache;
later new streams can retry after cooldown expiry. Single-target clients keep
their existing behavior: each new caller gets a fresh attempt, with no cooldown.

Once a route authenticates, any later failure closes that stream's two legs.
No request or partial tensor data is replayed, migrated or striped onto another
route, even if failure occurs before the first RPC byte. Concurrent streams can
use separate paths for independent calculations. A single ordered tensor/RPC
session stays on its chosen connection and needs a fresh runner session after
failure. Events expose only the zero-based route index, source-bound boolean and
error category, without addresses, certificates or RPC contents. Closing the
tunnel cancels pending candidates and active streams.

Listeners accept at most eight connections by default. TLS handshakes and target
connections time out after five seconds. Established streams have no short idle
deadline: model loading and inference can remain quiet for longer. Stream pipes
apply backpressure and preserve half-close so a worker can finish its response
after a caller ends its write side. Ctrl+C closes the listener and all socket
legs. Connection/authentication failures close that stream; no partial request
is replayed. The helper logs event types and error codes, without RPC contents,
certificate details or private-key paths.

## Placement and acceptance

With both helpers and the worker running, capture the inference host's actual
`--rpc 127.0.0.1:50054 --list-devices` output. Supply that inventory to
`scripts/llama-split-args.mjs` with `--rpc-tunnel "Mac=127.0.0.1:50054"` in the
same registration order. Select runner devices from this inventory, rather than
assuming a GPU or RPC device name. The coordinated trial targets llama.cpp
b12681 on both hosts; an installed binary or a device list alone does not verify
that a remote worker can load weights or complete generation.

Allocate one model at a time from fresh available memory after OS, context and
runtime reserves. Integrated GPU shared memory is part of host RAM, not extra
capacity. A 50/50 comparison is useful only when each host can hold its share.

Local synthetic tests validate authentication and stream behavior. They do not
establish two-computer inference, physical Ethernet/Wi-Fi/Bluetooth operation,
automatic transport failover, or restoration of Llama 3/Qwen to the council.
For those claims, retain the evidence required by
[the physical trial](PEERLINK_PHYSICAL_TRIAL.md), including nonzero weight loads
on both computers and completed generation. This TCP helper needs an IP route;
it does not adapt a native Bluetooth RFCOMM byte stream. Bluetooth control can
remain on PeerLink after separately verified native connectivity. The helper
does not assign production route roles or launch concurrent model jobs by itself.
A dropped established route requires a fresh runner session rather than
assuming live inference can resume.

## Offline verification

Run `node --test tests/llama-rpc-tunnel.test.mjs`. The tests use synthetic local
TCP services and temporary certificates; no model process or physical peer is
contacted. Certificate fixtures require an existing OpenSSL executable. Set
`CONSILIUM_TEST_OPENSSL` when it is not on PATH; this does not install software.
