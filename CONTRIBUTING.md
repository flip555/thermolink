# Contributing

## Before you start

- Open an issue for behaviour changes and new drivers.
- Keep camera-specific code inside `src/camera/<family>/`; `src/server/` must not
  import a concrete driver.

## Checks

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo build --release
```

CI runs the same four commands. Driver work should include parser/framing tests;
byte captures make the best fixtures.

## Pull requests

- One logical change per PR.
- Describe the hardware/driver tested and the commands run.
- Do not commit `thermolink-data/`, snapshots, captures, tokens or TLS keys.
- By contributing you agree your work is licensed under the repository's MIT
  licence.
