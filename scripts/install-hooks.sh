#!/usr/bin/env bash
# Installs a git pre-commit hook that runs the privacy gate on staged changes, so keys, tokens and
# personal data are caught before they are even committed. Run once per clone.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
hook=.git/hooks/pre-commit
cat > "$hook" <<'EOF'
#!/usr/bin/env bash
exec node scripts/check-secrets.mjs --staged
EOF
chmod 755 "$hook"
echo "Installed $hook (privacy gate on every commit)."
