# ThermoLink

**One browser UI for thermal cameras — live thermal and visible video, measurement, alarms and recording.**

![Thermal view](docs/screenshots/thermal.png)

ThermoLink is a Rust relay with a driver boundary: the web server never learns a
camera’s resolution, packet layout or network dialect. Drivers expose a
`CameraSpec`; the UI adapts to what the camera can actually do.

## Download

| Platform | Install | First run |
|---|---|---|
| **Windows** | [Installer (.exe)](https://github.com/flip555/thermolink/releases/download/v0.4.1/ThermoLink_0.4.1_x64-setup.exe) · [MSI](https://github.com/flip555/thermolink/releases/download/v0.4.1/ThermoLink_0.4.1_x64_en-US.msi) | **Unsigned** — SmartScreen: **More info → Run anyway** |
| **macOS** | [Apple Silicon app (.zip)](https://github.com/flip555/thermolink/releases/download/v0.4.1/ThermoLink-macos-aarch64-apple-darwin.zip) | **Unsigned/not notarized** — right-click **Open**, or **System Settings → Privacy & Security → Open Anyway** |
| **Linux** | [AppImage](https://github.com/flip555/thermolink/releases/download/v0.4.1/ThermoLink_0.4.1_amd64.AppImage) · [.deb](https://github.com/flip555/thermolink/releases/download/v0.4.1/ThermoLink_0.4.1_amd64.deb) | `chmod +x` the AppImage, or `apt install ./ThermoLink_*.deb` |
| **Server / CLI** | [Linux tarballs](https://github.com/flip555/thermolink/releases/download/v0.4.1/thermolink-x86_64-unknown-linux-gnu.tar.gz) | See [server installation](https://github.com/flip555/thermolink/wiki/Installation) |

> **Unsigned builds:** macOS notarization and Windows code signing are pending
> (Apple Developer Program / OV certificate). Verify the release
> `SHA256SUMS-*.txt` before running. Full per-platform steps:
> [Installing and running](docs/INSTALL.md).

## Features

- **Thermal:** palettes, clip/span, isotherm bands and profiles, hot/cold peaks,
  history, point/box/polygon measurement, ΔT references and A/B comparison.
- **Visible:** WebCodecs H.264 in the browser with an MJPEG fallback; Visible,
  Wipe, PiP and Fusion modes.
- **Operations:** snapshots, pre-record, clip ZIPs, alarms, CSV export,
  re-analysis, health and restart.
- **Access:** admin/guest roles, persisted sessions, HTTPS and nine UI languages.
- **Desktop:** Tauri shell for Windows, macOS and Linux; same UI, localhost
  server and bundled FFmpeg.

## Cameras

| Driver | Transport | Status |
|---|---|---|
| `qianli` | TCP 3001/3000 | **Validated** — full handshake, settings, reboot, discovery |
| `p3` / `p1` | USB | Experimental — `nusb` transport and parser; hardware validation pending |
| `uvc_radiometric` | USB/UVC (Linux) | Experimental — TC001/P2 Pro/TC002C/HT301/T2S+/Mini2 + generic fallback |
| `mock` | In-process | Simulator for development and tests |

The exact profile list and validation flags live in
[`src/camera/`](src/camera/) and [DRIVERS.md](docs/DRIVERS.md). The UI only shows
controls a driver advertises.

## Run

**Desktop:** install from the table above, create the admin account, then choose
**Camera → Driver**.

**Server / CLI:** see the [installation wiki](https://github.com/flip555/thermolink/wiki/Installation)
for setup, systemd, TLS, USB udev rules and camera networking.

Development simulator:

```bash
cargo run --release -- serve --demo --bind 0.0.0.0:8099
```

Open `http://localhost:8099`.

## Documentation

- [Wiki](https://github.com/flip555/thermolink/wiki) — installation, setup, drivers, UI
- [Installing and running](docs/INSTALL.md) — desktop and CLI, checksums, unsigned warnings
- [Drivers](docs/DRIVERS.md) — capabilities and profile status
- [HTTP API](docs/API.md)
- [Architecture](docs/ARCHITECTURE.md)
- [Security](docs/SECURITY.md) — self-signed HTTPS and exposure guidance
- [Troubleshooting](docs/TROUBLESHOOTING.md)

## Screenshots

| Thermal | Fusion | Wipe |
|---|---|---|
| ![Thermal](docs/screenshots/thermal.png) | ![Fusion](docs/screenshots/fusion.png) | ![Wipe](docs/screenshots/wipe.png) |

More views: [wiki screenshots](https://github.com/flip555/thermolink/wiki/Screenshots).

## License

MIT. See [LICENSE](LICENSE).
