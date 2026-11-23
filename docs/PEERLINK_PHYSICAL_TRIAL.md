# Windows–Mac PeerLink physical failover trial

Use the same Consilium build on both computers. Open a dedicated shared chat and keep model runners idle. Perform the outage steps with local access to both computers so you can restore an interface independently of PeerLink.

1. Connect both computers by Ethernet and join a trusted Wi-Fi network on both. In Settings → Linked computers, verify that both peers report connected and have separate Ethernet and Wi-Fi addresses configured. Record the build on each computer.
2. Run **Communication stress test** on both computers before starting the outage trial. Record the successful authenticated endpoints and the local interfaces that carried traffic. Verify each interface independently. Multiple reachable addresses can share one physical route.
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

The [PeerLink preview run](https://github.com/Ag3nt47/consilium-ai/actions/runs/45259525406#artifacts)
built commit `bdbb8d0` for Windows, macOS and Linux. It includes current-session packet
accounting and recovery from stalled socket writes. Hosted installation checks passed on
all three platforms, and the Windows, Linux and universal macOS artifact archives matched
their published SHA-256 digests. The workflow skipped publication.

These artifacts can serve as matching Windows and macOS builds for the physical trial.
The hosted checks did not exercise live traffic over Ethernet, Wi-Fi or Bluetooth.

## Simultaneous transfer trial

After the individual routes pass, test with Ethernet, Wi-Fi and Bluetooth PAN active. Start an isolated transfer that can select each interface explicitly. Record the bound source interface, destination interface, bytes delivered and timestamps for each path. Confirm the same payload checksum at the receiver.

Compare the three-path run against each single-path baseline with the same payload and computers. To pass, authenticated payload bytes must arrive over each claimed path during overlapping time intervals. TCP handshakes, heartbeats and changes in whole-adapter counters alone do not show that the transfer used all three paths. Repeat while disconnecting each path in turn. Check payload integrity and whether the transfer completed, retried or stopped.

## Split-model inference trial

With permission to start the runners, use the same compatible GGUF and runner build for each comparison. Record usable memory after runtime reserves, planned weight allocation, and actual weight and KV-cache allocation reported by every worker. Verify that each worker participating in the split actually loaded weights and contributed to processing the request.

### Memory preflight

Use fresh paired-computer reports before allocating weights. In the latest Windows–Mac preflight, the Mac returned 951 MiB out of 8192 MiB over PeerLink. The default 2 GiB runtime allowance left it with zero weight capacity. Windows had about 7.33 GiB available in a separate reading.

With that allowance and the installed file sizes, the per-device free memory targets for a 50/50 weight split are approximately 2.64 GiB for Qwen3 1.7B, 3.17 GiB for Qwen3 4B, and 4.17 GiB for Llama 3. Start with Qwen3 1.7B once both devices have sufficient headroom. These are planning thresholds. Confirm actual model and context allocations from the runner logs during an authorized trial.

### Inference measurements

Use a fixed prompt and record load time, time to first token, tokens per second, and peak memory per device. Run the same prompt with internet access disabled and the local paths still connected. During a separate request, interrupt the active path and record whether inference resumes, restarts from saved context, or fails. Confirm that the user sees one terminal result and that all worker allocations are released on cancellation. Report transport recovery and inference recovery separately.

For llama.cpp, use `node scripts/llama-split-args.mjs --model "Custom model"` with a saved
`model-fit.mjs --json` report piped into standard input. Supply one `--device "Computer=RunnerDevice"`
per allocated computer, using names reported by the authorized runner's `--list-devices` command.
The script prints the layer placement argument array and leaves inference idle. The tensor-split
proportions follow the explicit device order. Check actual layer placement and memory from the logs.
[llama.cpp b12621 split-mode reference](https://github.com/ggml-org/llama.cpp/blob/b12621/docs/multi-gpu.md).
