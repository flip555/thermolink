# Bundled desktop resources

Place the platform LGPL FFmpeg binary here before `tauri build`:

- Linux/macOS: `ffmpeg`
- Windows: `ffmpeg.exe`

The desktop packaging scripts populate this directory, and CI downloads
prebuilt LGPL builds for Linux and Windows. The binaries are git-ignored.

`COPYING.LGPLv2.1` is committed for license compliance; `FFMPEG-LICENSE.txt`
is copied from the downloaded build when present.

The app checks this directory (and the resource root) for the ffmpeg binary and
sets `THERMOLINK_FFMPEG` before starting the runtime.
