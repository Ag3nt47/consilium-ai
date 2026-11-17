#!/usr/bin/env bash
# github-setup.sh: cross-platform GitHub & Git setup tool for Consilium and AIs (macOS & Linux).
# Verifies Git and GitHub CLI (gh), sets up credential helpers, ensures safe commit identities,
# and clones/connects repositories without freezing non-interactive sessions.

set -euo pipefail

# Locate tools across standard and user-local directories
find_bin() {
    local name="$1"
    if command -v "$name" >/dev/null 2>&1; then
        command -v "$name"
        return 0
    fi
    for p in "$HOME/.local/bin/$name" "$HOME/.local/node/bin/$name" "/opt/homebrew/bin/$name" "/usr/local/bin/$name" "/usr/bin/$name"; do
        if [ -x "$p" ]; then
            echo "$p"
            return 0
        fi
    done
    return 1
}

GIT_BIN="$(find_bin git || true)"
GH_BIN="$(find_bin gh || true)"

cmd_status() {
    local json=false
    if [ "${1:-}" = "--json" ]; then json=true; fi

    local git_ver="" user_name="" user_email="" gh_ver="" gh_user="" cred_helper=""

    if [ -n "$GIT_BIN" ]; then
        git_ver="$("$GIT_BIN" --version 2>/dev/null || true)"
        user_name="$("$GIT_BIN" config --global --get user.name 2>/dev/null || true)"
        user_email="$("$GIT_BIN" config --global --get user.email 2>/dev/null || true)"
        cred_helper="$("$GIT_BIN" config --global --get-all credential.https://github.com.helper 2>/dev/null || true)"
    fi

    if [ -n "$GH_BIN" ]; then
        gh_ver="$("$GH_BIN" --version 2>/dev/null | head -n 1 || true)"
        gh_user="$("$GH_BIN" api user --jq .login 2>/dev/null || true)"
    fi

    local in_git=false repo_origin="" current_branch=""
    if [ -n "$GIT_BIN" ] && "$GIT_BIN" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
        in_git=true
        repo_origin="$("$GIT_BIN" remote get-url origin 2>/dev/null || true)"
        current_branch="$("$GIT_BIN" branch --show-current 2>/dev/null || true)"
    fi

    local has_gh_helper=false
    if echo "$cred_helper" | grep -q "auth git-credential"; then
        has_gh_helper=true
    fi

    if [ "$json" = true ]; then
        cat <<EOF
{
  "git": {
    "available": $([ -n "$GIT_BIN" ] && echo "true" || echo "false"),
    "path": "${GIT_BIN:-}",
    "version": "$git_ver",
    "userName": "$user_name",
    "userEmail": "$user_email",
    "hasGhHelper": $has_gh_helper
  },
  "gh": {
    "available": $([ -n "$GH_BIN" ] && echo "true" || echo "false"),
    "path": "${GH_BIN:-}",
    "version": "$gh_ver",
    "authenticated": $([ -n "$gh_user" ] && echo "true" || echo "false"),
    "account": "$gh_user"
  },
  "currentRepo": {
    "isRepo": $in_git,
    "origin": "$repo_origin",
    "branch": "$current_branch"
  }
}
EOF
        return 0
    fi

    echo "=== Consilium GitHub & Git Setup Status ==="
    if [ -n "$GIT_BIN" ]; then
        echo "✓ Git: $git_ver ($GIT_BIN)"
        echo "  - user.name:  ${user_name:-<not set>}"
        echo "  - user.email: ${user_email:-<not set>}"
        if [ "$has_gh_helper" = true ]; then
            echo "  - Credential helper: configured (gh auth git-credential)"
        else
            echo "  - Credential helper: not yet linked to gh (run '$0 setup')"
        fi
    else
        echo "✗ Git: not found on PATH"
    fi

    echo ""
    if [ -n "$GH_BIN" ]; then
        echo "✓ GitHub CLI: $gh_ver ($GH_BIN)"
        if [ -n "$gh_user" ]; then
            echo "✓ Account: @$gh_user (Authenticated)"
        else
            echo "! Account: Not logged in (run '$0 login')"
        fi
    else
        echo "✗ GitHub CLI: not installed (run '$0 install')"
    fi

    if [ "$in_git" = true ]; then
        echo ""
        echo "Current Directory Repository:"
        echo "  - Remote: ${repo_origin:-<no remote>}"
        echo "  - Branch: $current_branch"
    fi
    echo "============================================"
}

