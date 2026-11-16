# PeerLink (Windows)

Finds every working network path to a peer computer, tests SMB port 445 on each, and connects over the fastest one.

    PeerLink.cmd add <Peer> -Address <ip> -User <name>
    PeerLink.cmd status <Peer>
    PeerLink.cmd connect <Peer> [-Share <name> -Drive M]

peers.json and *.cred.xml are per-machine, git-ignored, and must never be committed.
