# ThermoLink Wiki

One web UI for thermal cameras, with the camera behind a driver boundary.

ThermoLink is a Rust relay: it captures thermal and visible frames, renders
palettes/measurements, records, raises alarms, and serves a browser UI. Drivers
hide vendor protocols; the server never imports a concrete driver.

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
- Drivers: Qianli over TCP, simulated source, P3/P1 USB (parser only, untested).
  Capabilities drive which UI controls appear; drivers hot-swap at runtime.
- Operations: snapshots, pre-record, per-clip ZIPs, alarms/events, CSV exports,
  self-update/restart.
- Access: admin/guest roles, persisted sessions, Bearer/cookie/query tokens,
  HTTPS, nine UI languages.

## Wiki pages

- [Installation](Installation)
- [Configuration](Configuration)
- [Drivers](Drivers)
- [Architecture](Architecture)
- [HTTP API](API)
- [Security](Security)
- [Troubleshooting](Troubleshooting)

Full repository docs live in
[`docs/`](https://github.com/flip555/thermolink/tree/main/docs).
