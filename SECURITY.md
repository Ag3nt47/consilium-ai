# Security policy

## Reporting a vulnerability

Please report security problems **privately** with GitHub's [Report a vulnerability](https://github.com/Ag3nt47/consilium-ai/security/advisories/new) form, not in a public issue. Include what you found, how to reproduce it, and which version you used. You'll get an acknowledgement within a week.

## Supported versions

Only the latest release receives security fixes.

## How Consilium is built to be safe

- **No credentials.** Consilium never asks for, stores, or transmits API keys, passwords or tokens. ChatGPT and Claude are reached through their official CLIs, which keep their own sign-ins.
- **Isolated AI turns.** Each AI CLI is started directly (no shell), in an empty temporary folder that is deleted afterwards, with its tools disabled (Claude Code: `--tools ""`, no MCP servers, no saved session; Codex: read-only sandbox). Stop and a 10-minute limit kill the CLI's whole process group.
- **Bounded automation.** AI-to-AI discussion is capped at 8 rounds and stops when every AI passes or one asks the human something.
- **Strict content policy** in the app window: scripts only from the app itself; network access only to the GitHub releases API.
- **Setup commands are fixed strings** in the Rust code, shown in a terminal and run only after the person presses Enter.
- **Updates are never installed automatically.** The app only reports that a release exists.

## Repository protections

- `scripts/check-secrets.mjs` blocks API keys, tokens, private keys, credential files, conversation logs, personal home-directory paths and email addresses. It runs in CI on every push and pull request, and as a local pre-commit hook after `scripts/install-hooks.sh`.
- GitHub secret scanning with push protection, Dependabot alerts and security updates, and CodeQL code scanning (JavaScript/TypeScript, Rust, GitHub Actions) are enabled.
- Workflows run with read-only `GITHUB_TOKEN` permissions unless a job needs more.
