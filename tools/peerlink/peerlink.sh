#!/bin/zsh
# PeerLink (macOS): find every working network path to a peer and connect over the best one.
# Counterpart to Windows PeerLink.ps1, sharing the same peers.json format.
exec /usr/bin/python3 -I - "$@" << 'EOF'
import sys, os, json, socket, subprocess, time, re

SCRIPT_DIR = os.path.dirname(os.path.abspath(sys.argv[0])) if sys.argv[0] != '-' else os.path.expanduser('~/PeerLink')
CFG_PATH = os.path.join(SCRIPT_DIR, 'peers.json')

def load_cfg():
    if os.path.exists(CFG_PATH):
        try:
            with open(CFG_PATH, 'r', encoding='utf-8') as f:
                return json.load(f)
        except Exception:
            return {}
    return {}

def save_cfg(cfg):
    os.makedirs(os.path.dirname(CFG_PATH), exist_ok=True)
    with open(CFG_PATH, 'w', encoding='utf-8') as f:
        json.dump(cfg, f, indent=2)

def norm_mac(m):
    return re.sub(r'[:-]', '-', m.strip().upper())

def merge_unique(old_list, new_list, max_len=6):
    res = []
    seen = set()
    for item in (new_list or []) + (old_list or []):
        if item and item not in seen:
            seen.add(item)
            res.append(item)
            if len(res) >= max_len:
                break
    return res

def test_tcp(ip, port=445, timeout=1.5):
    fam = socket.AF_INET6 if ':' in ip else socket.AF_INET
    s = socket.socket(fam, socket.SOCK_STREAM)
    s.settimeout(timeout)
    t0 = time.perf_counter()
    clean_ip = ip.split('%')[0]
    try:
        if fam == socket.AF_INET6:
            # Check for scope id
            scope_id = 0
            if '%' in ip:
                iface = ip.split('%')[1]
                try:
                    scope_id = socket.if_nametoindex(iface)
                except Exception:
                    scope_id = 0
            s.connect((clean_ip, port, 0, scope_id))
        else:
            s.connect((clean_ip, port))
        ms = int((time.perf_counter() - t0) * 1000)
        s.close()
        return max(ms, 1)
    except Exception:
        return -1

