# ThermoLink desktop

Tauri 2 shell around the same web UI and the same in-process server.

## How it works

`thermolink-desktop` starts `thermolink::runtime` on `127.0.0.1:0`, then opens a
Tauri webview at the bound address. The headless `thermolink serve` command uses
the same runtime; only the listener/TLS defaults and the window differ.

## Build

Ubuntu 24.04 build dependencies:

```bash
sudo apt-get install -y libwebkit2gtk-4.1-dev libgtk-3-dev libsoup-3.0-dev \
  libjavascriptcoregtk-4.1-dev librsvg2-dev patchelf
```

Run:

```bash
cargo run -p thermolink-desktop
```

Build bundles:

```bash
cd desktop
npm install
npm run tauri build
```

The bundle uses the repository's `web/` directory as `frontendDist`; the window
loads the in-process server, so the UI is not duplicated.

The release workflow builds unsigned bundles on Linux, macOS and Windows and
uploads them as workflow artifacts. Attaching them to the GitHub release and
code signing are follow-ups.

### macOS Local Network permission

Tauri is configured with `bundle.macOS.signingIdentity: "-"`, so every build is
ad-hoc signed automatically and `Info.plist` is bound. That lets macOS apply the
Local Network permission without a manual `codesign` step.

`desktop/sign-macos-dev.sh` remains as a fallback for a bundle built before that
setting or by another tool:

```bash
./desktop/sign-macos-dev.sh
```

Then allow ThermoLink under **System Settings → Privacy & Security → Local
Network**. End users do **not** run this script. A distributed build should use
a Developer ID identity and notarization instead of ad-hoc signing.

### Static ffmpeg

`thermolink` honors `THERMOLINK_FFMPEG`, and the desktop app looks for
`Contents/Resources/ffmpeg` first. Copy a licensed static ffmpeg binary there
for a self-contained app.

The `ffmpeg-static` npm test binary used during development is built with
`--enable-gpl --enable-nonfree`; do not redistribute it. Use an LGPL static
build (or a properly licensed binary) for a public release.

## Current scope

- Localhost only. Network sharing is a later opt-in feature.
- Default driver is `uvc_radiometric`; `camera.json` in the platform app-data
  directory can select another driver/profile.
- No in-app updater yet. The CLI updater remains for `thermolink serve`.
- Linux is the reference platform. macOS/Windows packaging is not validated.
- The bundled icon is a placeholder and should be replaced before a public
  release.
