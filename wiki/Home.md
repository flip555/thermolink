# ThermoLink Wiki

One web UI for thermal cameras, with the camera behind a driver boundary.

ThermoLink is a Rust relay: it captures thermal and visible frames, renders
palettes/measurements, records, raises alarms, and serves a browser UI. Drivers
hide vendor protocols; the server never imports a concrete driver.

![Thermal view](images/ui-thermal.png)

See the [screenshot gallery](Screenshots) for palettes, wipe, measurement,
alarms, capture, re-analysis and ΔT.

## Download and run

Desktop apps and the Linux CLI are published on the
[GitHub releases page](https://github.com/flip555/thermolink/releases), with
SHA-256 checksums.

- **macOS:** currently unsigned/not notarized. First launch: right-click
  **Open**, or **System Settings → Privacy & Security → Open Anyway**.
- **Windows:** currently unsigned. SmartScreen: **More info → Run anyway**.
- **Linux:** install the `.deb`, or `chmod +x` the AppImage and run it.

See [Installation](Installation) for per-platform steps, checksum verification
and the bundled-FFmpeg details.

## Quick start

```bash
cargo build --release
./target/release/thermolink serve --demo --bind 0.0.0.0:8099
```

Open `http://<host>:8099` (or `https://<host>:8443`, self-signed) and create the
first admin account.

## What it does

- Thermal: palettes, clip/span, isotherm bands and profiles, hot/cold peaks,
  point/box/polygon measurement, ΔT references, A/B comparison, history.
- Visible: H.264 WebCodecs over HTTPS with MJPEG fallback; Visible, Wipe, PiP,
  Fusion.
- Drivers: Qianli over TCP, simulated source, P3/P1 USB (nusb transport; hardware validation pending).
  Capabilities drive which UI controls appear; drivers hot-swap at runtime.
- UVC: the radiometric family includes TC001/P2 Pro/TC002C/HT301/T2S+/Mini2
  and a generic fallback. These profiles are experimental until a user reports
  a working capture.
- Operations: snapshots, pre-record, per-clip ZIPs, alarms/events, CSV exports,
  self-update/restart.
- Access: admin/guest roles, persisted sessions, Bearer/cookie/query tokens,
  HTTPS, nine UI languages.

## Wiki pages

- [Installation](Installation)
- [Drivers](Drivers)
- [Configuration](Configuration)
- [Using the view](Using-the-view)
- [Architecture](Architecture)
- [HTTP API](API)
- [Security](Security)
- [Troubleshooting](Troubleshooting)

Full repository docs live in
[`docs/`](https://github.com/flip555/thermolink/tree/main/docs).
- [Screenshots](Screenshots)
- [Alarms and recording](Alarms-and-recording)
