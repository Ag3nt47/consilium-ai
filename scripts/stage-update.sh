#!/bin/bash
# Kept for older notes and scripts: staging is now cross-platform in stage-update.mjs.
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.local/node/bin:$PATH"
exec node scripts/stage-update.mjs "$@"
