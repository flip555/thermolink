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

## Branch and release policy

- **`main` is the release branch.** Do not push feature work to it.
- **`dev` is the integration branch.** Work on a feature branch, open a PR into
  `dev`, and merge it there. CI, CodeQL and the security audit run on pushes to
  `dev`; they do not run on individual feature branches or PRs into `dev`.
- Keep `dev` green. If a merge breaks it, fix forward on `dev`; `main` remains
  unaffected.
- **Release:** open a PR from `dev` into `main` with the version bump. Checks
  run on that PR, then merge and push a `v*` tag. Release artifacts are built
  only from `v*` tags by `release.yml`.
- **Docs-only changes** (README, `docs/`, screenshots, wiki source, Markdown,
  issue templates) can go directly to `main`; they are path-ignored by CI.
  Do not use a docs PR for these.
- `desktop-package.yml` is a manual (`workflow_dispatch`) helper. Do not add
  push triggers that build installers for routine changes; they consume GitHub
  Actions minutes.
