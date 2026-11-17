# Session Safety & Change Protection Tool

Protects in-progress code changes, drafts, and untracked files across Consilium app updates and restarts, automatically merging them back into the updated codebase.

---

## Why this exists

When Consilium installs an update and relaunches, or when an AI turn is interrupted by a restart, any active working tree changes must not be lost or overwritten by incoming upstream commits.

This tool:
1. **Snapshots without touching working files**: Uses `git stash create` to generate commit objects of tracked changes without disrupting the active working tree.
2. **Backs up untracked files**: Copies untracked scripts and newly created files to a persistent backup directory.
3. **Performs 3-way merge on restart**: Merges the saved snapshot into the updated repository with standard 3-way merge, automatically preserving changes from both the update and the active session.
4. **Guarantees zero data loss**: If any conflict occurs, conflict markers (`<<<<<<<` / `=======` / `>>>>>>>`) are inserted while original files remain safely pinned under branch `consilium/session-safety`.

---

## Usage

### macOS & Linux (`session-safety.sh`)
```bash
# Snapshot active session changes before an update
tools/session-safety/session-safety.sh save

# Restore and merge session changes after an update
tools/session-safety/session-safety.sh merge

# Check status and last saved safety snapshot
tools/session-safety/session-safety.sh status
```

### Windows (`SessionSafety.ps1` / `session-safety.cmd`)
```powershell
# Snapshot active session changes before an update
.\tools\session-safety\SessionSafety.ps1 save

# Restore and merge session changes after an update
.\tools\session-safety\SessionSafety.ps1 merge

# Check status
tools\session-safety\session-safety.cmd status
```
