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
