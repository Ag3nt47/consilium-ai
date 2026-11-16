# PeerLink (Windows)

Finds every working network path to a peer computer (name/mDNS, MAC-address match, IPv6 link-local, last known IPs, optional static link, optional Tailscale), tests SMB port 445 on each, and connects over the fastest one.

    PeerLink.cmd add <Peer> -Address <ip> -User <name>   # register a peer
    PeerLink.cmd status <Peer>                           # show all paths
    PeerLink.cmd connect <Peer> [-Share <name> -Drive M] # sign in over the best path

Per-machine data (`peers.json`, `*.cred.xml`) is created next to the script, is git-ignored, and must never be committed. Credentials are DPAPI-encrypted for the current Windows user.
