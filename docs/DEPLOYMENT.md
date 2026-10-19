# Deployment

## Prerequisites

- Linux (the target has been a Proxmox LXC; a Raspberry Pi works the same way).
- Rust 1.75+ to build.
- `ffmpeg` on `PATH` for the visible MJPEG fallback. Without it the app still
  serves thermal; visible modes show “unavailable”.
- OpenSSL for the self-signed certificate (generated on first start).

## Build and run

```bash
cargo build --release

# simulated camera
./target/release/thermolink serve --demo --bind 0.0.0.0:8099

# Qianli camera
RUST_LOG=thermolink=info ./target/release/thermolink serve \
  --driver qianli --host 192.168.1.64 \
  --control-port 3001 --media-port 3000 \
  --data-dir /var/lib/thermolink --bind 0.0.0.0:8099
```

Ports: HTTP `:8099`, HTTPS `:8443` (same process). The HTTPS certificate and
key are written to the data directory on first start.

## systemd

```ini
[Unit]
Description=ThermoLink thermal camera relay
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=thermolink
Group=thermolink
WorkingDirectory=/opt/thermolink
ExecStart=/opt/thermolink/thermolink serve --host 192.168.1.64 --data-dir /var/lib/thermolink --bind 0.0.0.0:8099
Restart=always
RestartSec=3
Environment=RUST_LOG=thermolink=info

[Install]
WantedBy=multi-user.target
```

```bash
sudo useradd -r -s /usr/sbin/nologin thermolink
sudo install -d -o thermolink -g thermolink /opt/thermolink /var/lib/thermolink
sudo systemctl daemon-reload
sudo systemctl enable --now thermolink
```

The in-app **Restart relay** (and driver selection) re-executes the same binary
with the same arguments, so no supervisor is required for restarts. `systemd`
still restarts on crashes.

## Camera network

- Give the camera a static address or a DHCP reservation; set the relay's
  `--host` (or `camera.json` via the GUI) to it.
- Qianli uses TCP 3001 (control) and 3000 (media). Discovery probes the local
  `/24` on 3001 and verifies the vendor handshake.
- Open the UI, create the admin account, then use **Camera → Camera settings**
  to read/write the camera's IP block if needed.

## USB P3/P1 (untested)

The P3 driver is protocol-parsed but not wired to USB. When that lands:

- udev rule: `SUBSYSTEM=="usb", ATTR{idVendor}=="3474", ATTR{idProduct}=="45a2", MODE="0666"` (P3) or `45c2` (P1).
- Use a powered hub if the camera browns out; USB 2.0 bandwidth is sufficient.
- The driver advertises thermal-only capabilities, so the UI hides all video
  modes automatically.

## Backup and updates

Back up `--data-dir` (`auth.json`, `sessions.json`, `regions.json`,
`references.json`, `alarms.json`, `calibration.json`, snapshots, captures,
TLS key). It contains credentials and the TLS private key; protect it.

Updates: **Camera → Relay → Check for updates / Install / Restart**, or
`POST /api/update` and `POST /api/restart`. Installing replaces the binary;
the running build keeps serving until restarted. Sessions persist across the
restart.

## Firewall

Allow `:8099` and `:8443` only from trusted networks. Do not expose the relay
directly to the internet; put a trusted TLS reverse proxy or VPN in front. See
[Security](SECURITY.md).
