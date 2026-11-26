# Windows–Mac PeerLink physical failover trial

Use the same Consilium build on both computers. Open a dedicated shared chat and keep model runners idle. Perform the outage steps with local access to both computers so you can restore an interface independently of PeerLink.

1. Connect both computers by Ethernet and join a trusted Wi-Fi network on both. In Settings → Linked computers, verify that both peers report connected and have separate Ethernet and Wi-Fi addresses configured. Record the build on each computer.
2. Run **Test Connection** on both computers to check endpoint authentication and record the local adapters. If the build offers **Test from a local adapter**, select the adapter's source address and run **Test Connection** again. This binds the probe to the source address and leaves the live session in place. Then run **Communication stress test** to verify traffic after switching endpoints. If the build offers **Test target**, select the computer under test to keep other peer sessions out of the exercise. In **Try local source addresses**, select the Ethernet, Wi-Fi or Bluetooth PAN addresses you want to try in order. Record the resulting authenticated traffic at each source and whether the default route was restored. Multiple reachable addresses can share one physical route.
3. With both computers connected, use an established PeerLink session over Ethernet as the baseline. On Windows, map the established socket's local address to its adapter. On macOS, record the local address from the established socket and map it to the interface with the active address.
4. Disconnect Ethernet on one computer. Record the time and send a unique message from that computer while the session is reconnecting. Preserve the send and receive timestamps and count the matching messages on the receiving computer. Verify the send queue drains and confirm that the re-established socket uses Wi-Fi.
5. Restore Ethernet and repeat with Wi-Fi disconnected and Ethernet available. Start this leg with an active Wi-Fi session, and send the queued message from the other computer. Restore the original interface state after the run.

Before switching live routes, choose one computer under **Test target** and select addresses
on at least two adapters under **Try local source addresses**. Run **Check handshakes in parallel**
to start independent authenticated probes without replacing the live session. Record which
local addresses and peer endpoints authenticate. These short probes are a preflight for the
outage and simultaneous payload trials below.

Windows socket and interface inspection:

```powershell
Get-NetTCPConnection -State Established |
  Where-Object { $_.LocalPort -eq 8470 -or $_.RemotePort -eq 8470 } |
  ForEach-Object {
    $socket = $_
    Get-NetIPAddress -IPAddress $socket.LocalAddress |
      Select-Object InterfaceAlias, IPAddress,
        @{Name='RemoteAddress'; Expression={$socket.RemoteAddress}}
  }
```

macOS socket and interface inspection:

```sh
lsof -nP -iTCP:8470 -sTCP:ESTABLISHED
ifconfig
```

Record each leg with the initial and recovered interfaces on both computers, outage and recovery times, sender, received message count, and final queue state. Redact addresses and device names before publishing screenshots or logs. A leg passes when the live session recovers over the alternate physical interface, the receiver sees the marker once, and the sender confirms queue drainage. Run Bluetooth PAN as a separate leg if both computers expose an authenticated IP path over it. Native BLE and simultaneous payload transfer over multiple links require separate trials.


## Live Wi-Fi check on November 23, 2026

The installed Windows Consilium 0.3.56 app ran the communication stress test against the paired
Mac. Both saved endpoints returned traffic after individual switches (1,843 ms and 1,616 ms).
The Windows app returned to a live Ethernet session.

With Windows Wi-Fi temporarily connected to the same network, a short PeerLink session bound to
the Windows Wi-Fi address authenticated the paired Mac in 63 ms and received a fresh encrypted
heartbeat in 78 ms. The separate session was closed, and Windows Wi-Fi was returned to standby
with the installed app connected over Ethernet. The Mac-side receiving interface was not measured.

The Windows Bluetooth radio was on. An active Classic Bluetooth inquiry discovered the MacBook Pro.
Windows reported it as unpaired and disconnected. The PAN adapter was disconnected and had no
IP neighbors. An Ethernet outage, Wi-Fi automatic recovery, and
simultaneous payload transfer on multiple adapters remain separate trial steps.

### Repeat check on November 23, 2026

Both saved Mac endpoints returned a fresh authenticated hello and encrypted heartbeat when
the Windows diagnostic bound its TCP connection to each local adapter in turn.

| Windows source | Authenticated hello | Encrypted heartbeat |
| --- | ---: | ---: |
| Ethernet | 17 and 22 ms | 19 and 24 ms |
| Wi-Fi | 10 and 8 ms | 14 and 11 ms |

Windows had weak-host send and receive disabled on both interfaces. The receiving Mac
adapter was not sampled. Windows restored Wi-Fi to on and disconnected and later showed
an established PeerLink socket over Ethernet.

