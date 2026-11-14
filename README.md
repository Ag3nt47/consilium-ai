<p align="center"><img src="assets/icon-1024.png" width="128" alt="Consilium icon: two classical profiles facing each other under a Roman arch"></p>

<h1 align="center">Consilium</h1>
<p align="center"><i>A council of minds.</i></p>

Consilium is a desktop app where several AI assistants and you work in one conversation. Ask ChatGPT and Claude the same question side by side, or let them **discuss** it between themselves for a few rounds while you watch, step in, or press Stop.

- **No API keys.** ChatGPT takes part through OpenAI's Codex CLI and Claude through Anthropic's Claude Code CLI, each using your own existing sign-in. Consilium never sees a password, token or key.
- **One shared transcript**, color-coded by speaker. Anything an AI needs from you appears as a red **REQUEST:** callout.
- **Turn control.** *Ask* gets one answer from each AI you pick. *Discuss* lets them respond to each other for up to 8 rounds, and stops early when they're done or when one of them needs you. **Stop** ends it immediately.
- **Private by default.** Conversations stay on your computer. No telemetry. The only network call Consilium itself makes is an optional once-a-day check for a new release on GitHub.
- **Themes:** Consilium (indigo, ivory and gold), Cyber, Slate, CRT, and *Retro '84* with neon grid and scanlines.

Runs on macOS, Windows and Linux.

## Install

Download the installer for your system from [Releases](https://github.com/Ag3nt47/consilium-ai/releases), or use the one-line installer:

```bash
# macOS / Linux
curl -fsSL https://raw.githubusercontent.com/Ag3nt47/consilium-ai/main/install.sh | bash
```

```powershell
# Windows (PowerShell)
irm https://raw.githubusercontent.com/Ag3nt47/consilium-ai/main/install.ps1 | iex
```

The installers are not code-signed yet. On macOS the installer script removes the download quarantine for Consilium only; if you install the `.dmg` by hand, right-click the app and choose **Open** the first time. On Windows, choose **More info → Run anyway** in SmartScreen.

### First run

Consilium checks for both AI tools and shows a **Set up AIs** button for anything missing. It opens a terminal with the official install or sign-in command; press Enter, and sign in with your own account in the browser. You need at least one of:

| AI | Tool | Account |
| --- | --- | --- |
| ChatGPT | [Codex CLI](https://developers.openai.com/codex) (included in the ChatGPT desktop app on macOS) | ChatGPT plan |
| Claude | [Claude Code](https://claude.com/claude-code) | Claude plan |

Each AI's usage counts against your own plan with that provider.

## How it works

```
 You ─┐                      ┌─ Codex CLI (ChatGPT) ── your ChatGPT sign-in
      ├── Consilium core ────┤
      │   (Rust, event log)  └─ Claude Code CLI ───── your Claude sign-in
      └── React UI (Tauri)
```

Each AI turn starts the provider's CLI directly (never through a shell) in a fresh, empty temporary folder, with its tools turned off, and passes it the transcript. The AIs therefore see only the conversation, not your files. Replies are read from the CLIs' documented machine-readable output. Stop, or a 10-minute limit, kills the CLI and everything it started.

Conversations are append-only logs in the app's data folder (`~/Library/Application Support/io.github.ag3nt47.consilium` on macOS, `%APPDATA%\io.github.ag3nt47.consilium` on Windows, `~/.local/share/io.github.ag3nt47.consilium` on Linux).

## Build from source

Needs Node.js 20+, Rust, and on Linux the WebKitGTK libraries ([Tauri prerequisites](https://v2.tauri.app/start/prerequisites/)).

```bash
npm install
npm run app:dev      # run with live reload
npm run app:build    # build installers into src-tauri/target/release/bundle
```

Tests: `npm test` (privacy gate) and `cargo test` in `src-tauri`.

## Project layout

| Path | What |
| --- | --- |
| `src/` | React user interface |
| `src-tauri/src/` | Rust core: `store.rs` (event log), `agents.rs` (CLI adapters), `prompt.rs` (what each AI is told), `setup.rs` (Set up AIs), `lib.rs` (turn-taking) |
| `scripts/check-secrets.mjs` | Privacy gate: blocks keys, tokens and personal data from being committed |
| `self-improvement/` | How the AIs log issues and agree on patches, cheaply |
| `tools/doctor.mjs` | Agent Setup Kit: which AIs are installed and signed in, and the exact next step for any that aren't (`--json` for an AI to read) |
| `install.sh`, `install.ps1` | One-line installers |
| `docs/ROADMAP.md` | What's next, including voice mode and mobile |

## Security and privacy

See [SECURITY.md](SECURITY.md) and [PRIVACY.md](PRIVACY.md). Report vulnerabilities privately through GitHub's **Report a vulnerability** button on the Security tab.

## License

MIT; see [LICENSE](LICENSE). Visual design values for the Cyber, Slate and CRT themes come from the [UI Design Bundle](https://github.com/Ag3nt47/UI-Design-Bundle); see [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md). Consilium is an independent project, not affiliated with OpenAI or Anthropic. ChatGPT, Codex, Claude and Claude Code are trademarks of their owners.
