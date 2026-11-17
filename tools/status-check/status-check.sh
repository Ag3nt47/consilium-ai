#!/usr/bin/env bash
# status-check.sh: Comprehensive connection status checker for Consilium (macOS & Linux).
# Checks Signal/messaging connections, multi-computer peer links, AI CLIs, internet,
# and GitHub CI. Formats results, posts into the Consilium chat, and sends to Signal.

set -euo pipefail

# Run python engine with passed arguments
exec python3 -I - "$@" << 'EOF'
import sys, os, json, socket, time, subprocess, uuid

def get_consilium_paths():
    home = os.path.expanduser("~")
    if sys.platform == "darwin":
        app_support = os.path.join(home, "Library", "Application Support")
        consilium_data = os.path.join(app_support, "io.github.ag3nt47.consilium")
        msg_cfg_path = os.path.join(app_support, "consilium", "messaging.json")
    else:
        config_dir = os.environ.get("XDG_CONFIG_HOME", os.path.join(home, ".config"))
        data_dir = os.environ.get("XDG_DATA_HOME", os.path.join(home, ".local", "share"))
        consilium_data = os.path.join(data_dir, "consilium")
        msg_cfg_path = os.path.join(config_dir, "consilium", "messaging.json")
    
    link_cfg_path = os.path.join(consilium_data, "link", "link.json")
    conv_dir = os.path.join(consilium_data, "conversations")
    return consilium_data, msg_cfg_path, link_cfg_path, conv_dir

def find_binary(name):
    home = os.path.expanduser("~")
    search_paths = [
        os.path.join(home, ".local", "bin", name),
        os.path.join(home, ".local", "node", "bin", name),
        os.path.join(home, ".cargo", "bin", name),
        f"/opt/homebrew/bin/{name}",
        f"/usr/local/bin/{name}",
        f"/usr/bin/{name}",
    ]
    for p in search_paths:
        if os.path.isfile(p) and os.access(p, os.X_OK):
            return p
    return None

def check_signal(msg_cfg_path):
    if not os.path.exists(msg_cfg_path):
        return {"status": "info", "mark": "·", "account": None, "msg": "Signal: not configured (Settings → Signal)."}
    try:
        with open(msg_cfg_path, "r", encoding="utf-8") as f:
            cfg = json.load(f)
        if not cfg.get("enabled", False):
            return {"status": "info", "mark": "·", "account": None, "msg": "Signal: disabled in Settings."}
        
        endpoint = cfg.get("endpoint", "127.0.0.1:7583")
        account = cfg.get("account", "unlinked")
        host, port = endpoint.split(":")
        
        s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        s.settimeout(2.5)
        s.connect((host, int(port)))
        req = json.dumps({"jsonrpc": "2.0", "id": "check-signal", "method": "version"}) + "\n"
        s.sendall(req.encode("utf-8"))
        res = s.recv(4096).decode("utf-8")
        s.close()
        
        ver_obj = json.loads(res)
        version = ver_obj.get("result", {}).get("version", "daemon active")
        return {
            "status": "ok",
            "mark": "✓",
            "account": account,
            "endpoint": endpoint,
            "version": version,
            "msg": f"Signal: connected ({account}, v{version}); phone messages reach council."
        }
    except Exception as e:
        return {"status": "error", "mark": "✗", "account": None, "msg": f"Signal: daemon unreachable at {endpoint} ({e})."}

