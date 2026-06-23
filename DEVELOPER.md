# Developer guide

This file documents the build and CI contract for `loopctl`.
`CONTRIBUTING.md` covers the human side (branch naming, commit style,
PR process); this one is about the toolchain and the gates the build
system has to enforce.

## Toolchain

- Rust stable, edition 2024 (requires Cargo 1.85+).
- Linux x86_64 / aarch64 for the Konsole backend; macOS x86_64 / aarch64
  for the iTerm2 backend. Other platforms compile but have no native
  terminal plugin.
- D-Bus (`libdbus-1`) headers are needed at build time on Linux for the
  Konsole plugin's zbus dependency.

## Local checks (run before pushing)

```sh
cargo build
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

## CI enforcements

`loopctl` does not currently ship a CI configuration in this
repository. Whichever CI provider you wire up — SFCI Managed
(`.strata.yml`), GitHub Actions, or another — must enforce the gates
below to keep parity with what the maintainers run locally.

| Gate | Tool | Failure mode |
|---|---|---|
| Build | `cargo build --release` | compile error |
| Unit + integration tests | `cargo test --all-targets` | any test fails |
| Lint | `cargo clippy --all-targets -- -D warnings` | any warning |
| Format | `cargo fmt --check` | any unformatted file |
| License allow-list | `cargo deny check licenses` | dep with disallowed SPDX |
| Yanked / advisory check | `cargo deny check advisories` | yanked dep or RustSec advisory |
| Source allow-list | `cargo deny check sources` | dep from non-crates.io / non-allowed git |
| RustSec audit | `cargo audit` | known CVE in `Cargo.lock` |
| Notice freshness | `cargo about generate about.hbs -o /tmp/notices.md && diff -u THIRD_PARTY_NOTICES.md /tmp/notices.md` | `THIRD_PARTY_NOTICES.md` out of date |

Configuration files for these gates already live in the repo:

- `deny.toml` — license allow-list (mirrors `about.toml`), yanked-dep
  policy, source allow-list.
- `about.toml` + `about.hbs` — `cargo-about` config + Markdown template
  used to generate `THIRD_PARTY_NOTICES.md`.

## Regenerating `THIRD_PARTY_NOTICES.md`

After any change to `Cargo.toml` or `Cargo.lock`:

```sh
cargo install --locked cargo-about  # one time
cargo about generate about.hbs -o THIRD_PARTY_NOTICES.md
```

The notice-freshness CI gate will fail the build if you forget.

## Why no CI config in the tree?

The CI pipeline is added at OSS-publish time, not while the repo is
under internal development. If you fork or mirror this repo and want
CI today, port the gate list above into your provider's config file
(e.g. `.github/workflows/ci.yml` for Actions). The gate list is the
contract; the workflow file is just one implementation.