cmd_setup() {
    if [ -z "$GH_BIN" ]; then
        echo "Error: GitHub CLI (gh) is not installed. Run '$0 install' first." >&2
        return 1
    fi
    if [ -z "$GIT_BIN" ]; then
        echo "Error: Git is not installed." >&2
        return 1
    fi

    local gh_user
    gh_user="$("$GH_BIN" api user --jq .login 2>/dev/null || true)"
    if [ -z "$gh_user" ]; then
        echo "Error: Not signed in to GitHub. Run '$0 login' first." >&2
        return 1
    fi

    echo "Configuring Git credential helper for https://github.com..."
    "$GH_BIN" auth setup-git
    echo "✓ Credential helper configured."

    local user_name user_email
    user_name="$("$GIT_BIN" config --global --get user.name 2>/dev/null || true)"
    user_email="$("$GIT_BIN" config --global --get user.email 2>/dev/null || true)"

    if [ -z "$user_name" ] || [ -z "$user_email" ]; then
        echo "Retrieving GitHub identity for safe commit attribution..."
        local id name login
        read -r id login name <<< "$("$GH_BIN" api user --jq '[.id, .login, (.name // "")] | @tsv' 2>/dev/null || true)"
        if [ -n "$id" ] && [ -n "$login" ]; then
            if [ -z "$user_name" ]; then
                local safe_name="${name:-$login}"
                "$GIT_BIN" config --global user.name "$safe_name"
                echo "✓ Set user.name = '$safe_name'"
            fi
            if [ -z "$user_email" ]; then
                local safe_email="${id}+${login}@users.noreply.github.com"
                "$GIT_BIN" config --global user.email "$safe_email"
                echo "✓ Set user.email = '$safe_email' (privacy no-reply)"
            fi
        fi
    fi

    echo "✓ Setup complete! Git and GitHub CLI are now ready for seamless repository access."
}

cmd_login() {
    if [ -z "$GH_BIN" ]; then
        echo "Error: GitHub CLI (gh) is not installed. Run '$0 install' first." >&2
        return 1
    fi

    if [ -t 0 ]; then
        echo "Starting GitHub browser sign-in..."
        "$GH_BIN" auth login --hostname github.com --git-protocol https --web
        "$GH_BIN" auth setup-git
        echo "✓ Successfully authenticated and configured Git."
    else
        echo "Interactive sign-in requires a terminal. Please run the following command in your terminal:"
        echo "  $GH_BIN auth login --hostname github.com --git-protocol https --web && $GH_BIN auth setup-git"
        return 2
    fi
}

cmd_install() {
    echo "=== GitHub CLI (gh) Installation Instructions ==="
    if [[ "$(uname -s)" == "Darwin" ]]; then
        if command -v brew >/dev/null 2>&1; then
            echo "Run: brew install gh"
        else
            echo "Download official installer from: https://github.com/cli/cli/releases/latest"
            echo "Or install via Homebrew."
        fi
    elif [[ "$(uname -s)" == "Linux" ]]; then
        echo "Debian/Ubuntu: sudo apt install gh"
        echo "Fedora/RHEL:   sudo dnf install gh"
        echo "Arch Linux:    sudo pacman -S github-cli"
        echo "Tarball:       https://github.com/cli/cli/releases/latest"
    else
        echo "Windows: winget install --id GitHub.cli -e"
    fi
    echo "================================================="
}

cmd_clone() {
    local slug="${1:-}"
    local dest="${2:-}"
    if [ -z "$slug" ]; then
        echo "Usage: $0 clone <owner/repo|url> [destination]" >&2
        return 1
    fi

    if [ -n "$GH_BIN" ]; then
        if [ -n "$dest" ]; then
            "$GH_BIN" repo clone "$slug" "$dest"
        else
            "$GH_BIN" repo clone "$slug"
        fi
    elif [ -n "$GIT_BIN" ]; then
        local url="$slug"
        if [[ ! "$url" =~ ^https?:// ]] && [[ ! "$url" =~ ^git@ ]]; then
            url="https://github.com/$slug.git"
        fi
        if [ -n "$dest" ]; then
            "$GIT_BIN" clone "$url" "$dest"
        else
            "$GIT_BIN" clone "$url"
        fi
    else
        echo "Error: Neither gh nor git is available." >&2
        return 1
    fi
}

cmd_list() {
    if [ -z "$GH_BIN" ]; then
        echo "Error: GitHub CLI is not installed." >&2
        return 1
    fi
    "$GH_BIN" repo list --limit 30 --json nameWithOwner,isPrivate,updatedAt,description --template '{{range .}}{{printf "%-35s" .nameWithOwner}}{{if .isPrivate}} [private]{{else}} [public]{{end}}{{printf "\n  %s\n" .description}}{{end}}'
}

case "${1:-status}" in
    status)  cmd_status "${2:-}" ;;
    setup)   cmd_setup ;;
    login)   cmd_login ;;
    install) cmd_install ;;
    clone)   cmd_clone "${2:-}" "${3:-}" ;;
    list)    cmd_list ;;
    help|--help|-h)
        echo "Usage: $0 <command>"
        echo "Commands:"
        echo "  status [--json]   Show Git and GitHub CLI connection status"
        echo "  setup             Configure Git credential helper and commit identity"
        echo "  login             Launch browser sign-in flow"
        echo "  install           Show GitHub CLI installation instructions"
        echo "  clone <repo> [d]  Clone a repository using authenticated credentials"
        echo "  list              List repositories for the authenticated user"
        ;;
    *)
        echo "Unknown command: $1. Run '$0 help' for usage." >&2
        exit 1
        ;;
esac
