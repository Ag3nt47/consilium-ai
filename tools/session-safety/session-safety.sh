#!/usr/bin/env bash
# session-safety.sh: Session change protection and automatic merge script for Consilium (macOS & Linux).
# Protects in-progress session edits and untracked files across app restarts and updates,
# automatically merging them back into the updated codebase.

set -euo pipefail

exec python3 -I - "$@" << 'EOF'
import sys, os, json, time, subprocess, shutil

def get_safety_dir():
    home = os.path.expanduser("~")
    if sys.platform == "darwin":
        base = os.path.join(home, "Library", "Application Support", "io.github.ag3nt47.consilium")
    else:
        base = os.path.join(os.environ.get("XDG_DATA_HOME", os.path.join(home, ".local", "share")), "consilium")
    safety = os.path.join(base, "safety")
    os.makedirs(safety, exist_ok=True)
    return safety

def get_repo_dir(arg_path=None):
    if arg_path and os.path.isdir(arg_path):
        return os.path.abspath(arg_path)
    # Check current directory
    cwd = os.getcwd()
    res = subprocess.run(["git", "rev-parse", "--show-toplevel"], cwd=cwd, capture_output=True, text=True)
    if res.returncode == 0 and res.stdout.strip():
        return res.stdout.strip()
    # Check default project folder
    default_dir = os.path.expanduser("~/Projects/consilium-ai")
    if os.path.isdir(default_dir):
        return default_dir
    return cwd

def cmd_save(repo_dir):
    print(f"Checking repository at {repo_dir}...")
    st = subprocess.run(["git", "status", "--porcelain"], cwd=repo_dir, capture_output=True, text=True)
    lines = [l for l in st.stdout.splitlines() if l.strip()]
    
    if not lines:
        print("Working tree clean; no uncommitted session changes to save.")
        return 0

    ts = int(time.time())
    safety_dir = get_safety_dir()
    session_dir = os.path.join(safety_dir, f"session-{ts}")
    untracked_dir = os.path.join(session_dir, "untracked")
    os.makedirs(untracked_dir, exist_ok=True)

    untracked_files = []
    tracked_changes = []

    for l in lines:
        code = l[:2]
        path = l[3:].strip()
        if code == "??":
            untracked_files.append(path)
            src = os.path.join(repo_dir, path)
            dst = os.path.join(untracked_dir, path)
            os.makedirs(os.path.dirname(dst), exist_ok=True)
            if os.path.isfile(src):
                shutil.copy2(src, dst)
            elif os.path.isdir(src):
                shutil.copytree(src, dst, dirs_exist_ok=True)
        else:
            tracked_changes.append(path)

    stash_hash = ""
    # Capture tracked changes commit object without modifying working tree
    if tracked_changes:
        msg = f"consilium-session-safety-{ts}"
        h = subprocess.run(["git", "stash", "create", msg], cwd=repo_dir, capture_output=True, text=True).stdout.strip()
        if h:
            stash_hash = h
            # Point branch ref to prevent garbage collection
            subprocess.run(["git", "branch", "-f", "consilium/session-safety", h], cwd=repo_dir, capture_output=True)

    meta = {
        "timestamp": ts,
        "repo_dir": repo_dir,
        "stash_hash": stash_hash,
        "tracked_changes": tracked_changes,
        "untracked_files": untracked_files,
        "session_dir": session_dir
    }

    latest_file = os.path.join(safety_dir, "latest.json")
    with open(latest_file, "w", encoding="utf-8") as f:
        json.dump(meta, f, indent=2)

    print(f"✓ Session safety snapshot saved (timestamp {ts}):")
    if tracked_changes:
        print(f"  - {len(tracked_changes)} tracked file(s) snapshot commit: {stash_hash[:10]}")
    if untracked_files:
        print(f"  - {len(untracked_files)} untracked file(s) backed up to {untracked_dir}")
    print("Your work is safely protected against updates and restarts.")
    return 0

def cmd_merge(repo_dir):
    safety_dir = get_safety_dir()
    latest_file = os.path.join(safety_dir, "latest.json")
    if not os.path.exists(latest_file):
        print("No saved session snapshot found to merge.")
        return 0

    with open(latest_file, "r", encoding="utf-8") as f:
        meta = json.load(f)

    target_repo = meta.get("repo_dir", repo_dir)
    stash_hash = meta.get("stash_hash")
    untracked_files = meta.get("untracked_files", [])
    session_dir = meta.get("session_dir", "")
    untracked_dir = os.path.join(session_dir, "untracked")

    print(f"Restoring & merging session snapshot from {time.ctime(meta.get('timestamp', 0))} into {target_repo}...")

    # 1. Restore untracked files
    if os.path.exists(untracked_dir) and untracked_files:
        for f in untracked_files:
            src = os.path.join(untracked_dir, f)
            dst = os.path.join(target_repo, f)
            if os.path.exists(src):
                os.makedirs(os.path.dirname(dst), exist_ok=True)
                if os.path.isfile(src):
                    shutil.copy2(src, dst)
                elif os.path.isdir(src):
                    shutil.copytree(src, dst, dirs_exist_ok=True)
        print(f"✓ Restored {len(untracked_files)} untracked file(s).")

    # 2. 3-way merge of tracked changes
    if stash_hash:
        print(f"Merging tracked changes ({stash_hash[:10]}) with 3-way merge...")
        res = subprocess.run(["git", "stash", "apply", stash_hash], cwd=target_repo, capture_output=True, text=True)
        if res.returncode == 0:
            print("✓ All session changes successfully merged into working tree with 0 conflicts!")
        else:
            print("! Automatic merge encountered conflicts in some files:")
            print(res.stdout or res.stderr)
            print("Conflict markers (<<<<<<< / ======= / >>>>>>>) have been placed in the affected files.")
            print("Your original backup is safely preserved in consilium/session-safety.")
    else:
        print("✓ All untracked session files restored; no tracked changes were pending.")

    return 0

def cmd_status(repo_dir):
    safety_dir = get_safety_dir()
    latest_file = os.path.join(safety_dir, "latest.json")
    print(f"Repository: {repo_dir}")
    st = subprocess.run(["git", "status", "-s"], cwd=repo_dir, capture_output=True, text=True)
    if st.stdout.strip():
        print("Current uncommitted changes:")
        print(st.stdout.strip())
    else:
        print("Current working tree: clean.")

    if os.path.exists(latest_file):
        with open(latest_file) as f:
            m = json.load(f)
        print(f"\nLast saved safety snapshot: {time.ctime(m.get('timestamp', 0))}")
        print(f"  Tracked changes: {len(m.get('tracked_changes', []))} file(s)")
        print(f"  Untracked files: {len(m.get('untracked_files', []))} file(s)")
        print(f"  Stash commit: {m.get('stash_hash', 'none')}")
    else:
        print("\nNo saved safety snapshots.")

def main():
    args = sys.argv[1:]
    action = args[0] if args else "status"
    target = args[1] if len(args) > 1 else None

    repo_dir = get_repo_dir(target)

    if action in ("save", "snapshot", "backup"):
        sys.exit(cmd_save(repo_dir))
    elif action in ("merge", "restore", "apply"):
        sys.exit(cmd_merge(repo_dir))
    elif action in ("status", "check"):
        cmd_status(repo_dir)
    else:
        print("Usage: session-safety.sh [save|merge|status] [optional-repo-path]")
        sys.exit(1)

if __name__ == "__main__":
    main()
EOF
