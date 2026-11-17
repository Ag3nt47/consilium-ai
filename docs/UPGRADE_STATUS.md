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
| Add any AI, not just the built-in ones | 🔨 | Add AI has an "Add your own AI" form: any OpenAI-compatible service with the person's own key (presets for OpenRouter, Mistral, Groq, Together, OpenAI API) or a local model (Ollama, LM Studio; plain http only to this computer or the home network). Saved in `custom-ais.json`, keys kept apart in `api-keys.json`; added AIs can be deleted. Tests pass; not yet tried against a real service. |
| Devil's advocate in round 3+ | ✅ | Rotating reviewer |
| Context sliding window (summarise long sessions) | ✅ | Over 120k prompt characters, older turns become a bounded local digest prioritising human intent, REQUEST decisions and concrete outcomes; the newest 104k remain verbatim. Current file diffs are separately hashed and bounded |
| File-state hashing / git diff in each turn | ✅ | Bounded, redacted status/diff snapshot with a hard Git subprocess timeout before each editable turn |
| Destructive command sanitizer for Full capacity | 🔨 | Provider-native permission modes remain primary. An emergency brake now stops observable broad data-loss/disk/OS commands at the structured pre-execution stream boundary; provider ordering can change, so OS-enforced worktree isolation remains next |
| Secret redaction | ✅ | `redact.rs`, plus `check-secrets.mjs` before pushes |
| Git worktree per agent in Full capacity | ✅ | With `isolate_agents` on in Settings, Full-capacity AIs work in isolated git worktrees on branch `consilium/<id>` (`src-tauri/src/worktree.rs`); shared Rust `target` (via parent `.cargo/config.toml`) and `node_modules` symlink; idle merged copies fast-forward; conflict-aborting merge; interactive Branches drawer (`src/branches.tsx`) and Settings toggle to review ahead/behind/uncommitted commits, merge into main, or discard |
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
| Signal phone bridge, phone Approve/Deny | ✅ | `signal_chat.rs`: a "Consilium" group plus one group per AI, created on the linked account; the human's messages there (or in Note to Self) become their message in the newest conversation, Approve/Deny answers a single pending REQUEST, and AI replies and Consilium notes go back to the group they were asked from. Groups were created on the real account; the two-way loop hasn't been seen in the running app yet. Seen working 2026-11-16: the human's messages from the Signal groups reached the conversation; answering a REQUEST with Approve or Deny from the phone has not been tried yet |
| Voice visualizer, bars + orb, cyan/violet | ✅ | 0.3.6. AI side is an animation: system speech has no audio samples |
| Voice interrupt with 5-second resume; spoken "Consilium, …" commands, programs opened only after a click | 🟡 | 0.3.22. Not yet tried with a real microphone and speakers |
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
| Dedup ledger (`sync_ledger`) | 🔨 | Wired into the link: reserve → durable inbox → commit → acknowledge, so duplicates are acknowledged but not delivered twice. The outgoing queue also survives restart; interrupted inbox tails are repaired before a retry is appended. Importing those durable packets into the conversation store is next |
| 3 s heartbeat, 2 s reconnect with backoff | ✅ | Engine done: heartbeat every 3 s, silent link dropped after 10 s, redial from 2 s backing off to 30 s; unacknowledged packets re-sent and re-stamped on reconnect. Live status lights and RTT latency displayed per peer |
| Automatic discovery (mDNS/Bonjour) + manual IP fallback | 🔨 | Subnet detection & candidate IP auto-find on LAN in UI; manual host:port in Settings |
| Wireless: Wi-Fi LAN / Tailscale | 🔨 | Engine done: each side advertises all its addresses (cable, Wi-Fi, Tailscale); any reachable one works. Not tested across real networks yet |
| Auto-choose best channel and seamless failover | 🔨 | TCP part done: every known address is dialled at once and the first to answer is used; a dropped route is replaced on the next attempt and queued packets follow. Bluetooth and Signal fallback not built |
| Test connection button and route diagnostics | ✅ | Engine `test_peer` in `link.rs` and UI button in Settings: live route latency probes across all interfaces (LAN, Link-Local/Bluetooth, Tailscale), handshake verification, and outbox queue alignment audit (Gemini) |
| What syncs | 🔨 | Built (Claude, 0.3.28 and 0.3.30, `peer_sync.rs`): new chat messages copy to linked computers as a per-computer copy of the conversation; REQUEST decisions and replies typed in a copy go back to the owning computer and are applied once there; AIs from another computer are nicknamed by computer; copied lines never run requests, tasks or notes; 40-per-minute flood stop. Only one linked computer answers each Signal message (0.3.27). Each heartbeat also carries which AIs are answering on that computer ("Claude in Plans") and a one-line git summary of its project folder ("consilium-ai · main · 2 changed"), shown per computer in Settings → Linked computers; both are cleaned and size-capped on arrival (Claude, 0.3.33). The first time a conversation is shared, up to 200 earlier messages go first as one packet, filed once and never duplicated; link-local 169.254 routes are accepted so a direct cable, Thunderbolt bridge or Bluetooth network works without a router (Claude, peer-history branch). Unit-tested only; never run between two real computers |
| Bluetooth BLE/RFCOMM fallback | 🕓 | Rust BLE (`btleplug`) is central-only on macOS/Windows, so a Mac↔PC GATT server needs native per-OS code; built after the TCP link proves out, with MTU chunking + reassembly as specified |
| Phone bots (Telegram/Matrix) as out-of-band control | 🕓 | Shares the Signal receiver's safeguards (allowlist, replay, freshness); Telegram row in Document 2 |
| Cloud AI apps → local (Custom GPT Actions, Claude MCP via tunnel) | 🕓 | Exposes this computer to the internet; needs the human's explicit approval, an authenticated Cloudflare/Tailscale Funnel tunnel, an allowlisted command set and no shell access |
| "UI integration" (PySide6 signals) | ✅ | Topbar `LinkIndicator` with status LEDs (connected/disconnected/syncing/offline), `LinkSettings` in Settings, pairing code countdown, candidate subnet discovery, route & RTT meters, and Cmd+K palette hook |