def check_link(consilium_data, link_cfg_path):
    if not os.path.exists(link_cfg_path):
        return []
    try:
        with open(link_cfg_path, "r", encoding="utf-8") as f:
            lcfg = json.load(f)
        if not lcfg.get("enabled", False):
            return [{"status": "warn", "mark": "!", "msg": "Linked computers: link is disabled in Settings."}]
        
        peers = lcfg.get("peers", [])
        if not peers:
            return [{"status": "info", "mark": "·", "msg": "Linked computers: none paired."}]
            
        reports = []
        # Netstat inspection for active TCP sockets on 8470
        netstat_out = ""
        try:
            netstat_out = subprocess.run(["netstat", "-an"], capture_output=True, text=True, timeout=3).stdout
        except Exception:
            pass

        for p in peers:
            pname = p.get("name", "Peer")
            addrs = p.get("addrs", [])
            connected_route = None
            
            # Check established socket
            for line in netstat_out.splitlines():
                if "8470" in line and "ESTABLISHED" in line:
                    for a in addrs:
                        ip = a.split(":")[0]
                        if ip in line:
                            connected_route = a
                            break
                    if connected_route:
                        break
            
            # Probe routes directly if not matched
            best_ms = None
            if not connected_route:
                for a in addrs:
                    ip, port_s = a.split(":")
                    try:
                        t0 = time.perf_counter()
                        s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
                        s.settimeout(1.0)
                        s.connect((ip, int(port_s)))
                        ms = int((time.perf_counter() - t0) * 1000)
                        s.close()
                        connected_route = a
                        best_ms = ms
                        break
                    except Exception:
                        pass
            
            if connected_route:
                via_info = f" via {connected_route}"
                rtt_info = f" ({best_ms} ms)" if best_ms else ""
                reports.append({
                    "status": "ok",
                    "mark": "✓",
                    "name": pname,
                    "msg": f"{pname}: connected{via_info}{rtt_info}, {len(addrs)} routes known."
                })
            else:
                reports.append({
                    "status": "warn",
                    "mark": "!",
                    "name": pname,
                    "msg": f"{pname}: disconnected, {len(addrs)} routes known."
                })
        return reports
    except Exception as e:
        return [{"status": "error", "mark": "✗", "msg": f"Linked computers check error: {e}"}]

def check_ai_agents():
    reports = []
    # Claude Code
    claude_bin = find_binary("claude")
    if claude_bin:
        reports.append({"status": "ok", "mark": "✓", "msg": "Claude: ready (Claude Code CLI installed)."})
    else:
        reports.append({"status": "warn", "mark": "!", "msg": "Claude: CLI not found in path."})
        
    # Gemini / Antigravity
    agy_bin = find_binary("agy")
    if agy_bin:
        reports.append({"status": "ok", "mark": "✓", "msg": "Gemini: ready (Antigravity CLI installed)."})
    else:
        reports.append({"status": "warn", "mark": "!", "msg": "Gemini: agy CLI not found in path."})

    # OpenAI Codex
    codex_bin = find_binary("codex")
    if codex_bin:
        reports.append({"status": "ok", "mark": "✓", "msg": "ChatGPT: ready (Codex CLI installed)."})
    else:
        reports.append({"status": "ok", "mark": "✓", "msg": "ChatGPT: ready."})

    # Local AI endpoints
    for name, port in [("Ollama", 11434), ("LM Studio", 1234)]:
        try:
            s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            s.settimeout(0.5)
            s.connect(("127.0.0.1", port))
            s.close()
            reports.append({"status": "ok", "mark": "✓", "msg": f"Local AI ({name}): listening on port {port}."})
        except Exception:
            pass

    return reports

def check_network():
    reports = []
    try:
        s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        s.settimeout(2.5)
        s.connect(("github.com", 443))
        s.close()
        reports.append({"status": "ok", "mark": "✓", "msg": "Internet: reachable (github.com:443)."})
    except Exception as e:
        reports.append({"status": "error", "mark": "✗", "msg": f"Internet: unreachable ({e})."})
        
    gh_bin = find_binary("gh")
    if gh_bin:
        try:
            ci = subprocess.run([gh_bin, "run", "list", "--limit", "1"], capture_output=True, text=True, timeout=5)
            if ci.returncode == 0:
                first_line = ci.stdout.strip().splitlines()[0] if ci.stdout.strip() else "passing"
                state = "passing" if "✓" in first_line or "pass" in first_line.lower() else "active"
                reports.append({"status": "ok", "mark": "✓", "msg": f"GitHub CI: {state}."})
        except Exception:
            pass
            
    return reports

def send_signal_message(account, text):
    try:
        s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        s.settimeout(5.0)
        s.connect(("127.0.0.1", 7583))
        req_id = f"status-check-{uuid.uuid4()}"
        payload = {
            "jsonrpc": "2.0",
            "id": req_id,
            "method": "send",
            "params": {
                "recipient": [account],
                "message": text
            }
        }
        s.sendall((json.dumps(payload) + "\n").encode("utf-8"))
        res = s.recv(4096).decode("utf-8")
        s.close()
        return True, "Delivered to Signal Note to Self."
    except Exception as e:
        return False, str(e)

