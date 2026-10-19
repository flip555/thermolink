# Troubleshooting

## HTTPS login succeeds but every API call is 401

Symptom: the login POST returns 200 (or sessions appear in `sessions.json`), but
the UI keeps showing 401s.

Cause: the browser dropped the session cookie. On a self-signed certificate,
Chrome can treat the origin as non-secure and reject a `Secure` cookie.

What to do:

1. Hard-refresh the page. The current build stores the returned token in
   `localStorage` and sends `Authorization: Bearer`; the cookie is only a
   fallback and is deliberately not `Secure`.
2. Check `localStorage` for `thermolink.token` after logging in.
3. If you still see 401s, log in again (an old tab may hold a pre-token session),
   then reload.
4. Use `https://<host>:8443` consistently; a cookie from a different hostname
   will not apply.

Verified server-side with:

```bash
curl -sk -H "Authorization: Bearer $TOKEN" https://host:8443/api/thermal.json
curl -sk "https://host:8443/api/thermal.jpg?token=$TOKEN" -o frame.jpg
```

## Thermal works but the visible feed is blank

- Is `ffmpeg` installed and on `PATH`? Check `Camera → Source → Video`.
- Over HTTPS, the page first tries WebCodecs on `/api/raw.h264`. If the label
  says `MJPEG`, hover it or check the console for the reason:
  - `no VideoDecoder` — the browser is not exposing WebCodecs (certificate
    context or policy).
  - `decoder error` — the stream/SPS was rejected; MJPEG fallback should return.
- Force the old path with `?mjpeg=1` in the URL.
- A query token must be present on `<img>` URLs:
  `…/api/video.mjpg?t=…&token=…`.

## Visible streams are 401 while the API works

`<img>` cannot send an `Authorization` header. The page appends `?token=` to
image URLs; if you use the endpoint directly, do the same. Confirm the URL
contains `&token=` or `?token=`.

## Chart is empty

The chart needs at least two samples (one per second while thermal frames are
arriving). Check `/api/thermal.json` for `ready: true`, then wait a few seconds.
The clear action resets history; the window buttons are 5m / 1h / all.

## Snapshots open as a small image or return 401

Snapshot rows now have distinct **jpg / raw / json / delete** actions. Direct
links need a session: use the token query form (`?token=…`) or the in-app link.
The small image is the rendered JPEG; `raw` is the thermal samples and `json`
is metadata.

## Camera offline / no thermal

- Check `Camera → Source` (`Link`, `Video`, `Frame`).
- Ping the camera and confirm TCP 3001/3000 are reachable.
- Run `thermolink probe --host <camera>`.
- Use **Camera → Discovery**, or switch to `mock` to prove the UI/relay path.

## Driver switch made the feed disappear

`p3` uses `nusb` and needs permission to claim interfaces 0/1. If selecting it
produces no frames, install the udev rule from DEPLOYMENT.md, check `dmesg` for
USB errors, and confirm the PID (0x45A2 P3 / 0x45C2 P1). Hardware validation is
still pending for this driver. Switch back to `qianli` or `mock` to prove the
UI path; the capability UI hides video modes for thermal-only drivers.

## Camera settings or discovery controls are missing

They are capability-driven. A driver that does not advertise
`network_settings` or `discover` hides those controls; this is expected. See
`/api/spec` → `capabilities`.

## Re-analysis has no visible frame

Snapshots taken before visible-frame capture do not have a `.vis` file and show
thermal only. Take a new snapshot. If a new snapshot still has no video, check
that the visible feed is running (`Camera → Source → Video`).

## Re-analysis peaks show NaN

Fixed: `reanalyze.json` maps `Peak.celsius` to `c`. Hard-refresh the page.

## Disk full

`cargo test` builds a large debug tree. `cargo clean --profile dev` frees it
(the release binary is kept). Also prune `snapshots/` and `captures/` in the
data directory. Check with `df -h /`.

## Updates / restart

**Camera → Relay → Restart relay** re-executes the same binary with the same
arguments. Sessions persist. If the relay does not come back, run it manually
with `RUST_LOG=thermolink=info` and inspect the log.
