# Upgrade documents: status

Tracks every item from the two upgrade documents the human attached on 2026-11-15
("Consilium Upgrade Overhaul" and "2nd consilium update"). The council works down **Next** in order.
Update this file in the same commit that finishes an item.

Legend: ✅ built · 🔨 in progress · ⏭ next · 🕓 later · ⛔ not feasible as written (reason given)

## Document 1: Master upgrade blueprint

| Item | Status | Notes |
|---|---|---|
| Interactive prompt interception → REQUEST | ✅ | Stalled CLI prompts become a REQUEST |
| Unbuffered streams | ✅ | CLIs run with stream-json/JSON output, read as it arrives |
| Pre-flight auth check | ✅ | Add AI / setup checks each CLI's sign-in before use |
| Devil's advocate in round 3+ | ✅ | Rotating reviewer |
| Context sliding window (summarise long sessions) | ✅ | Over 120k prompt characters, older turns become a bounded local digest prioritising human intent, REQUEST decisions and concrete outcomes; the newest 104k remain verbatim. Current file diffs are separately hashed and bounded |
| File-state hashing / git diff in each turn | ✅ | Bounded, redacted status/diff snapshot with a hard Git subprocess timeout before each editable turn |
| Destructive command sanitizer for Full capacity | ⏭ | Can only gate what Consilium itself runs; the CLIs execute their own commands, so the real guard is their own permission modes plus worktrees |
| Secret redaction | ✅ | `redact.rs`, plus `check-secrets.mjs` before pushes |
| Git worktree per agent in Full capacity | ⏭ | `consilium/agent-<id>` branches, merged on approval |
| Atomic memory appends | ✅ | `memory.rs` writes under a lock, size-capped |
| Pre-commit lint/test before self-modification | ✅ | `.githooks/pre-commit`: secret scan, then `tsc` and `npm test` on an export of the staged index |
| Rollback + oscillation circuit breaker | 🕓 | |
| Global drag-and-drop | ✅ | Drop anywhere in the window |
| Parsers: images, PDF, docx, pptx, code | ✅ | `attach.rs`; AST analysis 🕓 |
| /deconstruct and /reassemble commands | 🕓 | |
| GitHub repo ingest, sandboxed clone, inspection | ✅ | 0.3.11. `repo.rs`: https github.com only, shallow clone with hooks/submodules/LFS/symlinks/file transports off, 2-min kill; read-only report. Repos button + Cmd+K |
| Wrapper generation, local skill vault, public index | 🕓 | After repo ingest |
| visionOS glass theme | ✅ | |
| Hover-to-expand controls | 🕓 | Hover explanations ✅; collapsing toolbars not yet |
| Local vector RAG | 🕓 | |
| MCP client layer | 🕓 | The CLIs already speak MCP themselves |
| Live terminal watcher drawer | 🕓 | Live activity line above replies ✅ |
| Rate-limit benching, cooldown, re-routing, 2-model consensus | ✅ | `bench.rs`, `council.rs` |
| Stall detection (silence + CPU), state lights | ✅ | Working / Stalled lights |
| Freeze controls: interrupt, kill & restart | 🕓 | Stop and Retry exist |
| Night shift, red/blue team, TDD self-healing, electoral routing, ideation, self-profiling | 🕓 | |
| Speculative decoding, digital twins, Lean/Coq | 🕓 | Research items |
| Latent-space / KV-cache sharing between frontier models | ⛔ | The providers' CLIs and APIs only exchange text; no access to model internals |
| OS-level patching beyond the OS's own protections | ⛔ | macOS/Windows sandboxing can't be bypassed, by design |

## Document 2: Second update blueprint

| Item | Status | Notes |
|---|---|---|
| Instant screen-only updates | ✅ | 0.3.5; hardened in 0.3.6 |
| Signal phone bridge, phone Approve/Deny | 🔨 | Adapter in `bridge.rs` (Gemini); not yet connected to the app |
| Voice visualizer, bars + orb, cyan/violet | ✅ | 0.3.6. AI side is an animation: system speech has no audio samples |
| Daily Bing wallpaper with frosted layer, off switch | ✅ | 0.3.7. Cached in ~/.config/consilium/wallpapers, base64 data-URL, off switch in Settings |
| Audit log drawer | ✅ | Applied instantly on 0.3.6 without restarting |
| Persona chips above the prompt | ✅ | Applied instantly on 0.3.6 without restarting |
| Phone "launch" actions | ✅ | 0.3.7. Allowlisted targets (terminal, vscode, dashboard) executed without shell |
| Telegram / Discord | 🕓 | After Signal |
| Quick-summon hotkey, clipboard/screenshot capture, Whisper, Ollama + HUD | 🕓 | |
| Obsidian search, secrets vault, fix-drafting watchdog, widget dock | 🕓 | |
| Consensus tribunal | ✅ | Discuss mode |

## Reliability and portability added during the upgrade

| Item | Status | Notes |
|---|---|---|
| macOS / Windows / Linux feature parity | ✅ | Update Now, orphan cleanup, Signal setup and platform-specific shortcuts/installers use native paths on all three |
| Adaptive support for older or weaker PCs | ✅ | Portable CPU/RAM profile caps local AI processes across the app and terminal; high pressure reduces polling and GPU-heavy visual effects |
| Crash-safe fail-forward behavior | ✅ | OS-held AI slots release after crashes; a broken capacity gate does not lock the user out; Stop remains cancellation-aware while queued |
| Clear Netrunner REQUEST callouts | ✅ | Opaque dark-red panel, brighter border/text and unblurred action buttons |
