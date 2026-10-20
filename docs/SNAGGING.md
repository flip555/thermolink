# Snagging list

Last updated: 2026-10-20

## Open

- [ ] **Desktop re-analysis locks the visible feed**
  - Repro: open a snapshot, Exit, then open a second snapshot before the live
    video feed has recovered.
  - Windows: the visible video stays frozen.
  - macOS: the visible video freezes and the Snapshot action stops responding.
  - Deferred for now; needs a reset/serialization fix around re-analysis entry
    and visible-stream recovery.

- [ ] **Linux desktop 0.4.1 pass**
  - The Linux release assets pass checksum/package verification; still needs the
    interactive desktop smoke test.

- [ ] **CodeQL `write_atomic` alerts**
  - Alerts #1–#6 may remain after the path-validation fixes. All callers pass
    fixed literals and the name is validated; dismiss as false positives once
    the post-0.4.1 CodeQL analysis publishes if they remain.

## Fixed in 0.4.1

- Qianli camera-IP field replaces the unsupported `window.prompt` in WKWebView.
- °C/°F applies to peak labels and the history chart.
- Visible-stream reset/retry after re-analysis, focus, and blank-stream timeout.
- Header quick controls for ΔT reference and thermal opacity.
- Isotherm presets: body, electronics, building, fire.
- Build SHA in `/api/version`; unsupported desktop update controls hidden.
- Install/run docs, unsigned-build guidance, and release checksums.
- Security: TLS advisories, stored-file path validation, test-only passwords.
