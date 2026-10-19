# Drivers and capabilities

A driver owns everything vendor-specific: how to connect, how packets are
framed, how pixels are laid out, and what the raw values mean. The rest of the
app works only with `CameraSpec`, `CameraFrame` and `Capabilities`.

## Contract

```rust
#[async_trait]
pub trait CameraDriver: Send + Sync {
    fn id(&self) -> &'static str;
    fn spec(&self) -> CameraSpec;
    async fn open(&self) -> Result<Box<dyn CameraSession>>;
}

#[async_trait]
pub trait CameraSession: Send {
    fn spec(&self) -> &CameraSpec;
    async fn next_frame(&mut self) -> Result<CameraFrame>;
    async fn close(&mut self);
}
```

`CameraSpec` advertises the streams and static capabilities:

```json
{
  "driver": "qianli",
  "model": "Qianli SuperCam …",
  "video": { "width": 1920, "height": 1080, "codec": "h264-annexb", "fps_hint": 30.0 },
  "thermal": { "width": 160, "height": 120, "bit_depth": 16,
               "calibration": { "scale": 0.01, "offset": 0.0 } },
  "capabilities": {
    "thermal": true, "visible": true,
    "discover": true, "network_settings": true, "reboot": true,
    "controls": ["network"]
  }
}
```

The UI hides modes/tabs/fields from `capabilities`. The API still validates;
unsupported operations return an error rather than pretending to work.

## Built-in drivers

### `qianli` — Qianli SuperCam over TCP

- Control/registration `:3001`, media `:3000`.
- Handshake, settings read/write, reboot, /24 discovery probe.
- Thermal 160×120, 16-bit raw wrapper; visible H.264 1920×1080.
- Capabilities: thermal + visible + discovery + network settings + reboot.

### `p3` — P3/P1 USB thermal

- VID `0x3474`; P3 PID `0x45A2` (256×192), P1 PID `0x45C2` (160×120).
- Protocol from [`jvdillon/p3-ir-camera`](https://github.com/jvdillon/p3-ir-camera):
  - 12-byte start marker, pixel buffer `2 * (2*h + 2) * w` bytes.
  - IR rows `0..h`, two metadata rows, thermal rows `h+2..2h+2`.
  - Raw values are 1/64 Kelvin; `scale = 1/64`, `offset = -273.15`.
- USB transport: `nusb` claims interfaces 0 and 1, sets interface 1 to alt 1,
  sends the vendor control commands, reads bulk endpoint `0x81`, and
  reassembles the start/pixel/end markers.
- The session validates `cnt1` between start and end markers, resynchronises
  after corrupt bytes, and queues any extra frames from a read.
- Shutter and high/low-gain commands are implemented on the session but not yet
  exposed by the server control surface.
- Tested offline: frame parser, marker validation, calibration and split-frame
  assembler tests for P3 and P1. **Hardware validation is still pending.**

### `mock` — simulator

- 160×120 synthetic thermal frames, no visible stream.
- Used by `serve --demo` and the test suite.

## Adding a driver

1. Create `src/camera/<family>/` with a `CameraDriver` + `CameraSession`.
2. Fill `CameraSpec` and `Capabilities` honestly.
3. Add one arm to `src/camera/registry.rs` and the id to `KNOWN`.
4. Add parser/protocol unit tests; capture bytes make the best fixtures.
5. Do not import the driver from `src/server/`.

Driver selection at runtime is `POST /api/camera/select` with
`{ "driver": "qianli", "host": "…" }`; it stores `camera.json` and hot-swaps the
supervisor. `/api/drivers` lists compiled drivers and the current one.

## Discovery and controls

Discovery is driver-specific: a network driver probes its protocol; a USB driver
enumerates VID/PID. Reboot, gain, shutter and network writes belong to the
driver session, not the server. A driver without a control simply omits it from
`capabilities.controls`.
