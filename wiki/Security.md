# Security

- Passwords are Argon2; roles are `admin` and `guest` with per-capability
  grants. Admin-only: account, notifications, camera settings/reboot, driver
  selection, update/restart.
- Sessions expire after 7 days and persist in `sessions.json` across restarts.
  Login returns a cookie and a token; the token is stored in `localStorage` and
  sent as a Bearer header. `?token=` is accepted for `<img>` streams.
- HTTPS on `:8443` uses a self-signed certificate. While it is self-signed the
  session cookie is **not** marked `Secure`: browsers can drop it on a
  cert-error origin, which caused a login/401 loop. The Bearer path is primary.
- Query tokens can leak via history/logs/Referer. Prefer the header; use query
  tokens only for image streams.
- Do not expose the relay directly to the internet. Use a trusted TLS reverse
  proxy or VPN, firewall `:8099`/`:8443` to trusted networks, and protect the
  data directory (it holds `auth.json`, `sessions.json`, `tls-key.pem`).

Full detail: [docs/SECURITY.md](https://github.com/flip555/thermolink/blob/main/docs/SECURITY.md).
