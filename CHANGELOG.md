# Changelog

## 0.4.0 — 2026-10-19

- P3/P1 USB transport using `nusb`: interface claim, vendor control init,
  alt-setting 1, bulk `0x81` reads, start/end marker + counter validation,
  resynchronisation and frame queueing.
- Shutter and high/low-gain commands are implemented on the session; the server
  control surface does not expose them yet.
- Offline tests cover the split-frame assembler and counter mismatch. Hardware
  validation against a real P3/P1 is still pending.

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
