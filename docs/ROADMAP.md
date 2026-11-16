# Roadmap

Implementation status for both supplied upgrade documents is tracked in
[`UPGRADE_STATUS.md`](UPGRADE_STATUS.md). That ledger distinguishes finished work from the active,
next, later, and platform-limited items.

## v0.1 (this release)

- ChatGPT (Codex CLI) and Claude (Claude Code CLI) in one conversation, using existing sign-ins
- Ask and Discuss modes, round cap, Stop
- Red `REQUEST:` callouts for anything an AI needs from you
- Five themes, including Retro '84
- Set up AIs button; daily release check (notify only)
- Privacy gate, CodeQL, Dependabot, secret scanning; CI on macOS, Windows and Linux

## v0.2: Shared Projects and usage

Shared Projects comes first (see v0.3 notes below, moved up). Each council card now shows the
provider-reported plan allowance remaining, from 0–100%:

- The lowest applicable limit is the headline because it is the one that will stop the AI first;
  hover the meter to see every reported window.
- Codex uses `account/rateLimits/read`, Claude uses its `get_usage` control request, and Antigravity
  uses headless `/usage`. These are read-only CLI operations and do not spend model quota.
- If a connector or account type does not expose plan limits, the UI says *usage unavailable*;
  Consilium never estimates a plan percentage.

A larger **Usage panel** now shows per-agent conversation and daily token totals. Remaining budget work:

- The back end records tokens per turn from each CLI's official machine-readable output (Codex
  `--json` `turn.completed.usage`, Claude Code JSON result `usage`, and Antigravity stream-json
  result `usage`) and exposes them in the Usage panel as conversation/day totals.
- A budget you set ("warn me at N tokens a day"), with a red `REQUEST:` before a Discuss run would exceed it.

## v0.2: Voice Studio

A toggleable voice mode now reads AI replies aloud and, where the WebView supports it, sends the
human's transcribed speech after a pause. macOS Dictation is the local fallback. Future extensions:

- A distinct, configurable installed voice for each participant. ✅
- A central live workspace showing the current file, code selection, proposed diff, command, test result or task status.
- One merged explanation panel in which every segment keeps its speaker's color (ChatGPT green, Claude amber, you ivory).
- Visible mic state, interrupt-to-stop, and transcripts that land in the normal conversation log.

## v0.3: More agents and API providers

- OpenAI and Anthropic API connectors as an alternative to the CLIs, keys kept in the OS keychain.
- Local and OpenAI-compatible models (for example Ollama), behind the same agent interface.

## v0.3: Shared projects

- A **Projects** folder the whole council works in: each project is a git repository everyone sees, so the human, ChatGPT and Claude stay on the same page.
- Add an existing project from anywhere, including one already open in the ChatGPT/Codex app or Claude Code. Consilium links the folder rather than copying it.
- Per-project, per-agent permissions (read only, propose edits, or edit), off by default; edits arrive as diffs to approve, like the self-improvement flow.
- Push and pull from the app, with the privacy gate running before every push.

## Later

- **Signal & cross-workstation mobile messaging:** A Signal CLI bridge carries messages over Signal's normal network path and syncs across owned Windows and Mac workstations. Offline Bluetooth is a separate companion-app transport because the Signal phone app does not expose a custom BLE channel. Phone numbers and contact metadata stay in local configuration and are never pushed to GitHub (see `docs/MESSAGING_BRIDGE.md`).
- Signed and notarized installers (needs Apple Developer and Windows code-signing certificates).
- Auto-update with signature verification.
- iOS and Android through Tauri's mobile targets, including phone-width navigation and touch-first controls.

## Second update blueprint (triaged 2026-11-15)

The blueprint's sample code targets Python, PySide6 and FastAPI. Consilium is Tauri (Rust) with a React screen, so each item is rebuilt in that stack rather than pasted in.

**Next, in order**
1. Instant screen-only updates (approved; in progress).
2. Signal phone bridge (approved). Phone buttons reuse the in-chat Approve/Deny decisions. Incoming messages are allowlisted by Signal ACI/UUID, deduplicated, and accepted only through a local-only daemon connection. A "launch" action may only open apps from a fixed list in local config and runs without a shell.
3. Voice visualizer: spectrum bars and a pulsing orb, a setting to pick one, cyan while the human speaks and violet while an AI speaks. Microphone input can use real analyser data and is never routed to the speakers. The current browser system TTS does not expose audio samples, so AI speech uses a clearly documented speaking-state animation unless the TTS path is later changed to playable audio.
4. Daily Bing wallpaper behind the chat, cached in the app data folder, with a dark frosted layer for readability and an off switch.
5. Audit log drawer: every approval, update, launch and phone message, newest first, with secrets masked.
6. Persona chips above the prompt box (Security Auditor, Code Refactor, General) that change the instructions for the next messages.

**Later**
- Global quick-summon hotkey with a floating prompt window.
- Clipboard and window-screenshot capture into the prompt (screenshot needs macOS screen-recording permission).
- Local Whisper speech-to-text, replacing the WebKit recogniser that the app window may not provide.
- Ollama as a local AI, then a token-speed and context HUD for it.
- Telegram and Discord bridges, after Signal works.
- Local search over an Obsidian vault (nothing leaves the machine).
- Encrypted local secrets vault, released per action by an approval.
- Background watchdog that drafts fixes for failing tests and leaves a branch to review, never merging.
- Desktop widget dock.

**Already built:** multi-AI Discuss mode with cross-review and a rotating devil's advocate covers the "consensus tribunal" item.
