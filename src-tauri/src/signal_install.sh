#!/bin/sh
# One-click signal-cli install for Consilium, with no Homebrew and no admin password.
# Downloads a pinned Java runtime (Eclipse Temurin) and a pinned signal-cli release over HTTPS,
# checks both against fixed SHA-256 sums, unpacks them into ~/.local/share/consilium-signal and
# puts a small `signal-cli` launcher in ~/.local/bin. Progress goes to $ROOT/install.log;
# $ROOT/installing exists while it runs and $ROOT/failed holds the reason if it stops.
set -eu
ROOT="$HOME/.local/share/consilium-signal"
BIN="$HOME/.local/bin"
SIGNAL_VER=0.14.10
SIGNAL_URL="https://github.com/AsamK/signal-cli/releases/download/v$SIGNAL_VER/signal-cli-$SIGNAL_VER.tar.gz"
SIGNAL_SHA=706a57d9faae7525cf732936efdabf210146e3c83f9e8ae7e4e62f5946121c33
case "$(uname -m)" in
  x86_64)
    JRE_URL="https://github.com/adoptium/temurin25-binaries/releases/download/jdk-25.0.5%2B8/OpenJDK25U-jre_x64_mac_hotspot_25.0.5_8.tar.gz"
    JRE_SHA=806902c796c670969b7b27df73b8f0fe0924a61bd65083aa53064115e49cab2d ;;
  arm64)
    JRE_URL="https://github.com/adoptium/temurin25-binaries/releases/download/jdk-25.0.5%2B8/OpenJDK25U-jre_aarch64_mac_hotspot_25.0.5_8.tar.gz"
    JRE_SHA=72ebd76b502c849e88dea39afae51baf5f82f2b5b1b6389c28ea707b20451bce ;;
  *) echo "unsupported processor $(uname -m)" >&2; exit 1 ;;
esac

mkdir -p "$ROOT" "$BIN"
rm -f "$ROOT/failed"
: > "$ROOT/installing"
exec >"$ROOT/install.log" 2>&1
WORK="$(mktemp -d "$ROOT/tmp.XXXXXX")"
fail() { echo "$1" > "$ROOT/failed"; rm -rf "$WORK" "$ROOT/installing"; exit 1; }
trap 'fail "The install stopped unexpectedly. See $ROOT/install.log"' EXIT

fetch() { # url sha256 dest-dir label
  echo "Downloading $4…"
  curl -fsSL --retry 3 --connect-timeout 20 -o "$WORK/pkg.tgz" "$1" || fail "Couldn't download $4. Check the internet connection and try again."
  got="$(shasum -a 256 "$WORK/pkg.tgz" | cut -d' ' -f1)"
  [ "$got" = "$2" ] || fail "The $4 download didn't match its expected checksum, so it wasn't installed."
  rm -rf "$WORK/x" && mkdir "$WORK/x"
  tar -xzf "$WORK/pkg.tgz" -C "$WORK/x" || fail "Couldn't unpack $4."
  top="$(find "$WORK/x" -mindepth 1 -maxdepth 1 -type d | head -n 1)"
  [ -n "$top" ] || fail "The $4 download was empty."
  rm -rf "$3" && mv "$top" "$3"
  rm -f "$WORK/pkg.tgz"
}

[ -x "$ROOT/jre/Contents/Home/bin/java" ] || fetch "$JRE_URL" "$JRE_SHA" "$ROOT/jre" "Java"
[ -x "$ROOT/signal-cli-$SIGNAL_VER/bin/signal-cli" ] || fetch "$SIGNAL_URL" "$SIGNAL_SHA" "$ROOT/signal-cli-$SIGNAL_VER" "signal-cli $SIGNAL_VER"

cat > "$BIN/signal-cli" <<EOF
#!/bin/sh
JAVA_HOME="$ROOT/jre/Contents/Home" exec "$ROOT/signal-cli-$SIGNAL_VER/bin/signal-cli" "\$@"
EOF
chmod 755 "$BIN/signal-cli"
"$BIN/signal-cli" --version || fail "signal-cli was installed but didn't start."
trap - EXIT
rm -rf "$WORK" "$ROOT/installing"
echo "Installed."
