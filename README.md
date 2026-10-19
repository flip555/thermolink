# ThermoLink

One web UI for thermal cameras, with the camera itself behind a driver boundary.

This is the Rust port of [`qianli-supercam`](https://github.com/flip555/qianli-supercam).
The Python project proved the protocol and the interface; ThermoLink keeps the
features and removes the coupling — the web server never learns a camera's
resolution, packet layout, or network dialect. It asks a driver for a
`CameraSpec` and consumes `CameraFrame`s.

**Status: milestone 2.** The driver boundary, Qianli handshake, thermal
extraction, palette/band renderer, HTTP server and a working live UI are in
place. A simulated driver lets you run the whole app with no hardware.

## Layout

```
thermolink/
├── Cargo.toml
├── src/
│   ├── main.rs              # CLI: drivers / probe / stream / config
│   ├── lib.rs
│   ├── config.rs            # driver-agnostic configuration
│   ├── camera/
│   │   ├── mod.rs           # CameraDriver, CameraSession, CameraSpec, frames
│   │   ├── registry.rs      # driver id -> implementation
│   │   ├── mock.rs          # simulated 160x120 source (demo/test)
│   │   └── qianli/
│   │       ├── mod.rs       # handshake + session read loop
│   │       ├── protocol.rs  # byte-exact packets (unit-tested vs capture)
│   │       └── thermal.rs   # 39448-byte wrapper -> 160x120 u16 frame
│   └── stream/
│       └── mod.rs           # FrameHub: latest-frame + broadcast fan-out
└── docs/
    └── PROTOCOL.md          # reverse-engineered Qianli protocol notes
```

The rule that keeps it modular: **nothing under `src/server/` (next milestone)
may import `camera::qianli`.** It imports `camera` only. Adding a camera family
means adding `src/camera/<family>/` and one arm in `registry.rs`.

## Driver contract

```rust
#[async_trait]
pub trait CameraDriver: Send + Sync {
    fn id(&self) -> &'static str;
    fn spec(&self) -> CameraSpec;                 // resolution, bit depth, calibration
    async fn open(&self) -> Result<Box<dyn CameraSession>>;
}

#[async_trait]
pub trait CameraSession: Send {
    fn spec(&self) -> &CameraSpec;
    async fn next_frame(&mut self) -> Result<CameraFrame>;
    async fn close(&mut self);
}
```

Frames are either `FramePayload::Video(Bytes)` (opaque, forwarded untouched) or
`FramePayload::Thermal(ThermalImage)` (raw `u16` samples plus calibration, so
temperature math is driver-neutral).

## Run

```bash
cargo build --release

# what can this binary talk to?
./target/release/thermolink drivers

# handshake + 4 seconds of frames; prints the spec and thermal min/max
./target/release/thermolink probe --host 192.168.1.64

# the camera loop with reconnect/backoff and frame logging
RUST_LOG=thermolink=info ./target/release/thermolink stream --host 192.168.1.64

# the whole app against the simulator — no camera required
./target/release/thermolink serve --demo --bind 0.0.0.0:8099

# the whole app against a real camera
./target/release/thermolink serve --host 192.168.1.64 --bind 0.0.0.0:8099
```

Open `http://localhost:8099`. The UI is a live MJPEG view with min/mean/max,
the three hottest spots, palette switching, and independent clip and span bands
(clip hides temperatures as white; span maps the palette onto a chosen range).
`Save JPEG` downloads the current render.

The UI is translated into English, Spanish, French, German, Portuguese, Chinese,
Japanese, Russian and Arabic. The picker sits in the header; the choice is kept
in `localStorage`, Arabic switches the page to right-to-left, and every key falls
back to English. Language files live in `web/i18n/` and are embedded in the
binary, so `/i18n/<lang>.json` works without a sidecar directory.

### HTTP surface

| Path | Purpose |
|------|---------|
| `/` | web UI |
| `/i18n/<lang>.json` | UI translations (public; `en`, `es`, `fr`, `de`, `pt`, `zh`, `ja`, `ru`, `ar`) |
| `/api/spec` | driver capabilities (resolution, bit depth, calibration) |
| `/api/thermal.json` | stats, peaks, display settings, connection state |
| `/api/thermal.jpg` | one rendered frame |
| `/api/thermal.mjpg` | multipart MJPEG at 10 fps |
| `/api/display` | GET/POST palette + clip/span |
| `/api/raw.h264` | raw visible stream passthrough |
| `/api/health` | liveness + frame sequence |
| `/api/version` | running build and update repository |
| `/api/update` | GET: latest release; POST: install its binary (admin) |
| `/api/restart` | exec the relay in place (admin) |

`probe` is the honest test: if no thermal frame arrives it exits non-zero. A
polite handshake alone does not mean the camera accepted the session.

## Qianli SuperCam (LC-AD11 / TB-03 v2)

* Control port **3001**, media port **3000**, both TCP.
* The camera issues a **fresh 4-byte flag per session** in the probe reply
  (bytes 28..30 are `01 00`, flag at 48..52). START and CTRL must echo it.
  A stale flag yields a perfectly polite handshake and a stream that closes
  after 1–3 s with zero frames.
* Thermal is a **160×120, 16-bit LE** image: 38400 bytes at offset 88 of each
  fixed **39448-byte** wrapper whose `RAW` marker sits at offset 48.
* Keepalives (type `0x05`) go on the **probe** sockets every 10 s, not on the
  streaming sockets.

Full details, including the settings ritual and the notes about port 23, are in
[`docs/PROTOCOL.md`](docs/PROTOCOL.md).

### Updating a deployment

`GET /api/update` asks GitHub for the latest release of `flip555/thermolink`
(override with `THERMOLINK_REPO`). `POST /api/update` downloads the asset whose
name matches this OS and architecture, checks it is a real executable, copies
the current binary to `thermolink.bak-<timestamp>` and renames the new one into
place. The running process keeps the old image until `POST /api/restart`, which
execs the new binary with the same arguments and PID; in-memory sessions are
lost, so everyone signs in again. Both endpoints are admin-only.

## Roadmap

1. **Visible + thermal composition** — WebCodecs H.264 in the browser, Wipe /
   PiP / Fusion modes, alignment controls.
2. **Measurement** — regions (point / rect / polygon), reference and ΔT image,
   CSV history, snapshot store with raw-frame re-analysis.
3. **Settings + calibration** — two-point fit, persisted per driver, plus the
   camera's own 376-byte network block behind a driver capability.
4. **Alarms, history, auth** — port the remaining Python modules.

## Credits

Protocol knowledge comes from the reverse-engineering work in
`qianli-supercam` (`docs/PROTOCOL.md`, `supercam/protocol.py`). No vendor code
or documentation is used or redistributed.
