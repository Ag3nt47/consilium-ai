# Roadmap

## v0.1 (this release)

- ChatGPT (Codex CLI) and Claude (Claude Code CLI) in one conversation, using existing sign-ins
- Ask and Discuss modes, round cap, Stop
- Red `REQUEST:` callouts for anything an AI needs from you
- Five themes, including Retro '84
- Set up AIs button; daily release check (notify only)
- Privacy gate, CodeQL, Dependabot, secret scanning; CI on macOS, Windows and Linux

## v0.2: Shared Projects, then usage panel

Shared Projects comes first (see v0.3 notes below, moved up). Then a **Usage panel** per AI:

- Tokens used per turn, per conversation and per day, from each CLI's official machine-readable output (Codex `--json` `turn.completed.usage`; Claude Code JSON result `usage`).
- A budget you set ("warn me at N tokens a day"), with a red `REQUEST:` before a Discuss run would exceed it.
- Plan quota (% of your ChatGPT or Claude plan left) is shown as *Unavailable*: neither CLI exposes it in a supported output, and Consilium won't estimate it.

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

- Signed and notarized installers (needs Apple Developer and Windows code-signing certificates).
- Auto-update with signature verification.
- iOS and Android through Tauri's mobile targets; the UI is already responsive down to phone width.
