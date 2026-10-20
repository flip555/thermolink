#!/usr/bin/env bash
set -euo pipefail

# Build and package the macOS app with the LGPL FFmpeg bundled into
# Contents/Resources/ffmpeg. This uses ad-hoc signing for local testing.
# For distribution use a Developer ID identity and notarization instead.

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "package-macos-dev: macOS only" >&2
  exit 1
fi

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
MODE="${1:-debug}"
if [[ "$MODE" != "debug" && "$MODE" != "release" ]]; then
  echo "usage: $0 [debug|release]" >&2
  exit 1
fi

FFMPEG="$ROOT/desktop/vendor/ffmpeg/ffmpeg"
if [[ ! -x "$FFMPEG" ]]; then
  echo "missing $FFMPEG" >&2
  echo "run ./desktop/build-lgpl-ffmpeg.sh first" >&2
  exit 1
fi

export PATH="$HOME/.local/bin:$HOME/.cargo/bin:$PATH"
cd "$ROOT/desktop"
npm install --no-audit --no-fund

if [[ "$MODE" == "release" ]]; then
  npm run tauri build -- --bundles app
  APP="$ROOT/target/release/bundle/macos/ThermoLink.app"
else
  npm run tauri build -- --debug --bundles app
  APP="$ROOT/target/debug/bundle/macos/ThermoLink.app"
fi

if [[ ! -d "$APP" ]]; then
  echo "app bundle not found: $APP" >&2
  exit 1
fi

mkdir -p "$APP/Contents/Resources"
cp -f "$FFMPEG" "$APP/Contents/Resources/ffmpeg"
chmod +x "$APP/Contents/Resources/ffmpeg"
cp -f "$ROOT/desktop/vendor/ffmpeg/COPYING.LGPLv2.1" \
  "$APP/Contents/Resources/COPYING.LGPLv2.1"

# Copying resources invalidates Tauri's signature; re-sign the whole bundle.
codesign --force --deep --sign - "$APP"
codesign -dv --verbose=4 "$APP" 2>&1 | \
  grep -E "Identifier=|Info.plist=|Sealed Resources=|Signature="

echo
echo "Packaged: $APP"
echo "First launch: allow ThermoLink under Privacy & Security → Local Network."
