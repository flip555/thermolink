# HTTP API

Base URLs: `http://<host>:8099` or `https://<host>:8443` (self-signed).
JSON unless stated. Errors are plain text with a 4xx/5xx status.

## Authentication

Send one of:

```http
Authorization: Bearer <session-token>
Cookie: thermolink_session=<session-token>
GET /api/thermal.jpg?token=<session-token>
```

The query form exists for `<img>`/MJPEG URLs, which cannot set headers. Login
returns the token in JSON:

```bash
curl -sk https://host:8443/api/login \
  -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"…"}'
# {"ok":true,"token":"…"}
```

Public paths: `/`, `/i18n/*`, `/api/health`, `/api/version`, `/api/whoami`,
`/api/setup`, `/api/login`.

## Roles

- **admin** — everything.
- **guest** — only granted capabilities: `measure`, `snapshot`, `display`,
  `reference`, `calibration`, `alarms`, `prerecord`.
- Admin-only: account management, notifications, camera settings/reboot,
  driver selection, discovery writes, update/restart.

## State and rendering

| Method | Path | Purpose |
|---|---|---|
| GET | `/api/spec` | driver, streams, capabilities, live calibration |
| GET | `/api/thermal.json` | stats, peaks, display, connection state, sequence |
| GET | `/api/thermal.jpg` | one rendered thermal frame |
| GET | `/api/thermal.mjpg` | thermal MJPEG stream |
| GET | `/api/video.jpg` | one visible JPEG (ffmpeg) |
| GET | `/api/video.mjpg` | visible MJPEG stream |
| GET | `/api/raw.h264` | live Annex-B H.264 for WebCodecs |
| GET | `/api/dt.jpg` / `.json` / `.mjpg` | difference against the reference |
| GET | `/api/compare.jpg` / `.json` | current vs one reference, or `a`/`b` |
| GET | `/api/reanalyze.jpg` / `.json` | re-render a snapshot with overrides |
| GET/POST | `/api/display` | palette, clip/span, ΔT span |
| GET/POST | `/api/align` | visible-feed alignment |
| GET/POST | `/api/calibration` | scale/offset |
| GET/POST | `/api/isotherm` | isotherm bands and profiles |

`reanalyze` and `compare` accept `name`, optional `palette`, `clip_lo`,
`clip_hi`, `span_lo`, `span_hi`; compare also accepts `a`, `b`.
`reanalyze.json` also returns the snapshot's `peaks`, `regions`, `display` and
`has_video`, which drives the stored-view UI.

## Measurement

| Method | Path | Purpose |
|---|---|---|
| GET/POST | `/api/regions` | list / create point, rect or polygon |
| DELETE | `/api/regions/{id}` | delete a region |
| POST | `/api/regions/{id}/ref` | mark a region as reference |
| GET | `/api/regions.csv` | region statistics export |
| GET | `/api/point?x=&y=` | probe one thermal pixel |
| GET/POST | `/api/references` | list / create snapshots as references |
| DELETE | `/api/references/{id}` | delete a reference |
| POST | `/api/references/{id}/activate` | make it the ΔT reference |
| POST/DELETE | `/api/reference` | set a reference from the current frame / clear |

## Alarms and history

| Method | Path | Purpose |
|---|---|---|
| GET/POST/DELETE | `/api/alarms` | list / add / clear alarms |
| DELETE | `/api/alarms/{id}` | remove one alarm |
| GET | `/api/events` | last 50 alarm events |
| GET | `/api/history.json` | samples; `limit`, `since`, `clear=1` |
| GET | `/api/history.csv` | history export |

## Snapshots, clips, pre-record

| Method | Path | Purpose |
|---|---|---|
| POST | `/api/snapshot` | save JPEG + raw + JSON |
| GET | `/api/snapshots` | list names |
| GET | `/api/snapshots.zip` | all snapshots |
| GET | `/api/snapshots/{name}` | JPEG |
| GET | `/api/snapshots/{name}.raw` / `.json` | raw samples / metadata |
| GET | `/api/snapshots/{name}.vis` | saved visible JPEG (new snapshots) |
| DELETE | `/api/snapshots/{name}` | delete |
| GET | `/api/captures` | list clips |
| GET | `/api/captures.zip` | all clips |
| GET | `/api/captures/{name}` | clip MJPEG |
| GET | `/api/captures/{name}.zip` | clip MJPEG + metadata |
| GET/POST | `/api/prerecord` | pre-record config / clip actions |

## Camera, drivers, account, update

| Method | Path | Purpose |
|---|---|---|
| GET/POST | `/api/camera/settings` | read / write camera network settings |
| POST | `/api/camera/reboot` | reboot camera; body `{"confirm":"reboot"}` |
| GET/POST | `/api/discover` | discover cameras |
| GET | `/api/drivers` | compiled drivers + capabilities + current id |
| POST | `/api/camera/select` | `{"driver":"qianli","host":"…"}`; hot-swaps |
| POST | `/api/setup` | first-run admin creation |
| POST | `/api/login` / `/api/logout` | session |
| GET | `/api/whoami` | role, guest state, permissions |
| GET/POST | `/api/account` | account, guest, permissions |
| GET/POST | `/api/notify` | notification credentials |
| GET | `/api/update` | update availability |
| POST | `/api/update` | install update |
| POST | `/api/restart` | in-place restart |
| GET | `/api/health` / `/api/version` | liveness / build |
| GET | `/i18n/{lang}.json` | translations (public) |

## Examples

```bash
curl -sk -H "Authorization: Bearer $TOKEN" \
  https://host:8443/api/thermal.jpg -o thermal.jpg

curl -sk "https://host:8443/api/thermal.mjpg?token=$TOKEN" -o thermal.mjpg

curl -sk -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"driver":"qianli","host":"192.168.1.64"}' \
  https://host:8443/api/camera/select
```

Status codes: `200` success, `400` validation, `401` no/expired session,
`403` role/capability, `404` missing, `409` setup conflict, `501` unsupported by
the selected driver, `502` camera/upstream failure, `503` no frame yet.
