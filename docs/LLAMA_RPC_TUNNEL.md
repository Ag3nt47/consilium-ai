# Temporary RPC tunnel for split-model trials

`scripts/llama-rpc-tunnel.mjs` carries llama.cpp RPC bytes over a separate,
mutually authenticated TLS connection. It uses Node.js built-ins and does not
install a runner, load weights, register a council model or change app settings.
It does not use PeerLink's persistent message queue or its pairing keys. A
standalone stream avoids replaying stateful RPC commands after reconnection.

The [upstream RPC backend](https://github.com/ggml-org/llama.cpp/blob/b12621/tools/rpc/README.md)
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
assuming a GPU or RPC device name. Use matching llama.cpp builds on both hosts.

Allocate one model at a time from fresh available memory after OS, context and
runtime reserves. Integrated GPU shared memory is part of host RAM, not extra
capacity. A 50/50 comparison is useful only when each host can hold its share.

Local synthetic tests validate authentication and stream behavior. They do not
establish two-computer inference, physical Ethernet/Wi-Fi/Bluetooth operation,
automatic transport failover, or restoration of Llama 3/Qwen to the council.
For those claims, retain the evidence required by
[the physical trial](PEERLINK_PHYSICAL_TRIAL.md), including nonzero weight loads
on both computers and completed generation. This TCP helper needs an IP route;
it does not adapt a native Bluetooth RFCOMM byte stream. A dropped route requires
a fresh runner session rather than assuming live inference can resume.

## Offline verification

Run `node --test tests/llama-rpc-tunnel.test.mjs`. The tests use synthetic local
TCP services and temporary certificates; no model process or physical peer is
contacted. Certificate fixtures require an existing OpenSSL executable. Set
`CONSILIUM_TEST_OPENSSL` when it is not on PATH; this does not install software.