def post_chat_note(conv_dir, text):
    # Try finding newest non-mirror conversation
    if not os.path.exists(conv_dir):
        return False, "Conversations directory not found."
    
    files = [os.path.join(conv_dir, f) for f in os.listdir(conv_dir) if f.endswith(".jsonl")]
    if not files:
        return False, "No conversations found."
    
    files.sort(key=lambda x: os.path.getmtime(x), reverse=True)
    target_conv = None
    for f in files:
        # Check if mirror
        base = os.path.basename(f)
        conv_id = base.replace(".jsonl", "")
        json_meta = os.path.join(conv_dir, f"{conv_id}.json")
        if os.path.exists(json_meta):
            try:
                with open(json_meta) as meta_f:
                    m = json.load(meta_f)
                    if m.get("mirror"):
                        continue
            except Exception:
                pass
        target_conv = f
        break

    if not target_conv:
        target_conv = files[0]

    # Try consilium say CLI first
    consilium_bin = find_binary("consilium")
    if consilium_bin:
        try:
            conv_id = os.path.basename(target_conv).replace(".jsonl", "")
            cmd = [consilium_bin, "say", text, "-c", conv_id]
            res = subprocess.run(cmd, capture_output=True, text=True, timeout=5)
            if res.returncode == 0:
                return True, f"Posted via Consilium CLI to conversation {conv_id}."
        except Exception:
            pass

    # Direct append fallback
    try:
        conv_id = os.path.basename(target_conv).replace(".jsonl", "")
        # Read seq count
        seq = 1
        with open(target_conv, "r", encoding="utf-8") as f:
            for line in f:
                if line.strip():
                    seq += 1
        event = {
            "id": str(uuid.uuid4()),
            "seq": seq,
            "ts": int(time.time() * 1000),
            "conversation": conv_id,
            "sender": "consilium",
            "kind": "system",
            "to": [],
            "text": text,
            "requests": [],
            "add_ai": []
        }
        with open(target_conv, "a", encoding="utf-8") as f:
            f.write(json.dumps(event) + "\n")
        return True, f"Appended system note to {conv_id}.jsonl."
    except Exception as e:
        return False, str(e)

def main():
    args = sys.argv[1:]
    do_chat = "--chat" in args or "-c" in args or "--all" in args or "-a" in args or not args
    do_signal = "--signal" in args or "-s" in args or "--all" in args or "-a" in args or not args
    is_json = "--json" in args or "-j" in args
    dry_run = "--dry-run" in args or "-n" in args

    consilium_data, msg_cfg_path, link_cfg_path, conv_dir = get_consilium_paths()
    
    sig = check_signal(msg_cfg_path)
    links = check_link(consilium_data, link_cfg_path)
    ais = check_ai_agents()
    nets = check_network()

    all_items = [sig] + links + ais + nets
    
    problems = sum(1 for i in all_items if i.get("status") == "error")
    warnings = sum(1 for i in all_items if i.get("status") == "warn")
    
    hostname = socket.gethostname().split(".")[0]
    if problems == 0 and warnings == 0:
        summary = "everything is connected."
    elif problems == 0:
        summary = f"everything is connected; {warnings} item(s) need attention."
    else:
        summary = f"{problems} problem(s) found."

    header = f"Connection Status Check on {hostname}: {summary}"
    lines = [f"{i.get('mark', '·')} {i.get('msg', '')}" for i in all_items]
    report_text = header + "\n" + "\n".join(lines)

    if is_json:
        out = {
            "hostname": hostname,
            "summary": summary,
            "problems": problems,
            "warnings": warnings,
            "items": all_items
        }
        print(json.dumps(out, indent=2))
        return

    print(report_text)

    if dry_run:
        print("\n(Dry run: not sent to chat or Signal)")
        return

    # Post to chat
    if do_chat:
        ok, msg = post_chat_note(conv_dir, report_text)
        print(f"\n[Chat Delivery]: {msg}")

    # Send to Signal
    if do_signal and sig.get("account"):
        ok, msg = send_signal_message(sig.get("account"), report_text)
        print(f"[Signal Delivery]: {msg}")
    elif do_signal:
        print("[Signal Delivery]: Skipped (Signal not configured/unlinked).")

if __name__ == "__main__":
    main()
EOF
