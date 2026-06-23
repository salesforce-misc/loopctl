# Contributing to loopctl

Thanks for your interest in `loopctl`. This document captures the
conventions the maintainers follow so PRs land cleanly.

## Governance Model

### Published but not supported

This project is open sourced because it may contain useful or
interesting code and concepts that we wish to share with the larger
open source community. Although occasional work may be done on it, the
maintainers will not be actively looking for or soliciting
contributions. Issues and pull requests will be reviewed on a
best-effort basis.

## Licensing of contributions

`loopctl` is licensed under Apache-2.0 (see `LICENSE.txt`). By
submitting a contribution you agree that your contribution is licensed
under the same terms.

## Branches and PRs

- Branch from `main`. Use `feature/<topic>` for new functionality and
  `fix/<topic>` for bug fixes.
- One logical change per PR. If you need to refactor as a setup step,
  ship the refactor in its own PR first.
- Every PR description should include a short **Description** section
  explaining the *why*, and a **Test plan** section listing what you
  ran (or what a reviewer should run) to verify the change.

## Commit messages

- Subject line: imperative mood, lowercase, **72 characters or fewer**.
  Examples: `tests: add fixture for nested progress.md`,
  `daemon: chmod 0600 on the IPC socket`.
- Body: explain *why* the change is needed, not *what* the diff does
  (the diff already says what). Wrap at ~80 characters.
- Do not add `Co-Authored-By` or AI-tool footers in OSS commits — keep
  attribution clean.

## Required local checks

Before opening a PR run, at minimum:

```sh
cargo build
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

CI runs the same checks plus `cargo deny check`, `cargo audit`, and a
freshness check on `THIRD_PARTY_NOTICES.md`. See `DEVELOPER.md` for the
authoritative list of CI gates.

## Updating dependencies

Whenever you change `Cargo.toml` or `Cargo.lock`, regenerate the
third-party license notice file:

```sh
cargo install --locked cargo-about  # one time
cargo about generate about.hbs -o THIRD_PARTY_NOTICES.md
```

The notices-freshness CI gate will fail the build if
`THIRD_PARTY_NOTICES.md` is out of date.

## Tests

- Add unit tests next to the code in a `#[cfg(test)] mod tests` block.
- Add integration tests under `tests/`. Tests that require a real
  Konsole / iTerm2 must be marked `#[ignore]` and gated on an env var
  (see `tests/konsole_smoke.rs` for the pattern).
- The `tests/cli/` directory holds BATS tests for end-to-end CLI
  behavior. The bootstrap script downloads BATS pinned to a
  SHA-256 — bumping the version means updating both `VERSION` and
  `EXPECTED_SHA256`.

## Contributor License Agreement ("CLA")

In order to accept your pull request, we need you to submit a CLA. You
only need to do this once to work on any of Salesforce's open source
projects. Complete your CLA here: <https://cla.salesforce.com/sign-cla>.

By contributing your code, you agree to license your contribution under
the terms of the project [LICENSE](LICENSE.txt) and to sign the
Salesforce CLA above.

## Reporting security issues

Please do **not** file security issues as public GitHub issues. See
`SECURITY.md` for the private reporting process.
