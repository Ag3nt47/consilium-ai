#!/usr/bin/env bash
# Installs the latest Consilium release on macOS or Linux.
#
#   curl -fsSL https://raw.githubusercontent.com/Ag3nt47/consilium-ai/main/install.sh | bash
#   ... | bash -s -- --dock        (macOS: also add Consilium to the Dock)
#
# It downloads only from this repository's GitHub releases, installs for the current user where
# possible, and never asks for or stores any credentials.
set -euo pipefail

REPO="Ag3nt47/consilium-ai"
DOCK=0
for arg in "$@"; do
  case "$arg" in
    --dock) DOCK=1 ;;
    -h|--help) sed -n '2,8p' "$0"; exit 0 ;;
  esac
done

say() { printf '\033[1;33m==>\033[0m %s\n' "$*"; }
die() { printf '\033[1;31mError:\033[0m %s\n' "$*" >&2; exit 1; }

command -v curl >/dev/null || die "curl is required."
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

say "Finding the latest Consilium release…"
API="https://api.github.com/repos/$REPO/releases/latest"
JSON="$(curl -fsSL -H 'Accept: application/vnd.github+json' "$API")" || die "couldn't reach GitHub, or there is no release yet."
TAG="$(printf '%s' "$JSON" | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -1)"
URLS="$(printf '%s' "$JSON" | sed -n 's/.*"browser_download_url": *"\([^"]*\)".*/\1/p')"
[ -n "$TAG" ] || die "couldn't read the release information."

pick() { printf '%s\n' "$URLS" | grep -E "$1" | grep "^https://github.com/$REPO/releases/download/" | head -1; }

case "$(uname -s)" in
  Darwin)
    URL="$(pick '\.dmg$')"
    [ -n "$URL" ] || die "this release has no macOS installer."
    say "Downloading Consilium $TAG for macOS…"
    curl -fL --progress-bar -o "$TMP/Consilium.dmg" "$URL"
    MNT="$(hdiutil attach -nobrowse -readonly "$TMP/Consilium.dmg" | awk -F'\t' '/\/Volumes\//{print $NF}' | tail -1)"
    [ -d "$MNT/Consilium.app" ] || die "the disk image doesn't contain Consilium.app."
    DEST="/Applications"
    [ -w "$DEST" ] || DEST="$HOME/Applications"
    mkdir -p "$DEST"
    rm -rf "$DEST/Consilium.app"
    cp -R "$MNT/Consilium.app" "$DEST/"
    hdiutil detach -quiet "$MNT" || true
    # The app isn't notarized yet; remove the download quarantine for this app only.
    xattr -dr com.apple.quarantine "$DEST/Consilium.app" 2>/dev/null || true
    say "Installed to $DEST/Consilium.app"
    "$DEST/Consilium.app/Contents/MacOS/consilium" install-cli || say "Couldn't set up the consilium terminal command; use Settings → Terminal in the app."
    if [ "$DOCK" = 1 ]; then
      if defaults read com.apple.dock persistent-apps 2>/dev/null | grep -q "Consilium.app"; then
        say "Consilium is already in the Dock."
      else
        defaults write com.apple.dock persistent-apps -array-add \
          "<dict><key>tile-data</key><dict><key>file-data</key><dict><key>_CFURLString</key><string>file://$DEST/Consilium.app/</string><key>_CFURLStringType</key><integer>15</integer></dict></dict></dict>"
        killall Dock
        say "Added to the Dock."
      fi
    fi
    open "$DEST/Consilium.app"
    ;;
  Linux)
    ARCH="$(uname -m)"
    case "$ARCH" in x86_64|amd64) A='(amd64|x86_64)' ;; aarch64|arm64) A='(arm64|aarch64)' ;; *) die "unsupported CPU: $ARCH" ;; esac
    URL="$(pick "$A.*\\.AppImage$")"
    [ -n "$URL" ] || die "this release has no AppImage for $ARCH."
    say "Downloading Consilium $TAG for Linux ($ARCH)…"
    mkdir -p "$HOME/.local/bin" "$HOME/.local/share/applications" "$HOME/.local/share/icons/hicolor/256x256/apps"
    curl -fL --progress-bar -o "$HOME/.local/bin/consilium.AppImage" "$URL"
    chmod 755 "$HOME/.local/bin/consilium.AppImage"
    curl -fsSL -o "$HOME/.local/share/icons/hicolor/256x256/apps/consilium.png" \
      "https://raw.githubusercontent.com/$REPO/$TAG/src-tauri/icons/128x128@2x.png" || true
    cat > "$HOME/.local/share/applications/consilium.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Consilium
Comment=A council of minds
Exec=$HOME/.local/bin/consilium.AppImage
Icon=consilium
Categories=Office;Utility;
Terminal=false
EOF
    say "Installed. Find Consilium in your applications menu, or run ~/.local/bin/consilium.AppImage"
    APPIMAGE="$HOME/.local/bin/consilium.AppImage" "$HOME/.local/bin/consilium.AppImage" install-cli || say "Couldn't set up the consilium terminal command; use Settings → Terminal in the app."
    say "To uninstall: rm ~/.local/bin/consilium.AppImage ~/.local/share/applications/consilium.desktop"
    ;;
  *) die "unsupported system; on Windows use install.ps1." ;;
esac

say "Next: open Consilium and press \"Set up AIs\" if ChatGPT or Claude isn't ready yet."
