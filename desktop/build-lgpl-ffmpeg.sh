#!/usr/bin/env bash
set -euo pipefail

# Build a self-contained LGPL FFmpeg for the desktop app.
# The binary is written to desktop/vendor/ffmpeg/ and is intentionally not
# committed. Re-run this when upgrading FFmpeg or building on a new platform.

FFMPEG_VERSION="${FFMPEG_VERSION:-7.1.1}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VENDOR="$ROOT/desktop/vendor"
SRC="$VENDOR/src/ffmpeg-$FFMPEG_VERSION"
TARBALL="$VENDOR/src/ffmpeg-$FFMPEG_VERSION.tar.xz"
OUT="$VENDOR/ffmpeg"

case "$(uname -s)" in
  Darwin|Linux) ;;
  *) echo "build-lgpl-ffmpeg: macOS and Linux only" >&2; exit 1 ;;
esac

mkdir -p "$VENDOR/src" "$OUT"

if [[ ! -d "$SRC" ]]; then
  if [[ ! -f "$TARBALL" ]]; then
    curl -L --fail -o "$TARBALL" \
      "https://ffmpeg.org/releases/ffmpeg-$FFMPEG_VERSION.tar.xz"
  fi
  tar -xf "$TARBALL" -C "$VENDOR/src"
fi

cd "$SRC"
if [[ -f config.h ]]; then
  make distclean >/dev/null 2>&1 || true
fi

./configure \
  --prefix="$OUT" \
  --disable-gpl \
  --disable-nonfree \
  --disable-doc \
  --disable-debug \
  --disable-network \
  --disable-autodetect \
  --enable-static \
  --disable-shared

JOBS="$(sysctl -n hw.ncpu 2>/dev/null || nproc 2>/dev/null || echo 4)"
make -j"$JOBS"
make install

cp -f "$OUT/bin/ffmpeg" "$OUT/ffmpeg"
cp -f COPYING.LGPLv2.1 "$OUT/COPYING.LGPLv2.1"

RES="$ROOT/desktop/src-tauri/resources"
mkdir -p "$RES"
cp -f "$OUT/ffmpeg" "$RES/ffmpeg"
cp -f "$OUT/COPYING.LGPLv2.1" "$RES/COPYING.LGPLv2.1"

"$OUT/ffmpeg" -version | head -3
echo
echo "LGPL FFmpeg ready: $OUT/ffmpeg"
echo "License: $OUT/COPYING.LGPLv2.1 (LGPL v2.1+)"
echo "The binary is ignored by git; ship it with the license/source notice."
