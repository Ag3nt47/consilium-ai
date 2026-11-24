# Windows–Mac PeerLink physical failover trial

Use the same Consilium build on both computers. Open a dedicated shared chat and keep model runners idle. Perform the outage steps with local access to both computers so you can restore an interface independently of PeerLink.

1. Connect both computers by Ethernet and join a trusted Wi-Fi network on both. In Settings → Linked computers, verify that both peers report connected and have separate Ethernet and Wi-Fi addresses configured. Record the build on each computer.
2. Run **Test Connection** on both computers to check endpoint authentication and record the local adapters. If the build offers **Test from a local adapter**, select the adapter's source address and run **Test Connection** again. This binds the probe to the source address and leaves the live session in place. Then run **Communication stress test** to verify traffic after switching endpoints. If the build offers **Test target**, select the computer under test to keep other peer sessions out of the exercise. Multiple reachable addresses can share one physical route.
3. With both computers connected, use an established PeerLink session over Ethernet as the baseline. On Windows, map the established socket's local address to its adapter. On macOS, record the local address from the established socket and map it to the interface with the active address.
4. Disconnect Ethernet on one computer. Record the time and send a unique message from that computer while the session is reconnecting. Preserve the send and receive timestamps and count the matching messages on the receiving computer. Verify the send queue drains and confirm that the re-established socket uses Wi-Fi.
5. Restore Ethernet and repeat with Wi-Fi disconnected and Ethernet available. Start this leg with an active Wi-Fi session, and send the queued message from the other computer. Restore the original interface state after the run.

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


## Preview build for the physical trial

The [draft preview run](https://github.com/Ag3nt47/consilium-ai/actions/runs/45291091103#artifacts)
built commit `54c40b4` with connection handoff, concurrent endpoint authentication, **Test AI handshake**,
capacity-weighted model splits, session and weekly council usage, and the opt-in telemetry backdrop.
Hosted installation and desktop launch checks passed on Windows, macOS and Linux. The macOS job
verified the Apple Silicon and Intel executable slices. The Linux job exercised DEB and AppImage
launches on Ubuntu and an RPM installation and launch on Fedora. The workflow skipped publication.
All three downloaded archives matched GitHub's SHA-256 digests.

| Platform | Draft archive | Verified archive SHA-256 |
| --- | --- | --- |
| Windows | [Windows EXE](https://github.com/Ag3nt47/consilium-ai/actions/runs/45291091103/artifacts/14873668553) | `d63eb600ff8b67191b97bd234be242807e4e0fbffb18d8fc99d6bcb9049f46d8` |
| macOS | [Universal DMG](https://github.com/Ag3nt47/consilium-ai/actions/runs/45291091103/artifacts/14874702191) | `33db5b3e879046b3ea9fe6827871badc18445d5eb30b06b534eacab704eeef11` |
| Linux | [DEB, RPM and AppImage](https://github.com/Ag3nt47/consilium-ai/actions/runs/45291091103/artifacts/14874976112) | `2bbd7aa30c06de8be829e4ab3b6d10a49617cc7eefde66d5ddda6c9cfa398e0a` |

Use the matching Windows and macOS archives for the connection handoff trial. Use the socket
commands above to confirm which adapters carry the active session during the outage. Live
Ethernet, Wi-Fi and Bluetooth behavior still needs the physical trial.

In **Multi-Computer Sync**, click **Test AI handshake** on each connected peer. Record whether a
fresh council report arrives and how many AIs have confirmations in both directions.
The check uses existing AI confirmations and sends no new prompts. In the same menu, open
**Check local model memory** to scan for fresh peer capacity reports.

## Simultaneous transfer trial

After the individual routes pass, test with Ethernet, Wi-Fi and Bluetooth PAN active. Start an isolated transfer that can select each interface explicitly. Record the bound source interface, destination interface, bytes delivered and timestamps for each path. Confirm the same payload checksum at the receiver.

Compare the three-path run against each single-path baseline with the same payload and computers. To pass, authenticated payload bytes must arrive over each claimed path during overlapping time intervals. TCP handshakes, heartbeats and changes in whole-adapter counters alone do not show that the transfer used all three paths. Repeat while disconnecting each path in turn. Check payload integrity and whether the transfer completed, retried or stopped.

## Split-model inference trial

With permission to start the runners, use the same compatible GGUF and runner build for each comparison. Record usable memory after runtime reserves, planned weight allocation, and actual weight and KV-cache allocation reported by every worker. Verify that each worker participating in the split actually loaded weights and contributed to processing the request.

### Memory preflight

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
