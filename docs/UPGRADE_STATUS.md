# Upgrade documents: status

This is the canonical backlog for every requirement in the three Word documents the human attached
on 2026-11-15: **Consilium Upgrade Overhaul.docx**, **2nd consilium update.docx** and **3rd update.docx**. These documents
are requested product scope, not optional inspiration. The council must prefer unfinished items
marked **in progress** or **next** when **Request App Update** starts new work, then later document
items, the roadmap, open bugs, and newly noticed improvements. Update this file in the same commit
that finishes an item, and mark an item complete only after implementation and verification.

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
| Destructive command sanitizer for Full capacity | 🔨 | Provider-native permission modes remain primary. An emergency brake now stops observable broad data-loss/disk/OS commands at the structured pre-execution stream boundary; provider ordering can change, so OS-enforced worktree isolation remains next |
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

### Master blueprint advanced phase coverage

The master document continues through Phase 27. The phase-level rows below keep that later scope
visible even when an earlier row above already tracks a shipped foundation. A phase remains open
until every feasible capability in it is implemented and verified; completing one sub-feature does
not complete the phase.

| Source phase | Status | Remaining scope retained from the document |
|---|---|---|
| Phase 10: Fault-tolerant rate limits | ✅ | Error sniffing, benching, cooldowns, rerouting and graceful participant reduction are built |
| Phase 11: Enterprise security and integrations | 🕓 | Continuous multi-agent review, secure enclaves, P2P/CRDT memory, policy compliance, cross-app RPC, generated API clients, stateful multi-file routing, formal guardrails and feedback-based routing |
| Phase 12: User and AI quality of life | 🕓 | Side-by-side diff approval, live autonomy controls, incremental AST caching, deterministic replay and zero-shot wrapper synthesis; Stop, recovery states and Cmd/Ctrl+K are partial foundations |
| Phase 13: Frontier-model intelligence | 🕓 | Multimodal UI-to-code, digital-twin sandboxes and theorem-prover integration; provider-internal speculative decoding and latent/KV sharing remain unavailable through public CLIs/APIs |
| Phase 14: Enterprise architectures | 🕓 | Air-gapped compliance evidence, local GPU cache pooling where supported, accessibility-driven cross-app automation, defensive threat simulation and fleet consensus |
| Phase 15: Ultra-cutting-edge capabilities | 🕓 | Ephemeral micro-VM isolation, local model adaptation/pruning, post-quantum agent-mesh security and explicitly opt-in ambient context |
| Phase 16: Cost and explainability | 🕓 | User-set token/spend limits, complexity-aware local/cloud routing and inspectable decision/debate traces; usage reporting and live activity are foundations |
| Phase 17: P2P local compute mesh | 🕓 | Private-LAN discovery, distributed offline inference, task sharding and disconnect-aware load balancing |
| Phase 18: Evolutionary code optimization | 🕓 | Sandboxed candidate mutation/crossover, benchmark fitness selection and approval-gated promotion of winners |
| Phase 19: Universal deconstruction | 🕓 | Deconstruct/reassemble commands, command-intent analysis and dependency/architecture mapping; attachment parsing and repo inspection are foundations |
| Phase 20: Autonomous AI onboarding | 🕓 | Capability handshake, dynamic tool/skill provisioning and bounded memory/project synchronization for newly added models |
| Phase 21: Synthetic data and evaluation | 🕓 | Edge-case data generation, continuous performance/security benchmarks and privacy-safe human/AI feedback loops |
| Phase 22: Cross-device telemetry and routing | 🕓 | Thermal, battery and power-aware telemetry across user-owned devices plus consented workload routing; single-machine adaptive limits are built |
| Phase 23: Spatial computing | 🕓 | Optional AR/VR architecture graphs, git topology and debate visualization |
| Phase 24: Agent generation and Agent Shed | 🕓 | Specialized agent synthesis, persistent local/hybrid registry, approval-gated update/deprecation/purge and dynamic council onboarding |
| Phase 25: Browser and cross-app automation | 🕓 | Permission-gated browser agents, API/DOM inspection and end-to-end multimodal web testing |
| Phase 26: Consilium self-improvement | 🕓 | Low-overhead profiling, bounded code/prompt experiments, isolated regression/security harnesses and rollback-safe closed-loop deployment; pre-commit and update foundations are built |
| Phase 27: Deployment roadmap | 🕓 | Finish Stages 2–3 safety/skill work, then evaluate the Stage 4 multi-node mesh only with explicit security and hardware gates |

## Document 2: Second update blueprint

| Item | Status | Notes |
|---|---|---|
| Instant screen-only updates | ✅ | 0.3.5; hardened in 0.3.6 |
| Signal phone bridge, phone Approve/Deny | 🔨 | Adapter in `bridge.rs` (Gemini); not yet connected to the app |
| Voice visualizer, bars + orb, cyan/violet | ✅ | 0.3.6. AI side is an animation: system speech has no audio samples |
| Distinct AI voices and reliable speaker handoff | ✅ | Per-AI system voice picker and preview in Settings; a WebKit completion watchdog clears a stuck speaker and returns control to the next AI or human microphone |
| Daily Bing wallpaper with frosted layer, off switch | ✅ | 0.3.7. Cached in ~/.config/consilium/wallpapers, base64 data-URL, off switch in Settings |
| Wallpaper refresh across midnight or system wake | 🕓 | Preserve the current cached image on failure; refresh asynchronously without blocking startup |
| Audit log drawer | ✅ | Applied instantly on 0.3.6 without restarting |
| Persona chips above the prompt | ✅ | Applied instantly on 0.3.6 without restarting |
| Phone "launch" actions | ✅ | 0.3.7. Allowlisted targets (terminal, vscode, dashboard) executed without shell |
| Telegram / Discord | 🕓 | After Signal |
| Quick-summon hotkey, clipboard/screenshot capture, Whisper, Ollama + HUD | 🕓 | |
| Obsidian search, secrets vault, fix-drafting watchdog, widget dock | 🕓 | |
| Consensus tribunal | ✅ | Discuss mode |