## Reliability and portability added during the upgrade

| Item | Status | Notes |
|---|---|---|
| macOS / Windows / Linux feature parity | ✅ | Update Now, orphan cleanup, Signal setup and platform-specific shortcuts/installers use native paths on all three |
| Windows setup: npm install fails on a broken cache | ✅ | `setup.rs` (Claude, 0.3.33): the Windows setup window retries a global npm install once with `--cache %TEMP%\consilium-npm-cache` after `npm error syscall mkdir … npm-cache\_cacache\tmp`, and reports failure instead of always printing Done. Unit-tested; not yet seen on the Windows PC |
| Approval-gated native terminal capability | ✅ | Every Full/Own CLI agent receives OS-specific PowerShell/cmd or Unix-shell guidance. Administrator/root elevation requires a fresh exact-command REQUEST, is single-use, keeps UAC/sudo authentication in the OS, and forbids password capture or piping |
| AIs seek tools and skills; installs need approval | ✅ | `toolbox.rs` (Claude): Full/Own AIs are told to search for tools, CLIs, MCP servers and skills on their own and are shown which package managers this host has. Project-local dependencies stay ordinary work; any system-wide install (brew, winget, scoop, choco, apt, global npm/pip/cargo/go/gem, gh extensions, MCP/skill registration, curl\|sh) is stopped before it runs and turned into `REQUEST: … INSTALL_COMMAND_JSON:`; the approved exact command runs once in the next turn. An install grant never covers elevation. Verified by unit tests on the detector, the grant and a fake provider stream; not yet seen in a live turn |
| Idle, cross-platform spyware scan adapter | 🔨 | Zero Breach Toolkit 3.0.2 was source-reviewed: cross-platform, read-only, offline during scans, explicit JSON/exit-code contract. The adapter is built with private reports, weekly quick scans, a five-minute idle gate, and automatic cancellation/retry when AI work or high resource pressure returns. It remains in progress until the separately approved pinned scanner is installed and an end-to-end scan is verified. AnonRouter was evaluated but is confidential AI inference, not general web protection, and is not installed as a web-security control.. 2026-11-16: the pinned 3.0.2 script is installed and its SHA-256 matches. A manual run with the adapter's exact arguments completed end to end on this Mac (5 min 20 s, exit 3 = no known indicators, some evidence unreadable without Full Disk Access; private 0600 JSON and text reports written). That run found the in-app gate never opened: it compared max(CPU, memory) with 35%, and memory sits near 63% on an 8 GB Mac. 0.3.32 gates on CPU and only blocks for memory at 90%. Stays 🔨 until an in-app idle scan is seen finishing |
| Saved "Always allow" approvals | ✅ | Admin and install REQUESTs with an exact command get an **Always allow** button. The exact command is saved in `council.json` on this computer; every Full/Own AI is told it is pre-approved and may run it without asking again, character for character. Settings → Saved approvals lists and removes them. The OS password/UAC prompt still appears. Unit-tested; not yet seen in a live turn. Seen 2026-11-16: three saved install commands appear in every AI's prompt |
| In-chat command boxes with Run / Deny / Copy | ✅ | Fenced bash/zsh/PowerShell/cmd blocks from an AI get Run, Deny and Copy; Run reads the command from the stored message (never the screen), runs it once in the project folder with a 10-minute limit, uses the OS password/UAC prompt for admin lines, and returns the output to the suggesting AI. Every code block gets Copy |
| Adaptive support for older or weaker PCs | ✅ | Portable CPU/RAM profile caps local AI processes across the app and terminal; high pressure reduces polling and GPU-heavy visual effects |
| Crash-safe fail-forward behavior | ✅ | OS-held AI slots release after crashes; a broken capacity gate does not lock the user out; Stop remains cancellation-aware while queued |
| Clear Netrunner REQUEST callouts | ✅ | Opaque dark-red panel, brighter border/text and unblurred action buttons |
| Compact conversation controls | ✅ | Messaging, peer-link, status, audit, repo, copy and delete controls fit in one slim icon row with tooltips and accessible labels; the chat starts directly below it without a narrow-window second row |
| Disk & temporary artifact cleanup script | ✅ | `scripts/clean.mjs` (`npm run clean`, `npm run clean:all`, `./scripts/clean.sh`) scrubs idle build workspaces, logs, test artifacts and optional caches with dry-run previews; live turns, builds, updates and capacity locks are preserved |
| GitHub checks light and alerts | ✅ | `ci.rs` (Claude): a background watcher reads each active project's GitHub checks with `gh` about once a minute (cancelled runs replaced by a newer push are skipped). The title bar shows a green/red/amber CI light beside the versions tab; every AI's prompt carries the last result so nobody polls `gh run list`; a Consilium note is posted in each conversation using that project when the newest commit's checks pass or fail. Unit-tested and read live against this repo; not yet seen in the running app. Seen 2026-11-16: pass notes were posted in the chat and every AI's prompt carries the result; the light itself was not checked on screen by an AI |
| Auto-fix request on failing checks | 🟡 | `ci.rs` + `Engine::note_for_council` (Claude): when the newest commit's checks fail, the most recent conversation for that project gets a Consilium note addressed to every member asking them to agree who fixes it (claiming with TASK:), and starts one round if no turn is running. Not yet seen in a real turn |
| Sidebar scrolling and Netrunner see-through chat | 🟡 | `App.tsx`/`styles.css` (Claude): the conversation list keeps a share of the sidebar and scrolls; the council list scrolls from the fifth member; in Netrunner over the wallpaper the message panel is transparent and each message is on dark glass. Seen only in tests and type checks |
- **Done (0.3.19):** Tool catalog of not-bundled tools (AnonRouter, Zero Breach Toolkit, Ghidra, JADX) shown to every shell-capable AI with its install state, so an AI can recognise when one fits and request the install. Nothing is downloaded with Consilium. AnonRouter verified as `anonrouter-confidential` on PyPI (requires Python >= 3.10 and OpenSSL).
- **Done:** Settings → Tools page listing every catalog tool (AnonRouter, Zero Breach Toolkit, REA, Ghidra, JADX) with its install state. Install sends the pinned command to one chosen AI as the human's approval for that single install (the engine grants it only when the command matches the catalog exactly and the message is the newest human message addressed to that AI); tools without a pinned command get "Ask an AI". Not yet seen in the running app. Used 2026-11-16: the human installed Zero Breach, Ghidra and JADX through it.
- **Done (0.3.19):** Signal linking renders in-terminal ANSI QR codes via `scripts/qr.mjs` without requiring `qrencode`, uses unique per-machine device names (`Consilium (<Host>)`), and documents ecosystem routing over Document 3 peer sync. Fixed Windows unused-variable lint in `security_scan.rs`.
| Shared task checklist and Consilium task notes | ✅ | `tasks.rs` + `src/tasks.tsx` (Claude): AIs write `TASK: take/done/blocked/hand/add/drop` lines; the board is kept per conversation in `<id>.tasks.json`, shown in the sidebar with progress, editable by the human, given to every AI each turn, and each change posts a Consilium note naming who did what plus % done. Per-AI task receipts are unchanged. Unit-tested; not yet seen in the running app. Seen 2026-11-16: TASK lines from Claude and Gemini produced Consilium task notes with % done; the board now sits in a clipboard box at the top right of the chat |
| Rename the conversation (human and AI) | 🟡 | Click the title to edit it in place (the old double-click used `prompt()`, which shows nothing in the app window); AIs rename it with a `TITLE:` line and Consilium notes the change. Not yet seen in the running app |
