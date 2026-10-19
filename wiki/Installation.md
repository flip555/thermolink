# Installation

## Requirements

- Linux (Raspberry Pi or an LXC/VM).
- Rust 1.75+ to build.
- `ffmpeg` on `PATH` for the visible MJPEG fallback (optional).
- `openssl` for the self-signed certificate (generated on first start).

## Build and run

```bash
cargo build --release

# simulator — no camera
./target/release/thermolink serve --demo --bind 0.0.0.0:8099

# Qianli camera
RUST_LOG=thermolink=info ./target/release/thermolink serve \
  --driver qianli --host 192.168.1.64 \
  --data-dir /var/lib/thermolink --bind 0.0.0.0:8099
```

Open `http://<host>:8099` or `https://<host>:8443` (self-signed; accept the
warning). The first visitor creates the admin account.

Ports: HTTP `8099`, HTTPS `8443`. The certificate/key land in the data
directory.

## systemd

```ini
[Unit]
Description=ThermoLink
After=network-online.target

[Service]
User=thermolink
WorkingDirectory=/opt/thermolink
ExecStart=/opt/thermolink/thermolink serve --host 192.168.1.64 --data-dir /var/lib/thermolink --bind 0.0.0.0:8099
Restart=always
Environment=RUST_LOG=thermolink=info

[Install]
WantedBy=multi-user.target
```

The in-app restart/driver switch re-executes the same binary, so no supervisor
is required for those; systemd still restarts on crashes.

Full detail: [docs/DEPLOYMENT.md](https://github.com/flip555/thermolink/blob/main/docs/DEPLOYMENT.md).
