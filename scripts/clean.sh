#!/usr/bin/env bash
# Consilium cleanup: removes leftover build workspaces, logs, and temp artifacts.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export PATH="$HOME/.local/node/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"

exec node "$ROOT/scripts/clean.mjs" "$@"