A separate Classic Bluetooth inquiry found the Mac and reported it as unpaired and
disconnected. The Windows Bluetooth network adapter had no usable IPv4 address. Its
data transfer test needs device pairing and an active Bluetooth network on both computers.
The separate address-bound checks provide no result for simultaneous transfer or physical
failover.

## Preview build for the physical trial

The [draft preview run](https://github.com/Ag3nt47/consilium-ai/actions/runs/45307151629#artifacts)
built commit `a00a4c8` with sequential source switching, parallel handshakes, authenticated
payload transfer tests, and the Bluetooth PAN setup guide.
Hosted installation and desktop launch checks passed on Windows, macOS and Linux. The macOS job
verified the Apple Silicon and Intel executable slices. The Linux job exercised DEB and AppImage
launches on Ubuntu and an RPM installation and launch on Fedora. The workflow skipped publication.
All three downloaded archives matched the SHA-256 digests published by GitHub.

| Platform | Draft archive | GitHub archive SHA-256 |
| --- | --- | --- |
| Windows | [Windows EXE](https://github.com/Ag3nt47/consilium-ai/actions/runs/45307151629/artifacts/14880509896) | `9322e4a662c380c7399709c4725003d6ad6922696d9f13159354abf47f8f69ed` |
| macOS | [Universal DMG](https://github.com/Ag3nt47/consilium-ai/actions/runs/45307151629/artifacts/14881847623) | `7af686aeb7c4ce8f7630fae313a96368ca7437cd1b34beed411481ef168a7192` |
| Linux | [DEB, RPM and AppImage](https://github.com/Ag3nt47/consilium-ai/actions/runs/45307151629/artifacts/14881089369) | `81c57e1be13eee3cb8840a0f4206817e7287331449a0e631f86b33607014ddbc` |

Use the matching Windows and macOS archives for the connection handoff trial. Use the socket
commands above to confirm which adapters carry the active session during the outage. Live
Ethernet, Wi-Fi and Bluetooth behavior still needs the physical trial.

In **Multi-Computer Sync**, click **Test AI handshake** on each connected peer. Record whether a
fresh council report arrives and how many AIs have confirmations in both directions.
The check uses existing AI confirmations and sends no new prompts. In the same menu, open
**Check local model memory** to scan for fresh peer capacity reports.

## Bluetooth verification

### Native Bluetooth between Windows and Mac

Use matching installed builds that include native Bluetooth. Keep the existing
Ethernet PeerLink connection while enabling **Native Bluetooth · Windows and Mac**
in Consilium's linked-computer settings on both hosts. Allow macOS Bluetooth access
when requested. On Windows, open the linked Mac's card and choose **Show Bluetooth code**.
Compare the Windows number with the macOS pairing prompt, type the Mac's number into
**Enter Mac code**, and confirm on both computers. The device pairing prompt is separate
from the code generated when first linking the computers in Consilium.
The Mac announces its RFCOMM endpoint only after its transient
service is published; the Windows peer learns it through authenticated heartbeats.
This route does not require a Bluetooth PAN network or an IP address.

On Windows, select **Native Bluetooth** under **Connection and AI handshake route**
and run **Test Connection**, then **Test AI handshake**. Select the Mac as the
communication test target, choose **Native Bluetooth** under **Routes to test**,
and run **Communication stress test**. Record fresh authentication and traffic,
actual Bluetooth adapter identities, service UUID and the discovered channel.
The AI check requests a fresh authenticated status report on its own native connection
while the existing Ethernet session continues. Verify the default route after the
switch stress test, including failed or cancelled checks.

On November 24, 2026, cycling the Windows Bluetooth radio restored the operating system's
numeric comparison prompt. Both computers showed the bond after the attended confirmation.
Windows recorded the remote adapter as authenticated, remembered and connected after the
diagnostic pairing request had ended. The follow-up native data probe on both installed
0.3.58 build #606 apps still reported the RFCOMM route as refused or unreachable and returned
no channel evidence. Ethernet continued carrying the session. Windows Wi-Fi was disconnected.
The native AI handshake and switch stress tests remained pending after the failed data probe.
An encrypted Windows RFCOMM socket reached the connect completion event with error `10049`.
An uncached service-directory lookup reached the selected Mac and found an L2CAP record, while
the lookup for the Consilium service UUID returned no result. The Mac listener now includes the
service ID and public browse group in its SDP record. Repeat the targeted lookup on the updated
Mac before treating the physical Bluetooth data route as verified.

After both apps updated to 0.3.58 build #626, an uncached Windows SDP lookup found the
Consilium service on the paired Mac and reported RFCOMM channel 1. Two secured socket
probes reached the correct radio. The service-UUID dial reported port 0 through
`getpeername()`. The dial to the freshly resolved channel reported port 1 through the
same API. Consilium's connection test stopped at the port-0 validation check.

The Windows dial now resolves the service on the selected radio before opening the
authenticated and encrypted socket to its explicit channel. Its route evidence requires
the connected socket to report the same radio and channel. Run the three app tests on an
updated matching preview to verify PeerLink over this physical route.

For parallel payload trials, choose **Native Bluetooth** as one path's local
adapter and match its RFCOMM endpoint. Native paths have no source/receiver IP;
receipts must identify the reversed radio endpoints and service/channel instead.
Verify checksum receipts, distinct receiving adapters and overlapping receipt
windows across the selected transports. Repeat the outage-and-single-delivery
steps above with Bluetooth as the surviving route. Pairing, type checks and
loopback tests alone do not establish this physical result.

### Optional Bluetooth PAN

Pair the computers in their operating systems and establish a Bluetooth Personal Area Network
(PAN) with an IP address on each end. The Consilium pairing guide under Multi-computer sync
links to the platform setup instructions. Add the peer's PAN address if it is missing from the
peer card, then open **Test from a local adapter** and choose **Refresh local sources**. Select
the local PAN address and run **Test Connection**. An authenticated result from that source to
the peer's PAN address is required before an outage or simultaneous transfer trial.

## Simultaneous transfer trial

After the individual routes pass, test with Ethernet, Wi-Fi and the verified Bluetooth path active. Open
**Test parallel payload transfer**, choose one computer, then match each local adapter with
its peer endpoint. Use both computers' network settings to confirm the selected addresses
belong to distinct physical adapters. Run the test and record the bound source interface,
receiver IP and mapped adapter, checksum-verified bytes and receipt window for each path. Consilium
sends four independently authenticated 8 KiB chunks per selected path and checks their
SHA-256 receipts. The overlap is the shared first-to-last receipt window on the same receiving
process, with a checksum-verified sample from each path inside that window. This bounded
diagnostic can run alongside the live session.

Use the reported receiving adapter names to check the endpoint mapping. The receiving
computer supplies those names from its socket addresses. Use OS measurements at both ends
to establish which physical interface actually carried the traffic.

The report ranks completed IP paths by median authenticated sample time and suggests a primary
calculation route and separate paths for independent jobs or fallback. It suggests a verified
native Bluetooth path for small coordination messages and recovery. Each suggested IP route
uses a different receiving adapter. Sample time includes connection setup and authentication.
The report measures and recommends routes while leaving the live session in place.

For a throughput comparison, compare the three-path run against each single-path baseline
with the same payload and computers. To pass the physical overlap trial, authenticated payload
bytes must arrive over each claimed path during overlapping receipt windows. Repeat while
disconnecting each path in turn. Check payload integrity and record whether the trial returned
a partial result. Live chat and model traffic use their own routing and recovery logic.

### Live dual-route check on November 24, 2026

The Windows and Mac apps both ran 0.3.58 build #606. Windows joined the previously tested Wi-Fi
network while Ethernet stayed active. The transfer test returned all four checksum receipts on
each route, totaling 32 KiB per route. The Mac reported distinct receiving adapters and 258 ms
of overlap between authenticated payload receipts. The existing PeerLink session remained on
Ethernet throughout the parallel transfer. This run measured payloads over two IP routes.

The source-bound communication stress test then confirmed authenticated traffic after switching
to each Windows adapter. Ethernet switched in 107 ms and confirmed traffic in 1,625 ms. Wi-Fi
switched in 109 ms and confirmed traffic in 1,639 ms. The default route reconnected in 104 ms,
confirmed traffic in 1,638 ms, and the live link returned to Ethernet. Windows Wi-Fi went back
to its original disconnected state. The established PeerLink socket stayed on Ethernet.

## Split-model inference trial

With permission to start the runners, use the same compatible GGUF and runner build for each comparison. Record usable memory after runtime reserves, planned weight allocation, and actual weight and KV-cache allocation reported by every worker. Verify that each worker participating in the split actually loaded weights and contributed to processing the request.

### Memory preflight

Use **Set runtime allowances by device** if the runner or context uses different amounts
of memory on Windows and Mac. From the CLI, an override such as
`--device-reserve "Mac=3"` sets the allowance for that device in GiB. A capacity-weighted
plan allocates each share from the remaining weight budget. Recheck the allowances against
runner logs during the live trial.

Use fresh paired-computer reports before allocating weights. In the last Mac scan, the Mac returned 951 MiB out of 8192 MiB over PeerLink. The default 2 GiB runtime allowance left it with zero weight capacity. A separate, later Windows reading showed 3.53 GiB free.

With that allowance and the installed file sizes on Windows, the per-device free memory targets for a 50/50 weight split are approximately 2.63 GiB for Qwen3 1.7B, 3.16 GiB for Qwen3 4B, and 4.17 GiB for Llama 3. Start with Qwen3 1.7B once both devices have sufficient headroom. These are planning thresholds. Confirm actual model and context allocations from the runner logs during an authorized trial.

A later read-only preflight on November 23, 2026 found 3,809 MiB available on Windows.
The paired Mac had 8,192 MiB physical RAM, 493 MiB of free pages and 2,719 MiB of inactive
pages. Speculative pages were reported separately and were not added to free pages. With
free pages alone, the Mac had no weight capacity after the 2 GiB reserve. Counting free
and inactive pages as the placement estimate, the planner projected the following split.

| Model | Windows weights | Mac weights | Outcome |
| --- | ---: | ---: | --- |
| Qwen3 1.7B | 0.8 GiB | 0.5 GiB | Fits the estimated pooled budget |
| Qwen3 4B | 1.4 GiB | 0.9 GiB | Fits the estimated pooled budget |
| Llama 3 | | | About 1.5 GiB short of the estimated pooled budget |

Both model runners remained idle. Recheck available memory and the selected runner devices
immediately before a live load. A model that fits this system-memory estimate can still exceed
the available memory on a selected accelerator.

### Inference measurements

Keep a single model runner's ordered RPC stream on one route. Test separate jobs on different
IP routes and put small status requests on a separate verified path. Compare this against one
route with the same model, prompts, context size and concurrency. Measure both per-job latency
and total job throughput. Losing an active RPC stream requires the runner to recover or restart
the request from saved context. A fresh request can choose an available route independently.
[Configure source-bound worker routes](LLAMA_RPC_TUNNEL.md#credentials-and-endpoints).

Use a fixed prompt and record load time, time to first token, tokens per second, and peak memory per device. Run the same prompt with internet access disabled and the local paths still connected. During a separate request, interrupt the active path and record whether inference resumes, restarts from saved context, or fails. Confirm that the user sees one terminal result and that all worker allocations are released on cancellation. Report transport recovery and inference recovery separately.

Save one inference-host log per trial. With a llama.cpp b12621 log, check the last model load
for nonzero weight buffers on the expected devices and record the runner's final decode rate:

```powershell
node scripts/llama-split-evidence.mjs --log trial.log --expect-buffer CUDA0 --expect-buffer "RPC0[127.0.0.1:50052]"
```

Replace the example buffer names with the names printed by the loader. The JSON includes model
and KV buffer sizes and available timing data. Verify remote worker activity separately and measure
time to first token and peak memory on each computer during the trial.

For llama.cpp, use `node scripts/llama-split-args.mjs --model "Custom model"` with a saved
`model-fit.mjs --json` report piped into standard input. Supply one `--device "Computer=RunnerDevice"`
per allocated computer, using names reported by the authorized runner's `--list-devices` command.
The script prints the layer placement argument array and leaves inference idle. The tensor-split
proportions follow the explicit device order. Check actual layer placement and memory from the logs.
[llama.cpp b12621 split-mode reference](https://github.com/ggml-org/llama.cpp/blob/b12621/docs/multi-gpu.md).

To include the tunnel endpoints in the argument array, add `--runner-inventory devices.txt` and
one `--rpc-tunnel "Computer=127.0.0.1:Port"` per remote computer. Use saved `--list-devices` output
from the inference host with the same tunnels registered in the same order. For b12621 a remote
entry looks like `RPC0: 127.0.0.1:50052 (4096 MiB, 3000 MiB free)`. The mapper matches each remote
device to its endpoint before printing `--rpc`, `--device` and `--tensor-split`. It only reads
the saved report and inventory. Bind each RPC worker to loopback and forward it through an
authenticated tunnel during the authorized trial.
[llama.cpp b12621 RPC setup](https://github.com/ggml-org/llama.cpp/blob/b12621/tools/rpc/README.md).

The following dry run uses an installed Qwen model and example free-memory readings. Replace
the two memory readings with fresh samples before planning a live load. Replace the example
runner device names with the actual names in the desired order.

```powershell
node scripts/model-fit.mjs --json --split --capacity-weighted --host 14 --device "Mac=6" --ollama qwen3:1.7b |
  node scripts/llama-split-args.mjs --model "qwen3:1.7b" --device "This computer=CUDA0" --device "Mac=RPC0"
```

With the default 2 GiB runtime reserve, those sample inputs put 75% of the estimated weights
on the host and 25% on the Mac. If a device has no post-reserve memory, the planner excludes it.
The argument mapper requires a split across at least two participating devices.
