# HTTP API

Base: `http://<host>:8099` or `https://<host>:8443`.

## Auth

```http
Authorization: Bearer <token>
Cookie: thermolink_session=<token>
GET /api/thermal.jpg?token=<token>
```

`POST /api/login` returns `{"ok":true,"token":"…"}`. Public: `/`, `/i18n/*`,
`/api/health`, `/api/version`, `/api/whoami`, `/api/setup`, `/api/login`.

Admin sees everything; guests get per-capability permissions (`measure`,
`snapshot`, `display`, `reference`, `calibration`, `alarms`, `prerecord`).

## Groups

| Area | Paths |
|---|---|
| State | `/api/spec`, `/api/thermal.json`, `/api/display`, `/api/align`, `/api/calibration` |
| Images | `/api/thermal.jpg/.mjpg`, `/api/video.jpg/.mjpg`, `/api/raw.h264`, `/api/dt.*`, `/api/compare.*`, `/api/reanalyze.*` |
| Measurement | `/api/regions`, `/api/regions/{id}`, `/api/regions/{id}/ref`, `/api/regions.csv`, `/api/point` |
| References | `/api/references`, `/api/references/{id}`, `/api/references/{id}/activate`, `/api/reference` |
| Alarms | `/api/alarms`, `/api/alarms/{id}`, `/api/events` |
| History | `/api/history.json`, `/api/history.csv` |
| Snapshots | `/api/snapshot`, `/api/snapshots`, `/api/snapshots.zip`, `/api/snapshots/{name}[.raw|.json|.vis]` |
| Clips | `/api/captures`, `/api/captures.zip`, `/api/captures/{name}[.zip]`, `/api/prerecord` |
| Camera | `/api/camera/settings`, `/api/camera/reboot`, `/api/discover` |
| Drivers | `/api/drivers`, `/api/camera/select` |
| Account | `/api/account`, `/api/login`, `/api/logout`, `/api/whoami` |
| Relay | `/api/update`, `/api/restart`, `/api/health`, `/api/version` |
| UI text | `/i18n/{lang}.json` |

Full table and examples: [docs/API.md](https://github.com/flip555/thermolink/blob/main/docs/API.md).
