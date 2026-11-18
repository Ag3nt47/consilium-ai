# Multi-Computer AI Coordination Protocol

This document establishes the canonical operating rules for AI assistants (Gemini, Claude, ChatGPT) collaborating across multiple linked computers (e.g., `Lukes-MBP` macOS host and `MSI-AGENT47` Windows host) running Consilium.

---

## 1. Shared Task Board & Ownership

1. **One Authoritative Board Per Conversation**:
   - The computer that originates the conversation hosts the authoritative task board.
   - Linked computers replicate the board as mirrors.
2. **Canonical Identifiers**:
   - Tasks are numbered sequentially and consistently across all linked nodes.
   - Task owners are recorded as canonical handle slugs (e.g., `claude-msi-agent47`, `gemini-lukes-mbp`).
   - An AI on a mirror refers to local agents by their local name and remote agents by `<agent>-<machine>`.
3. **Claiming & Handoff**:
   - A task must be claimed with `TASK: take <#n>` before code is modified.
   - An AI must never claim or begin work on a task already marked `doing` by another participant unless explicitly handed off with `TASK: hand <#n> to @name`.
   - Completed work must be verified and committed before being marked `TASK: done <#n>`.
   - Finished task events broadcast automatically to all mirrors via `peer_sync`.

---

## 2. Git Worktree Isolation & Branching

1. **Never Edit Directly on Shared `main` During Concurrent Work**:
   - AIs working on separate tasks concurrently must use isolated git worktrees rather than the primary working copy.
   - Create a worktree:
     ```bash
     git worktree add ../consilium-<feature> -b ai/<handle>/<task-id>
     ```
   - This prevents uncommitted file collisions, build lock contention, and dirty-tree rebase conflicts.
2. **Repository Configuration Integrity**:
   - Never alter repository-level git configuration flags (e.g., `git config core.bare true`).
   - Clean up completed worktrees with `git worktree remove` once their branch is merged or pushed.

---

## 3. Host Roles & Resource Constraints

1. **Machine Specialization**:
   - **Mac (`Lukes-MBP`)**: Primary builder and releaser. Multi-platform CI triggers, Vite UI builds, documentation, and releases are coordinated from the Mac.
   - **Windows (`MSI-AGENT47`)**: Platform-specific verifier. Responsible for Windows registry settings, PowerShell scripts, Defender integration, and Windows installer runtime behavior.
2. **Drive & Build Directory Isolation**:
   - On Windows, C: drive space is constrained. All Cargo builds on Windows must configure `$env:CARGO_TARGET_DIR = "D:\Agent47\cargo-target\consilium"` to protect system storage.
   - Windows update installs must download published, hash-verified release installers from GitHub rather than compiling release binaries locally on C:.

---

## 4. Command Box Hygiene & Auto-Run Protocol

1. **Executable Code Blocks**:
   - Shell blocks (````bash````, ````zsh````, ````powershell````) in chats with Auto-run enabled are executed automatically.
   - Only place complete, self-contained, fully-parameterized commands inside executable blocks.
2. **Templates and Pseudo-Code**:
   - Any script template containing placeholder parameters (e.g., `param(...)`, `<path>`, `TODO`), diagnostic snippets, or informational text MUST be enclosed in a ````text```` block.
3. **Transcript Verification Before Polling**:
   - AIs must inspect recent transcript entries before posting "waiting on command output" to verify whether the command result has already been delivered.
4. **Elevated & Destructive Commands**:
   - Destructive operations (deleting directories, removing software, administrative elevation) must always follow the formal `REQUEST:` structure and await explicit user approval.

---

## 5. File Sharing & Log Transfer

1. **Source Code**:
   - Code transfers exclusively through git branches and commits pushed to `origin`.
2. **Binary Artifacts & Large Logs**:
   - Diagnostic captures, heap dumps, and test traces are transferred across nodes via Consilium's authenticated `peer_sync` frame streaming with SHA-256 digest validation.
   - Local network file shares (e.g., SMB `M:`) serve as an optional manual fallback and must not be assumed as the default transport.

---

## 6. GitHub CI & Release Coordination

1. **Single CI Watcher**:
   - Only the AI participant who commits and pushes a change to `origin/main` tracks and announces the CI run status. Other participants must refrain from posting duplicate CI checks.
2. **Multi-Node Zero-Touch Update Flow**:
   - When new features or fixes are ready for deployment, the version number is bumped across `package.json`, `Cargo.toml`, and `tauri.conf.json`.
   - Once pushed to `origin/main` and all platform workflows pass, GitHub Actions publishes the release draft.
   - Clicking **Request App Update** broadcasts an `update_request` packet to all connected peers.
   - Each node waits for active tasks to conclude (`RUNS_ACTIVE` guard), verifies the platform installer SHA-256 digest, silently applies the update, and relaunches.
