# Installation

## Download

Prebuilt artifacts are on the
[GitHub releases page](https://github.com/flip555/thermolink/releases):

- Linux CLI tarballs (`x86_64`, `aarch64`)
- Linux desktop `.deb` / `.AppImage`
- macOS desktop `.app.zip`
- Windows desktop `.exe` / `.msi`
- `SHA256SUMS-*.txt` checksums

The desktop app bundles the LGPL FFmpeg used for the visible stream. The CLI
needs `ffmpeg` on `PATH` for visible video; thermal works without it.

## Signing status

The builds are currently **unsigned**.

| Platform | What you will see | What to do |
|---|---|---|
| macOS | “Unidentified developer” / Gatekeeper block | Right-click **Open**, or **System Settings → Privacy & Security → Open Anyway** |
| Windows | SmartScreen “unrecognized app” | **More info → Run anyway** |
| Linux | No OS signature | Verify the checksum; `chmod +x` the AppImage |

macOS notarization requires a paid Apple Developer Program membership; Windows
requires an OV/EV code-signing certificate. Neither is in place yet.

Only run artifacts from the official release and verify the checksum first.

## Verify checksums

```bash
# Linux
sha256sum -c SHA256SUMS-x86_64-unknown-linux-gnu.txt

# macOS
shasum -a 256 -c SHA256SUMS-aarch64-apple-darwin.txt
```

Windows PowerShell:

```powershell
Get-FileHash .\ThermoLink_0.4.0_x64-setup.exe -Algorithm SHA256
```

Compare with the release checksum file.

## Desktop

### macOS

1. Unzip `ThermoLink-macos-aarch64-apple-darwin.zip`.
2. First launch: right-click **ThermoLink.app → Open**; if needed use
   **System Settings → Privacy & Security → Open Anyway**.
3. Allow ThermoLink in **Privacy & Security → Local Network**, then relaunch.
4. Create the admin account.

The app is ad-hoc signed, not notarized. The Local Network prompt is expected.

### Windows

1. Run `ThermoLink_0.4.0_x64-setup.exe` (recommended) or the `.msi`.
2. SmartScreen: **More info → Run anyway**.
3. Create the admin account.
4. **Camera → Driver → qianli**, then enter the camera IP.

`ffmpeg.exe` is bundled; no separate install is needed.

### Linux

```bash
# Debian/Ubuntu
sudo apt install ./ThermoLink_0.4.0_amd64.deb
thermolink-desktop

# AppImage
chmod +x ThermoLink_0.4.0_amd64.AppImage
./ThermoLink_0.4.0_amd64.AppImage
```

If FUSE is unavailable:

```bash
./ThermoLink_0.4.0_amd64.AppImage --appimage-extract-and-run
```

## CLI

```bash
tar -xzf thermolink-x86_64-unknown-linux-gnu.tar.gz
cd thermolink-x86_64-unknown-linux-gnu
./thermolink --help

# simulator
./thermolink serve --demo --bind 0.0.0.0:8099

# Qianli
RUST_LOG=thermolink=info ./thermolink serve \
  --driver qianli --host 192.168.1.64 \
  --data-dir /var/lib/thermolink --bind 0.0.0.0:8099
```

Open `http://<host>:8099` or `https://<host>:8443` (self-signed certificate).
The first visitor creates the admin account.

## UVC cameras

UVC support is experimental. In **Camera → Driver**:

1. Select `uvc_radiometric`.
2. Pick the closest profile; the dropdown shows its validation state.
3. Click **Use driver**.
4. If it works or fails, click **Report hardware / capture** with the VID:PID,
   profile and a raw frame.

`uvc_generic` is a best-effort fallback with temperature measurement disabled.
`0bda:5830` is shared by several models; auto-detect reports ambiguity rather
than guessing.

## Data directories

- macOS: `~/Library/Application Support/dev.flipa.thermolink`
- Windows: `%APPDATA%\dev.flipa.thermolink`
- Linux: `~/.local/share/dev.flipa.thermolink`
- CLI default: `./thermolink-data` or `--data-dir`

Back up this directory to preserve accounts, calibration, regions, references,
alarms and snapshots. It also contains the TLS private key.
