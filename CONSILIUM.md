# Project notes for the council

Every AI working in this folder through Consilium receives this file. Keep it short and current.

## What this is

Consilium: a Tauri 2 desktop app (Rust back end in `src-tauri/`, React + TypeScript front end in `src/`) where several AIs and a human share one conversation.

## Build and test

- Front end: `npm run build` (type-check and bundle).
- Rust: `cargo test` and `cargo clippy --all-targets -- -D warnings` in `src-tauri/`. CI runs both on macOS, Windows and Linux, so avoid platform-only imports outside `cfg` blocks.
- App: `npm run app:build:mac` produces `src-tauri/target/release/bundle/macos/Consilium.app` without packing a `.dmg` (that step opens a Finder window that looks like an installer). `npm run app:build` builds every installer type.
- Updates: `npm run update:stage` (macOS, Windows, Linux) builds and stages a full build for the in-app **Update Now** (restart): an `.app` on macOS, the program plus a `VERSION` file in `<data>/update/next` elsewhere (and the AppImage on Linux). If only the screen changed (`src/`, nothing in `src-tauri`), `npm run update:ui` stages it for **Apply Now · instant**, which reloads the page with no restart; the script refuses when the engine differs from the installed build (fingerprint in `src-tauri/build.rs` and `scripts/backend-fingerprint.mjs`, which must stay in step).
- Privacy gate: `node scripts/check-secrets.mjs`. Never commit keys, tokens, personal paths or conversation logs.
- Disk & artifact cleanup: `npm run clean` (or `./scripts/clean.sh`) removes staging workspaces, logs, old test artifacts and `dist/`. Temporary artifacts must be inactive for an hour; active turns, builds, updates and capacity locks are preserved. `npm run clean:all` also removes the Rust build cache and cached repo clones when they are idle. Use `--dry-run` to preview reclaimed bytes.
- Pre-commit hook (`.githooks/pre-commit`, enable with `git config core.hooksPath .githooks`): runs the secret scan on staged files, then type-checks and tests an export of exactly what is staged, so a partial commit that picks up or misses another AI's lines is refused. Rust is left to `cargo` and CI.

## How to work

- Small, focused commits with a clear message; tests with every behavior change.
- An update is not delivered until its focused commit is integrated onto `main`, pushed to `origin/main`, and GitHub checks pass on macOS, Windows and Linux. Stage or publish from that pushed `main` commit, never from an unpushed side branch.
- Before changing a file another participant may have touched this turn, re-read it.
- For long JavaScript or TypeScript files, run `node scripts/symbol-map.mjs <file>` first, then read the relevant line ranges. To locate an inner declaration, pass the top-level function and a name filter, such as `node scripts/symbol-map.mjs src/App.tsx App send`.
- Full-capacity CLI agents may use the host's native PowerShell/Command Prompt or Unix shells. Administrator/root elevation always needs a fresh REQUEST for one exact command and the normal visible OS authentication prompt; never collect or pipe credentials.
- Treat `docs/UPGRADE_STATUS.md` as the canonical backlog for the six Word blueprints. For **Request App Update**, prefer its in-progress and next items before later roadmap work and update it when work is verified.
- New AI connectors follow `docs/CONNECTORS.md`; record known issues in `self-improvement/ledger.json`.
- Ask the human (REQUEST:) before publishing a release, changing repository settings, or anything that can't be undone.
