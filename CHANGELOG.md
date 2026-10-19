# Changelog

## 0.3.1 — 2026-10-19

- Sync the `Cargo.lock` package version after the 0.3.0 tag.
- `jpeg-encoder` 0.7.1 and CodeQL action v4 (closes the remaining Dependabot PRs).

## 0.3.0 — 2026-10-19

- Stored-view snapshot re-analysis in the main window: saved thermal + visible
  frames, current palette/bands, wipe/PiP/fusion and alignment, stored peaks and
  regions, then Exit back to live.
- Snapshots now save the visible frame (`.vis`) alongside JPEG/raw/JSON.
- Driver hot-swap and per-driver camera settings; P3 and Qianli improvements.
- Security: `zip` 9.0.5 (RUSTSEC-2026-0367/0368/0369), `argon2` 0.6 /
  password-hash 0.6.
- CI: fmt, clippy, tests, release build; weekly RustSec audit; CodeQL gated to
  public repos; tag releases for x86_64 and aarch64.