## Document 3: Multi-computer link (3rd update)

Goal: Consilium on the human's Mac and Windows PC find each other, stay linked and keep conversations,
REQUEST decisions and project state in sync, choosing the best working channel automatically and
switching channels without losing or duplicating messages. The document's Python/PySide6/asyncio code
is translated to Consilium's Rust/Tauri engine; the protocol requirements are kept as written.

| Item | Status | Notes |
|---|---|---|
| Wired/LAN TCP link, JSON-lines framing, static port 8470 | 🔨 | Engine done in `link.rs` (Claude): one JSON object per line, 3 MB line cap, frames sealed with ChaCha20-Poly1305 and every packet HMAC-checked, fresh challenge both ways per session. Tested between two link instances on one Mac; not yet between two real computers. Off until the human pairs or turns it on |
| Pairing and authentication | ✅ | Engine + Screen complete: one-time 6-digit code (approved by human), SPAKE2 key exchange so code cannot be guessed offline, key confirmation both ways, 5-min/3-try expiration, 0600 keys, countdown UI, and candidate subnet discovery (Gemini + Claude) |
| Dedup ledger (`sync_ledger`) | 🔨 | Wired into the link: reserve → dispatch → commit → acknowledge, so duplicates are acknowledged but not delivered twice (tested). Dispatch currently emits a `link-packet` event; writing received chat into the conversation store is next |
| 3 s heartbeat, 2 s reconnect with backoff | ✅ | Engine done: heartbeat every 3 s, silent link dropped after 10 s, redial from 2 s backing off to 30 s; unacknowledged packets re-sent and re-stamped on reconnect. Live status lights and RTT latency displayed per peer |
| Automatic discovery (mDNS/Bonjour) + manual IP fallback | 🔨 | Subnet detection & candidate IP auto-find on LAN in UI; manual host:port in Settings |
| Wireless: Wi-Fi LAN / Tailscale | 🔨 | Engine done: each side advertises all its addresses (cable, Wi-Fi, Tailscale); any reachable one works. Not tested across real networks yet |
| Auto-choose best channel and seamless failover | 🔨 | TCP part done: every known address is dialled at once and the first to answer is used; a dropped route is replaced on the next attempt and queued packets follow. Bluetooth and Signal fallback not built |
| What syncs | ⏭ | Chat messages, REQUEST decisions (applied once, on the machine that owns the turn), AI activity, project git state summary. No file contents or secrets in the first release |
| Bluetooth BLE/RFCOMM fallback | 🕓 | Rust BLE (`btleplug`) is central-only on macOS/Windows, so a Mac↔PC GATT server needs native per-OS code; built after the TCP link proves out, with MTU chunking + reassembly as specified |
| Phone bots (Telegram/Matrix) as out-of-band control | 🕓 | Shares the Signal receiver's safeguards (allowlist, replay, freshness); Telegram row in Document 2 |
| Cloud AI apps → local (Custom GPT Actions, Claude MCP via tunnel) | 🕓 | Exposes this computer to the internet; needs the human's explicit approval, an authenticated Cloudflare/Tailscale Funnel tunnel, an allowlisted command set and no shell access |
| "UI integration" (PySide6 signals) | ✅ | Topbar `LinkIndicator` with status LEDs (connected/disconnected/syncing/offline), `LinkSettings` in Settings, pairing code countdown, candidate subnet discovery, route & RTT meters, and Cmd+K palette hook |

## Reliability and portability added during the upgrade

| Item | Status | Notes |
|---|---|---|
| macOS / Windows / Linux feature parity | ✅ | Update Now, orphan cleanup, Signal setup and platform-specific shortcuts/installers use native paths on all three |
| Approval-gated native terminal capability | ✅ | Every Full/Own CLI agent receives OS-specific PowerShell/cmd or Unix-shell guidance. Administrator/root elevation requires a fresh exact-command REQUEST, is single-use, keeps UAC/sudo authentication in the OS, and forbids password capture or piping |
| In-chat command boxes with Run / Deny / Copy | ✅ | Fenced bash/zsh/PowerShell/cmd blocks from an AI get Run, Deny and Copy; Run reads the command from the stored message (never the screen), runs it once in the project folder with a 10-minute limit, uses the OS password/UAC prompt for admin lines, and returns the output to the suggesting AI. Every code block gets Copy |
| Adaptive support for older or weaker PCs | ✅ | Portable CPU/RAM profile caps local AI processes across the app and terminal; high pressure reduces polling and GPU-heavy visual effects |
| Crash-safe fail-forward behavior | ✅ | OS-held AI slots release after crashes; a broken capacity gate does not lock the user out; Stop remains cancellation-aware while queued |
| Clear Netrunner REQUEST callouts | ✅ | Opaque dark-red panel, brighter border/text and unblurred action buttons |
| Disk & temporary artifact cleanup script | ✅ | `scripts/clean.mjs` (`npm run clean`, `npm run clean:all`, `./scripts/clean.sh`) scrubs idle build workspaces, logs, test artifacts and optional caches with dry-run previews; live turns, builds, updates and capacity locks are preserved |
