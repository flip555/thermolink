# Changelog

## Unreleased — Phase 3 profiles

- Added `topdon_tc002c`, `ht301`, `xtherm_t2s` and `hik_mini2` profiles ported
  from public reverse-engineering projects.
- HT301/T2S+ use per-frame table calibration (65,536-entry LUT); TC002C uses
  an empirical device-temperature calibration; HIK Mini2 exposes a relative
  Y-channel image with `radiometric: false`.
- The Linux UVC transport applies the HT301/T2S raw-mode command through the
  V4L2 zoom control before starting ffmpeg.
- Added synthetic layout/calibration tests and new fingerprint tests.
- Profiles now expose a validation state (`ported_unvalidated`, `needs_hardware`,
  `needs_research`) in `/api/drivers` and the UI; `docs/DRIVERS.md` lists the
  hardware still needed for each model.
- All new profiles are `tested: false`; no hardware support is claimed.

## Unreleased — desktop shell

- Phase 4A: shared `src/runtime.rs` for `thermolink serve` and the desktop
  shell. `RunningServer` exposes the bound address, graceful shutdown and a
  join handle; the camera supervisor and TLS/HTTPS setup moved out of
  `main.rs`.
- Phase 4B: Tauri 2 desktop crate at `desktop/src-tauri`. It starts the same
  runtime on `127.0.0.1:0`, opens the UI in a webview at the bound address, and
  shuts the server down on exit.
- CI: added a Linux `desktop` job that installs WebKitGTK/GTK dependencies and
  runs `cargo check`/`clippy` for `thermolink-desktop`.
- macOS: added `NSLocalNetworkUsageDescription`, `signingIdentity: "-"` so
  Tauri ad-hoc signs every bundle, and desktop file logging. The linker-signed
  debug bundle left `Info.plist` unbound and made macOS ignore Local Network
  permission; proper bundle signing fixed it. `desktop/sign-macos-dev.sh` is a
  fallback for other builds.
- Desktop now honors `THERMOLINK_FFMPEG` and checks
  `Contents/Resources/ffmpeg`, enabling a bundled static ffmpeg. Verified the
  Qianli thermal and visible streams with a local static build; that test
  binary is GPL/nonfree and must not be redistributed.
- Added `desktop/build-lgpl-ffmpeg.sh` and `desktop/package-macos-dev.sh` to
  build and bundle an LGPL static FFmpeg reproducibly; the binary stays
  git-ignored, with its license copied into the app bundle.
- Bundle LGPL FFmpeg on every desktop target: BtbN LGPL builds for Linux and
  Windows in CI, source build for macOS. Tauri resources include `resources/*`
  and the app resolves `ffmpeg`/`ffmpeg.exe` from the resource root or
  `resources/` subdirectory.
- Camera UI: Auto-detect is now explicitly UVC-only; Qianli users are pointed at
  Discover cameras.
- The release workflow now builds unsigned desktop bundles on Linux, macOS and
  Windows and uploads them as workflow artifacts. Attaching those bundles to
  the GitHub release and code signing remain follow-ups; macOS/Windows are
  unvalidated, and Linux is the reference platform.

## Unreleased — 0.5.0 (M1)

- New unified `src/camera/uvc_radiometric/` family: one driver, data-only
  profiles, and a shared frame-layout/radiometry contract.
- M1 profiles: `topdon_tc001`, `infiray_p2pro` (256×192, split YUYV, 1/64 K)
  and `uvc_generic` (best-effort raw stream, uncalibrated).
- Detection: explicit `camera.json` profile → VID/PID + product string →
  ambiguity → generic fallback. `0bda:5830` is reported as ambiguous, never
  silently assigned to the TC001 or P2 Pro.
- New `radiometric` capability separates “thermal stream exists” from
  “temperature is calibrated”. `uvc_generic` sets it false and the UI hides
  measurement.
- API: `/api/drivers` lists profiles; `/api/camera/detect` returns candidates;
  `/api/camera/select` accepts `profile` and `device`; `/api/spec` includes
  profile/device metadata.
- Architecture: server discovery no longer imports the Qianli protocol; the
  driver trait provides discovery/detection. Added an architecture test that
  fails if `src/server/` imports a concrete driver.
- Tests: offline split/packed layout fixtures, ambiguity/fallback detection,
  legacy `camera.json` compatibility.
- All new UVC profiles are `tested: false`. No hardware support is claimed.

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
