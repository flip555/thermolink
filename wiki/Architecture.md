# Architecture

```
camera driver -> FrameHub -> server/render -> browser
 (vendor I/O)    (fan-out)    (HTTP/JPEG)      (UI/WebCodecs)
```

- **Drivers** own transport and protocol; they emit `ThermalImage` (raw u16 +
  calibration) and/or H.264 `Bytes`.
- **FrameHub** keeps the latest thermal/visible frame and broadcasts to
  subscribers; slow clients are dropped rather than blocking the camera.
- **Supervisor** opens a session, reads frames, and `select!`s on a generation
  watch. A driver switch closes the old session and reopens immediately.
- **CameraRuntime** (`driver` + `spec`) lives in an `ArcSwap`, so handlers read
  it synchronously during hot-swap.
- **Rendering**: thermal JPEG/MJPEG is cached per `(seq, display, calibration)`.
  ΔT and A/B compare are derived from references. Visible uses ffmpeg MJPEG, or
  WebCodecs over HTTPS (`/api/raw.h264` → canvas) with MJPEG fallback.
- **Auth**: Argon2 accounts, admin/guest roles, persisted sessions; the token is
  accepted as a cookie, Bearer header, or `?token=` for image URLs.

Key invariant: `src/server/` never imports a concrete driver.
