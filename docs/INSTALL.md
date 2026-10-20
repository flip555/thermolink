# Installing and running ThermoLink

Prebuilt artifacts are published on the
[GitHub releases page](https://github.com/flip555/thermolink/releases). Each
release has:

- Linux CLI tarballs (`x86_64` and `aarch64`)
- Linux desktop packages (`.deb`, `.AppImage`)
- macOS desktop app (`.zip`)
- Windows desktop installer (`.exe` / `.msi`)
- `SHA256SUMS-*.txt` for the artifacts

The desktop app is self-contained: it bundles the LGPL FFmpeg binary used for
the visible stream. The CLI needs `ffmpeg` on `PATH` for visible video; thermal
works without it.

## Visible video path

The browser UI prefers WebCodecs when the webview supports H.264. Otherwise it
uses MJPEG from the bundled/server FFmpeg:

- Desktop: WebCodecs → bundled FFmpeg → MJPEG. No system FFmpeg is required.
- CLI: WebCodecs → `THERMOLINK_FFMPEG` / `PATH` FFmpeg → MJPEG.

Use `?video=webcodecs`, `?video=mjpeg` or `?video=auto` to override; the older
`?mjpeg=1` still forces MJPEG. If WebCodecs is forced but unsupported, the UI
falls back to MJPEG with a console reason.

## Current signing status

These builds are **not code-signed or notarized yet**. macOS needs an Apple
Developer Program membership and Developer ID/notarization; Windows needs an
OV/EV code-signing certificate. Until then:

| Platform | What you will see | What to do |
|---|---|---|
| macOS | Gatekeeper says the app is from an unidentified developer | Right-click the app → **Open**, or **System Settings → Privacy & Security → Open Anyway** |
| macOS | No notarization ticket | The above applies; this is expected for the current unsigned build |
| Windows | SmartScreen warns about an unrecognized app | **More info → Run anyway** |
| Linux | No OS-level signature | Verify the SHA-256 checksum below; make the AppImage executable |

Only run artifacts downloaded from the official GitHub release and verified
against its checksum file.

## Verify checksums

Each release includes a `SHA256SUMS-<target>.txt`.

Linux:

```bash
sha256sum -c SHA256SUMS-x86_64-unknown-linux-gnu.txt
```

macOS:

```bash
shasum -a 256 -c SHA256SUMS-aarch64-apple-darwin.txt
```

Windows PowerShell:

```powershell
Get-FileHash .\ThermoLink_0.4.0_x64-setup.exe -Algorithm SHA256
```

Compare the output with the matching line in the release checksum file.

## Desktop app

### macOS

1. Download `ThermoLink-macos-aarch64-apple-darwin.zip` and unzip it.
2. Move `ThermoLink.app` to Applications if you want.
3. First launch: right-click → **Open**. If macOS still blocks it, use
   **System Settings → Privacy & Security → Open Anyway**.
4. Allow ThermoLink under **Privacy & Security → Local Network**, then relaunch.
5. Create the admin account on first run.

The current app is ad-hoc signed, not notarized. A Developer ID + notarized
build will remove the Gatekeeper warning later.

### Windows

1. Download `ThermoLink_0.4.0_x64-setup.exe` (recommended) or the `.msi`.
2. Run it. If SmartScreen appears: **More info → Run anyway**.
3. Launch ThermoLink and create the admin account.
4. In **Camera → Driver**, choose `qianli` and enter the camera IP
   (for example `172.20.1.85`).

The desktop bundle includes `ffmpeg.exe`; no separate FFmpeg install is needed.

### Linux

Debian/Ubuntu:

```bash
sudo apt install ./ThermoLink_0.4.0_amd64.deb
thermolink-desktop
```

AppImage:

```bash
chmod +x ThermoLink_0.4.0_amd64.AppImage
./ThermoLink_0.4.0_amd64.AppImage
```

If FUSE is unavailable:

```bash
./ThermoLink_0.4.0_amd64.AppImage --appimage-extract-and-run
```

The desktop bundle includes the LGPL FFmpeg binary. The `.deb` installs the
WebKitGTK runtime dependencies; the AppImage still needs the usual GTK/WebKit
libraries on the target system.

## CLI (headless relay)

Download the matching tarball, then:

```bash
tar -xzf thermolink-x86_64-unknown-linux-gnu.tar.gz
cd thermolink-x86_64-unknown-linux-gnu
./thermolink --help
```

Simulator:

```bash
./thermolink serve --demo --bind 0.0.0.0:8099
```

Qianli:

```bash
RUST_LOG=thermolink=info ./thermolink serve \
  --driver qianli --host 192.168.1.64 \
  --control-port 3001 --media-port 3000 \
  --data-dir /var/lib/thermolink --bind 0.0.0.0:8099
```

Open `http://<host>:8099` or `https://<host>:8443` (self-signed certificate;
accept the warning). The first visitor creates the admin account.

For the visible stream the CLI needs `ffmpeg` on `PATH`; the desktop app bundles
it.

## UVC thermal cameras

UVC support is experimental. In **Camera → Driver**:

1. Select `uvc_radiometric`.
2. Pick the closest profile; the dropdown shows its validation state.
3. Click **Use driver**.
4. If it works or fails, click **Report hardware / capture** and include the
   VID:PID, profile and a raw frame.

The profiles are not hardware-validated yet. `uvc_generic` is a best-effort
fallback with temperature measurement disabled. `0bda:5830` is shared by
several models, so auto-detect reports the ambiguity instead of guessing.

## Data directories

Desktop app data:

- macOS: `~/Library/Application Support/dev.flipa.thermolink`
- Windows: `%APPDATA%\dev.flipa.thermolink`
- Linux: `~/.local/share/dev.flipa.thermolink`

CLI default: `./thermolink-data`, or the directory passed with `--data-dir`.

Back up that directory to preserve accounts, camera selection, calibration,
regions, references, alarms and snapshots. It also contains the TLS private key.
