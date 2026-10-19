# Security

## Accounts and roles

- First run: `POST /api/setup` creates the admin account; remote setup may
  require the one-time setup token printed in the log.
- Passwords are Argon2 hashes in `auth.json`.
- Roles: `admin` and `guest`. Guests are opt-in, with per-capability grants:
  `measure`, `snapshot`, `display`, `reference`, `calibration`, `alarms`,
  `prerecord`.
- Admin-only: account management, notifications, camera settings/reboot, driver
  selection, discovery writes, update/restart.
- Reads require a signed-in session; the UI hides what a role cannot do, and the
  server enforces it.

## Sessions

Sessions live in `sessions.json`, expire after 7 days, and survive relay
restarts. Login returns both a cookie and a token:

- cookie `thermolink_session` (HttpOnly, SameSite=Lax)
- JSON `token`, stored in `localStorage` and sent as `Authorization: Bearer`
- `?token=` is accepted for `<img>`/MJPEG URLs, which cannot send headers

Logout removes the session server-side, clears the cookie and the stored token.

## HTTPS

The relay serves HTTPS on `:8443` with a self-signed certificate generated on
first start (`tls-cert.pem`, `tls-key.pem` in the data dir). The browser warning
is expected; the certificate is valid for `172.20.1.85`, `172.20.5.127`,
`127.0.0.1` and `localhost`.

**Cookie caveat:** while the certificate is self-signed, the session cookie is
deliberately **not** marked `Secure`. Chrome treats cert-error origins as
non-secure and otherwise drops the cookie, which produces a successful login
followed by 401s. The Bearer/token path is the primary mechanism. Once a trusted
certificate or reverse proxy is in place, re-enable `Secure`.

## Token exposure

- Bearer tokens are stored in `localStorage`; XSS would expose them, so treat the
  origin as trusted.
- Query tokens can appear in browser history, logs, proxies and `Referer`
  headers. Prefer the Bearer header for API calls; use query tokens only for
  image streams.
- Treat the data directory as sensitive: `auth.json`, `sessions.json`,
  `tls-key.pem`, notification credentials.

## Network exposure

- Do not expose `:8099`/`:8443` directly to the internet. Put a trusted TLS
  reverse proxy or VPN in front, and keep the relay on a trusted LAN.
- If HTTP is exposed, remember the non-Secure cookie can travel over it. Prefer
  HTTPS and firewall HTTP to the LAN.
- Discovery probes the local `/24` for port 3001 and verifies the vendor
  protocol; it does not scan beyond the configured subnet.

## Recommendations

1. Replace the self-signed certificate with a trusted one (reverse proxy or
   ACME), then re-enable `Secure` cookies.
2. Keep the relay on a segmented camera VLAN; restrict admin access.
3. Back up `auth.json` and `tls-key.pem` securely; rotate the admin password and
   notification credentials if the host is compromised.
4. Review guest permissions; disable the guest account when unused.
