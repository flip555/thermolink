# Drivers

| id | transport | thermal | visible | status |
|----|-----------|---------|---------|--------|
| `qianli` | TCP 3001/3000 | 160×120 | 1920×1080 H.264 | full |
| `p3` | USB | 256×192 / 160×120 | — | parser + calibration, **untested**, USB not wired |
| `mock` | in-process | 160×120 | — | simulator |

A driver owns transport, framing, pixels and calibration. The server consumes
`CameraSpec` and `CameraFrame`; it never imports a concrete driver.

## Capabilities

`CameraSpec.capabilities` advertises thermal, visible, discovery, network
settings, reboot and a controls list. The UI hides modes and fields that do not
apply; the API still validates. A P3 therefore shows no video modes.

## Hot-swap

`GET /api/drivers` lists compiled drivers. `POST /api/camera/select` with
`{"driver":"qianli","host":"192.168.1.64"}` stores `camera.json`, swaps the
`CameraRuntime` in an `ArcSwap`, and bumps a generation watch; the supervisor
closes the old session and opens the new one. No process restart.

## Adding a driver

1. `src/camera/<family>/` implements `CameraDriver` + `CameraSession`.
2. Fill the spec and capabilities honestly.
3. Add one arm + id in `src/camera/registry.rs`.
4. Add protocol/parser tests; byte captures are ideal fixtures.

Full contract: [docs/DRIVERS.md](https://github.com/flip555/thermolink/blob/main/docs/DRIVERS.md).
