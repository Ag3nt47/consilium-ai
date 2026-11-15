# Roadmap

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

A larger **Usage panel** per AI remains planned:

- The back end now records tokens per turn from each CLI's official machine-readable output (Codex
  `--json` `turn.completed.usage`, Claude Code JSON result `usage`, and Antigravity stream-json
  result `usage`) and exposes conversation/day totals. The Usage panel UI remains to be built.
- A budget you set ("warn me at N tokens a day"), with a red `REQUEST:` before a Discuss run would exceed it.

## v0.2: Voice Studio

A toggleable voice mode where you and both AIs talk:

- Push-to-talk by default, optional hands-free. Local speech recognition and text-to-speech first; any cloud voice service optional and clearly labeled.
- A distinct, configurable voice for each participant.
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

- **Signal & cross-workstation mobile messaging:** Mobile communication bridge (Signal CLI daemon, encrypted relay, offline Bluetooth Low Energy mesh) allowing the human to message conversations from their phone over the internet or offline Bluetooth; syncs across owned Windows and Mac workstations while ensuring phone numbers and contact metadata are scrubbed and never pushed to GitHub (see `docs/MESSAGING_BRIDGE.md`).
- Signed and notarized installers (needs Apple Developer and Windows code-signing certificates).
- Auto-update with signature verification.
- iOS and Android through Tauri's mobile targets, including phone-width navigation and touch-first controls.
