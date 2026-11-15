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
| Context sliding window (summarise long sessions) | ⏭ | Summarise old turns once a token threshold is crossed |
| File-state hashing / git diff in each turn | 🔨 | Implementation is in progress; bounded, redacted snapshots still need a hard subprocess timeout |
| Destructive command sanitizer for Full capacity | ⏭ | Can only gate what Consilium itself runs; the CLIs execute their own commands, so the real guard is their own permission modes plus worktrees |
| Secret redaction | ✅ | `redact.rs`, plus `check-secrets.mjs` before pushes |
| Git worktree per agent in Full capacity | ⏭ | `consilium/agent-<id>` branches, merged on approval |
| Atomic memory appends | ✅ | `memory.rs` writes under a lock, size-capped |
| Pre-commit lint/test before self-modification | 🕓 | |
| Rollback + oscillation circuit breaker | 🕓 | |
| Global drag-and-drop | ✅ | Drop anywhere in the window |
| Parsers: images, PDF, docx, pptx, code | ✅ | `attach.rs`; AST analysis 🕓 |
| /deconstruct and /reassemble commands | 🕓 | |
| GitHub repo ingest, sandboxed clone, inspection | ⏭ | |
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
| Daily Bing wallpaper with frosted layer, off switch | ⏭ | Gemini, after Signal |
| Audit log drawer | 🔨 | UI committed; awaiting a compatible Apply Now or full app update |
| Persona chips above the prompt | 🔨 | UI committed; awaiting a compatible Apply Now or full app update |
| Phone "launch" actions | ⏭ | Fixed app list in local config, no shell |
| Telegram / Discord | 🕓 | After Signal |
| Quick-summon hotkey, clipboard/screenshot capture, Whisper, Ollama + HUD | 🕓 | |
| Obsidian search, secrets vault, fix-drafting watchdog, widget dock | 🕓 | |
| Consensus tribunal | ✅ | Discuss mode |