def update_neighbors(p):
    for ip in p.get('IPs', []):
        if ip and ':' not in ip:
            subprocess.run(['ping', '-c', '1', '-W', '300', ip], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    # Ping IPv6 all-nodes on en0 and en4
    for iface in ['en0', 'en4']:
        subprocess.run(['ping6', '-c', '1', '-X', '1', f'ff02::1%{iface}'], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

def get_candidates(peer_name, p):
    candidates = []
    seen = set()

    def add(via, ip):
        if ip and ip not in seen:
            seen.add(ip)
            candidates.append({'Via': via, 'Address': ip})

    # 1. Names
    for h in p.get('Hosts', [f'{peer_name}.local', peer_name]):
        if h:
            try:
                for res in socket.getaddrinfo(h, 445, socket.AF_UNSPEC, socket.SOCK_STREAM):
                    addr = res[4][0]
                    add(f'name {h}', addr)
            except Exception:
                pass

    # 2. MAC matching via arp -an
    macs = [norm_mac(m) for m in p.get('Macs', []) if m]
    if macs:
        try:
            arp = subprocess.run(['arp', '-an'], capture_output=True, text=True).stdout
            for line in arp.splitlines():
                # Format: ? (192.168.1.157) at 4c:cc:6a:81:6b:3c on en4 ifscope [ethernet]
                m = re.search(r'\(([\d\.]+)\)\s+at\s+([0-9a-fA-F:]+)\s+on\s+([a-zA-Z0-9]+)', line)
                if m:
                    ip_found, mac_found, iface = m.group(1), norm_mac(m.group(2)), m.group(3)
                    if mac_found in macs:
                        add(f'hardware address ({iface})', ip_found)
        except Exception:
            pass

        # Also check ndp -an for IPv6 link-local
        try:
            ndp = subprocess.run(['ndp', '-an'], capture_output=True, text=True).stdout
            for line in ndp.splitlines():
                parts = line.split()
                if len(parts) >= 3 and parts[0].startswith('fe80:'):
                    ipv6_addr = parts[0]
                    mac_found = norm_mac(parts[1])
                    iface = parts[2]
                    if mac_found in macs:
                        add(f'router-free link-local ({iface})', ipv6_addr)
        except Exception:
            pass

    # 3. Last known IPs
    for ip in p.get('IPs', []):
        add('last known address', ip)

    # 4. Direct link
    if p.get('Direct'):
        add('direct static link', p['Direct'])

    # 5. Tailscale
    if p.get('Tailscale'):
        try:
            ts = subprocess.run(['tailscale', 'ip', '-4', p['Tailscale']], capture_output=True, text=True)
            if ts.returncode == 0 and ts.stdout.strip():
                add('tailscale', ts.stdout.strip().splitlines()[0])
        except Exception:
            pass

    return candidates

def parse_args():
    args = sys.argv[1:]
    if not args:
        return {'action': 'list'}
    action = args[0].lower()
    if action in ['-h', '--help', 'help']:
        print("Usage: peerlink.sh [list | status <peer> | add <peer> [options] | connect <peer> [-Share share]]")
        sys.exit(0)
    
    if action not in ['list', 'add', 'status', 'connect']:
        # Could be peer directly e.g. peerlink.sh MSI-AGENT47
        peer = action
        action = 'status'
        idx = 1
    else:
        peer = args[1] if len(args) > 1 else None
        idx = 2

    options = {'action': action, 'peer': peer, 'address': [], 'mac': [], 'user': '', 'share': '', 'tailscale': '', 'direct': '', 'port': 445}

    while idx < len(args):
        k = args[idx].lower().lstrip('-')
        v = args[idx+1] if idx + 1 < len(args) else ''
        if k in ['address', 'a', 'ips']:
            options['address'] = [x.strip() for x in v.split(',') if x.strip()]
            idx += 2
        elif k in ['mac', 'm', 'macs']:
            options['mac'] = [norm_mac(x) for x in v.split(',') if x.strip()]
            idx += 2
        elif k in ['user', 'u']:
            options['user'] = v
            idx += 2
        elif k in ['share', 's']:
            options['share'] = v
            idx += 2
        elif k in ['tailscale', 't']:
            options['tailscale'] = v
            idx += 2
        elif k in ['direct', 'd']:
            options['direct'] = v
            idx += 2
        elif k in ['port', 'p']:
            options['port'] = int(v) if v.isdigit() else 445
            idx += 2
        else:
            idx += 1

    return options

def main():
    opts = parse_args()
    action = opts['action']
    cfg = load_cfg()

    if action == 'list':
        if not cfg:
            print("No peers configured yet. Add one: peerlink.sh add <PeerName> -Address <ip> -User <user>")
            return
        header = f"{'Peer':<16} {'User':<12} {'IPs':<32} {'Macs':<36} {'Share':<12}"
        print(header)
        print("-" * len(header))
        for p_name, p in cfg.items():
            ips_str = ", ".join(p.get('IPs', []))
            macs_str = ", ".join(p.get('Macs', []))
            print(f"{p_name:<16} {p.get('User',''):<12} {ips_str:<32} {macs_str:<36} {p.get('Share',''):<12}")
        return

    peer = opts['peer']
    if not peer:
        print("Error: Specify a peer name, e.g.: peerlink.sh status MSI-AGENT47", file=sys.stderr)
        sys.exit(1)

    if action == 'add':
        p = cfg.get(peer, {
            'Hosts': [f'{peer}.local', peer],
            'IPs': [],
            'Macs': [],
            'User': '',
            'Share': '',
            'Tailscale': '',
            'Direct': ''
        })
        p['IPs'] = merge_unique(p.get('IPs', []), opts['address'])
        p['Macs'] = merge_unique(p.get('Macs', []), opts['mac'])
        if opts['user']: p['User'] = opts['user']
        if opts['share']: p['Share'] = opts['share']
        if opts['tailscale']: p['Tailscale'] = opts['tailscale']
        if opts['direct']: p['Direct'] = opts['direct']
        cfg[peer] = p
        save_cfg(cfg)
        print(f"Saved peer {peer}")
        action = 'status'

    if peer not in cfg:
        print(f"Unknown peer {peer}. Add it first: peerlink.sh add {peer} -Address <ip> -User <name>", file=sys.stderr)
        sys.exit(1)

    p = cfg[peer]
    update_neighbors(p)
    candidates = get_candidates(peer, p)

    results = []
    for c in candidates:
        ms = test_tcp(c['Address'], opts.get('port', 445))
        results.append({
            'Via': c['Via'],
            'Address': c['Address'],
            'Open': ms >= 0,
            'Ms': ms if ms >= 0 else None
        })

    # Sort open first, then lowest ms
    results.sort(key=lambda r: (not r['Open'], r['Ms'] if r['Ms'] is not None else 999999))

    # Update learned IPs and MACs
    v4 = [r['Address'] for r in results if r['Open'] and ':' not in r['Address']]
    p['IPs'] = merge_unique(p.get('IPs', []), v4)
    # Learn MACs from arp
    try:
        arp_out = subprocess.run(['arp', '-an'], capture_output=True, text=True).stdout
        learned_macs = []
        for ip in v4:
            m = re.search(rf'\({re.escape(ip)}\)\s+at\s+([0-9a-fA-F:]+)', arp_out)
            if m:
                learned_macs.append(norm_mac(m.group(1)))
        p['Macs'] = merge_unique(p.get('Macs', []), learned_macs)
    except Exception:
        pass
    cfg[peer] = p
    save_cfg(cfg)

    print(f"Paths to {peer} (port {opts.get('port', 445)}):")
    fmt = "{:<32} {:<24} {:<8} {:<6}"
    print(fmt.format("Via", "Address", "Open", "Ms"))
    print(fmt.format("-" * 30, "-" * 22, "-" * 6, "-" * 4))
    for r in results:
        ms_str = str(r['Ms']) if r['Ms'] is not None else ""
        print(fmt.format(r['Via'], r['Address'], str(r['Open']), ms_str))

    if action == 'connect':
        best = next((r for r in results if r['Open']), None)
        if not best:
            print(f"No working path to {peer}.", file=sys.stderr)
            sys.exit(1)
        
        user = p.get('User') or opts['user'] or os.environ.get('USER', '')
        share = opts['share'] or p.get('Share') or 'MacShare'
        host = best['Address']

        # Format URL for macOS Finder SMB
        smb_url = f"smb://{user}@{host}/{share}"
        print(f"\nConnecting to {peer} via {best['Via']} [{host}] -> {smb_url}")
        
        # Connect via open
        res = subprocess.run(['open', smb_url])
        if res.returncode == 0:
            print(f"Connected to {peer}. Finder mount initiated for {smb_url}")
        else:
            print(f"Failed to open {smb_url}", file=sys.stderr)
            sys.exit(1)

if __name__ == '__main__':
    main()
EOF
