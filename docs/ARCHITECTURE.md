# Architecture

ThermoLink is a single process with four layers. The rule that keeps it modular:
**the server never imports a concrete driver.**

```
camera driver  ->  FrameHub  ->  server/render  ->  browser
 (vendor I/O)      (fan-out)      (HTTP/JPEG)       (UI/WebCodecs)
```

## 1. Drivers

`src/camera/` defines the contract:

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

A driver owns transport, framing, pixel layout and calibration. A frame is
either `FramePayload::Thermal(ThermalImage)` or `FramePayload::Video(Bytes)`.
Thermal images carry raw `u16` samples plus a linear `Calibration { scale, offset }`
and an optional 65 536-entry temperature LUT. Rendering calls
`ThermalImage::celsius()`, so table-calibrated sensors do not leak vendor math
above the driver boundary.

`CameraSpec` carries:

- `driver`, `model`, optional `profile` and `device`
- `thermal: Option<ThermalStream>` (size, bit depth, calibration)
- `video: Option<VideoStream>` (size, codec hint, fps)
- `capabilities`: thermal, visible, radiometric, discover, network settings,
  reboot, controls

Capabilities are static facts. The UI hides modes and controls from them; the
API still enforces them server-side.

Qianli is `src/camera/qianli/`; the simulator is `src/camera/mock.rs`; P3/P1 is
`src/camera/p3/` (parser and calibration only — see [Drivers](DRIVERS.md)).
The UVC family is `src/camera/uvc_radiometric/`: one `CameraDriver`, data-only
profiles (`topdon_tc001`, `infiray_p2pro`, `uvc_generic` in M1), a fingerprint
table, and a transport trait with a Linux V4L2/ffmpeg backend plus a mock.

## 2. FrameHub

`src/stream/` keeps the latest thermal and visible frame and broadcasts every
frame to subscribers. `seq` increases per frame; MJPEG clients and the
supervisor are independent subscribers. A slow subscriber is dropped
(`Lagged`) rather than blocking the camera.

## 3. Camera supervisor and hot-swap

`camera_supervisor` owns the reconnect loop:

1. read the current `CameraRuntime { driver, spec }` from an `ArcSwap`;
2. `driver.open()` with a 20 s timeout;
3. `select!` between `next_frame()` and a generation watch;
4. on stream end, close and back off; on a driver switch, reopen immediately.

`POST /api/camera/select` builds the new driver, stores the runtime, and bumps
the generation watch. Handlers read `st.camera.load()` synchronously; there is no
process restart and the HTTP server keeps serving.

## 4. Rendering

- Thermal JPEG/MJPEG: one render per `(seq, display, calibration)` is cached and
  shared by all clients.
- ΔT JPEG/MJPEG: reference frame subtracted, span from display settings.
- Compare: current frame against one reference or reference A/B.
- Visible: ffmpeg transcodes camera H.264 to MJPEG for compatibility. Over
  HTTPS the browser instead reads `/api/raw.h264` and decodes with
  `VideoDecoder` (WebCodecs), drawing to a canvas clipped to the thermal frame;
  any failure falls back to MJPEG (`?mjpeg=1` forces it).
- Snapshot: rendered JPEG + raw thermal + saved visible frame + JSON metadata
  (stats, display, regions, region stats, reference time, `has_video`).
- Re-analysis: the stored view swaps the live `<img>` sources for the saved
  thermal/visible frames, re-renders thermal on display changes, freezes live
  stats/peaks, and keeps the viewer modes working until Exit.

## 5. HTTP and auth

`src/server/` is the only HTTP surface. Middleware order: optional TLS marker,
auth, then route handlers. Public paths are `/`, `/i18n/*`, `/api/health`,
`/api/version`, `/api/whoami`, `/api/setup`, `/api/login`.

Auth accepts, in order:

1. `Authorization: Bearer <session>`
2. `thermolink_session` cookie
3. `?token=<session>` (for `<img>` streams, which cannot send headers)

Sessions are Argon2-backed accounts with admin/guest roles and persisted in
`sessions.json`; guest permissions are per capability.

## 6. Persistence

All state is JSON in `--data-dir`: auth, sessions, calibration, display, align,
regions, alarms, references, isotherm profiles, camera selection, prerecord
config, notifications. Writes go through `write_atomic` (temp file + rename) and
return errors to the caller. Snapshots/captures are files; pre-record uses an
in-memory ring and flushes clips.

## Extending

- Add a driver: implement `CameraDriver` in `src/camera/<family>/`, add one arm
  to `registry.rs`, set `capabilities`, done. No server or UI change.
- Add a UVC profile: add one `RadiometricProfile` table entry (and a
  `FrameLayout` variant only if the pixel layout is genuinely new). Do not
  copy a driver.
- Add a control: extend the driver's session API and advertise it in
  `capabilities.controls`; add a descriptor so the UI can render it.
- Never import a concrete driver from `src/server/`.
