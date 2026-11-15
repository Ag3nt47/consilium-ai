# Project notes for the council

Every AI working in this folder through Consilium receives this file. Keep it short and current.

## What this is

Consilium: a Tauri 2 desktop app (Rust back end in `src-tauri/`, React + TypeScript front end in `src/`) where several AIs and a human share one conversation.

## Build and test

- Front end: `npm run build` (type-check and bundle).
- Rust: `cargo test` and `cargo clippy --all-targets -- -D warnings` in `src-tauri/`. CI runs both on macOS, Windows and Linux, so avoid platform-only imports outside `cfg` blocks.
- App: `npm run app:build:mac` produces `src-tauri/target/release/bundle/macos/Consilium.app` without packing a `.dmg` (that step opens a Finder window that looks like an installer). `npm run app:build` builds every installer type.
- Privacy gate: `node scripts/check-secrets.mjs` (also the pre-commit hook). Never commit keys, tokens, personal paths or conversation logs.

## How to work

- Small, focused commits with a clear message; tests with every behavior change.
- Before changing a file another participant may have touched this turn, re-read it.
- New AI connectors follow `docs/CONNECTORS.md`; record known issues in `self-improvement/ledger.json`.
- Ask the human (REQUEST:) before publishing a release, changing repository settings, or anything that can't be undone.
