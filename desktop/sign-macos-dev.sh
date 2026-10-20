#!/usr/bin/env bash
set -euo pipefail

# Tauri's debug bundle is linker-signed; macOS does not bind its Info.plist, so
# Local Network permission can be ignored. Re-sign the whole bundle ad-hoc after
# every debug build. For distribution use a Developer ID identity instead.
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP="${1:-$ROOT/target/debug/bundle/macos/ThermoLink.app}"

if [[ ! -d "$APP" ]]; then
  echo "app bundle not found: $APP" >&2
  exit 1
fi

codesign --force --deep --sign - "$APP"
codesign -dv --verbose=4 "$APP" 2>&1 | grep -E "Identifier=|Info.plist=|Sealed Resources=|Signature="
