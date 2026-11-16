# PeerLink: Multi-Path Peer Connection & Failover Tool

PeerLink discovers all network routes between linked machines (macOS & Windows) and connects over the lowest-latency active path with automatic failover.

## Transports Discovered & Ranked
1. **Name resolution**: `<peer>.local` (mDNS) and hostname.
2. **Hardware addresses (ARP/NDP)**: Discovers MACs in the neighbor table so connections survive DHCP address changes.
3. **Router-free direct link**: Link-local IPv6 (`fe80::...`) and IPv4 (`169.254.x.x`).
4. **Last known IP addresses**: Stored in `peers.json`.
5. **Tailscale**: Automated lookup via `tailscale ip -4 <peer>` when installed.

Port 445 (SMB) and Port 8470 (Consilium Sync) are probed concurrently to rank routes by latency (ms).

## Usage

### macOS (`peerlink.sh`)
```bash
# List registered peers
tools/peerlink/peerlink.sh list

# Add or update peer
tools/peerlink/peerlink.sh add MSI-AGENT47 -Address 192.168.1.157,192.168.1.170 -Mac 4C-CC-6A-81-6B-3C,84-EF-18-F3-48-A5 -User Agent47 -Share MacShare

# Check route status and latency
tools/peerlink/peerlink.sh status MSI-AGENT47

# Connect in Finder via fastest route
tools/peerlink/peerlink.sh connect MSI-AGENT47
```

### Windows (`PeerLink.cmd` / `PeerLink.ps1`)
```powershell
# List registered peers
tools\peerlink\PeerLink.cmd list

# Register Mac peer
tools\peerlink\PeerLink.cmd add Lukes-MBP -Address 192.168.1.169,192.168.1.168 -Mac 00-E0-4C-5E-38-28,8C-85-90-7C-D3-7F -User lukelucas -Share lukelucas

# Connect Mac home folder to drive M:
tools\peerlink\PeerLink.cmd connect Lukes-MBP -Drive M
```
