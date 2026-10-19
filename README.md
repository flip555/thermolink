# ThermoLink

One web UI for thermal cameras, with the camera itself behind a driver boundary.

This is the Rust port of [`qianli-supercam`](https://github.com/flip555/qianli-supercam).
The Python project proved the protocol and the interface; ThermoLink keeps the
features and removes the coupling — the web server never learns a camera's
resolution, packet layout, or network dialect. It asks a driver for a
`CameraSpec` and consumes `CameraFrame`s.

**Status: milestone 1.** Project structure, driver contract, the Qianli
handshake, and thermal frame extraction are implemented and unit-tested against
captured bytes. The HTTP UI is the next milestone.

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
```

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

## Roadmap

1. **Web server** — Axum: `/thermal.json`, `/stream.mjpg`, `/raw.h264`,
   `/health`, settings endpoints. Port `web/index.html` unchanged.
2. **Thermal rendering** — palette LUTs (rainbow / ironbow / white-hot /
   black-hot), clip and span bands, isotherm, ΔT against a reference, regions.
3. **Settings + calibration** — two-point fit, persisted per driver, plus the
   camera's own 376-byte network block behind a driver capability.
4. **Alarms, history, snapshots, auth** — port the remaining Python modules.

## Credits

Protocol knowledge comes from the reverse-engineering work in
`qianli-supercam` (`docs/PROTOCOL.md`, `supercam/protocol.py`). No vendor code
or documentation is used or redistributed.
