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

### `uvc_radiometric` — unified UVC family (M1, Linux)

One driver, data-only profiles. The current profile set is:

| profile | VID:PID | layout | radiometry | validation |
|---------|---------|--------|------------|------------|
| `topdon_tc001` | `0bda:5830` | 256×384 YUYV; bottom half u16 LE, 256×192 | linear, 1/64 K | ported; needs device |
| `infiray_p2pro` | `0bda:5830` | same as TC001 | linear, 1/64 K | ported; needs device |
| `topdon_tc002c` | unknown (explicit profile) | 256×392 YUYV; packed raw16 + metadata + image | empirical device-temperature fit | needs hardware + VID/PID |
| `ht301` | `1514:0001` | 384×288 raw U16 + 4 metadata rows | per-frame table | ported; needs device |
| `xtherm_t2s` | T2S+ v1 `1514:xxxx`, v2 `04b4:0100` | 256×192 raw U16 + 4 metadata rows | per-frame table | ported; needs device |
| `hik_mini2` | `2bdf:0102` (probable) | 256×192 YUYV Y-channel | relative only; `radiometric: false` | needs hardware + coefficients |
| `uvc_generic` | any UVC | best-effort raw view | unknown; `radiometric: false` | needs research |

- Transport: Linux V4L2 through ffmpeg's `v4l2` input. ffmpeg is already a
  runtime dependency for the visible-stream transcode; no libclang/bindgen is
  required.
- Detection order: explicit `camera.json` `profile` → VID/PID + product string
  → geometry/ambiguity → `uvc_generic` fallback. Detection never blocks
  streaming because a device is unknown.
- `0bda:5830` is shared by the TC001, P2 Pro, TS001, MaAnt and UTi261M. M1
  returns both candidate profiles and marks the result **ambiguous**; it never
  silently claims one model. Use `profile` in `camera.json` or the UI profile
  selector to force one.
- `uvc_generic` streams a best-effort raw view with `radiometric: false`.
  Temperature measurement is hidden by the UI and rejected by the API.
- `topdon_tc002c`, `ht301`, `xtherm_t2s` and `hik_mini2` are ported from public
  reverse-engineering projects. They are likely-working implementations, not
  hardware-validated ones. `topdon_tc002c` has no published VID/PID and must be
  selected explicitly; HIK Mini2 is relative-only until per-unit polynomial
  coefficients are supplied.
- HT301/T2S+ require the UVC zoom-unit raw-mode command (`0x8004`). The Linux
  transport applies it with a V4L2 `VIDIOC_S_CTRL` before starting ffmpeg.
- Controls (shutter, gain, palette) are **not exposed in M1**. The P2 Pro
  vendor command set is documented for a later phase.
- **No hardware validation has been performed by this project.** Every profile
  is `tested: false`; community sources are listed in `/api/drivers`.

### Hardware needed

The validation state is exposed in `/api/drivers`, `/api/spec` and the UI badge.
The following devices are wanted for capture/validation; a loan unit or a user
with the model is enough:

| profile | what is missing |
|---------|-----------------|
| `topdon_tc001` / `infiray_p2pro` | a real `0bda:5830` unit to confirm the split and resolve the TC001/P2 Pro ambiguity |
| `topdon_tc002c` | VID/PID, a raw frame, and a temperature reference; the public calibration is empirical |
| `ht301` | a `1514:0001` unit to validate the raw-mode command, metadata parsing and table LUT |
| `xtherm_t2s` | a T2S+ v1/v2 unit; v2 is `04b4:0100`, v1 PID is unpublished |
| `hik_mini2` | a unit plus per-unit polynomial coefficients; otherwise relative-only |

Until one of these is available, the profiles stay **ported/unvalidated** and
must not be described as supported hardware.

### Testing an unvalidated profile

1. In the Camera tab, set the driver to `uvc_radiometric`.
2. Pick the closest profile; the dropdown shows its validation state.
3. Click **Use driver**.
4. If it works or fails, click **Report hardware / capture** and include the
   VID:PID, profile, and a raw frame or short capture.

`camera.json` for an explicit M1 profile:

```json
{
  "driver": "uvc_radiometric",
  "profile": "topdon_tc001",
  "auto_detect": false,
  "device": { "vid": 3034, "pid": 22576, "path": null }
}
```

`GET /api/camera/detect` returns candidates, scores, reasons, `tested`,
`confidence`, and `ambiguous`. `POST /api/camera/select` accepts `profile` and
`device` alongside `driver`.

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
