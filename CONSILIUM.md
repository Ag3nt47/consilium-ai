# Project notes for the council

Every AI working in this folder through Consilium receives this file. Keep it short and current.

## What this is

Consilium: a Tauri 2 desktop app (Rust back end in `src-tauri/`, React + TypeScript front end in `src/`) where several AIs and a human share one conversation.

## Build and test

- Front end: `npm run build` (type-check and bundle).
- Rust: `cargo test` and `cargo clippy --all-targets -- -D warnings` in `src-tauri/`. CI runs both on macOS, Windows and Linux, so avoid platform-only imports outside `cfg` blocks.
- App: `npm run app:build:mac` produces `src-tauri/target/release/bundle/macos/Consilium.app` without packing a `.dmg` (that step opens a Finder window that looks like an installer). `npm run app:build` builds every installer type.
- Updates: `npm run update:stage` builds and stages a full build for the in-app **Update Now** (restart). If only the screen changed (`src/`, nothing in `src-tauri`), `npm run update:ui` stages it for **Apply Now · instant**, which reloads the page with no restart; the script refuses when the engine differs from the installed build (fingerprint in `src-tauri/build.rs` and `scripts/backend-fingerprint.mjs`, which must stay in step).
- Privacy gate: `node scripts/check-secrets.mjs` (also the pre-commit hook). Never commit keys, tokens, personal paths or conversation logs.

## How to work

- Small, focused commits with a clear message; tests with every behavior change.
- Before changing a file another participant may have touched this turn, re-read it.
- New AI connectors follow `docs/CONNECTORS.md`; record known issues in `self-improvement/ledger.json`.
- Ask the human (REQUEST:) before publishing a release, changing repository settings, or anything that can't be undone.
