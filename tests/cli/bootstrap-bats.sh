#!/usr/bin/env bash
# Download + extract a pinned BATS release for shell-level CLI tests.
# Idempotent: skips work if the cached install already exists.
#
# Print the path to the bats binary on stdout. Designed to be invoked
# from the Rust integration-test wrapper:
#
#   let bats = Command::new("tests/cli/bootstrap-bats.sh").output()?;
#   let bin = String::from_utf8(bats.stdout)?.trim();
#   Command::new(bin).args(...).status()?;

set -euo pipefail

VERSION="1.11.0"
# SHA-256 of https://github.com/bats-core/bats-core/archive/refs/tags/v1.11.0.tar.gz
# Update both VERSION and EXPECTED_SHA256 in lockstep when bumping BATS.
EXPECTED_SHA256="aeff09fdc8b0c88b3087c99de00cf549356d7a2f6a69e3fcec5e0e861d2f9063"
CACHE_ROOT="$(cd "$(dirname "$0")" && pwd)/.bats-cache"
INSTALL_DIR="$CACHE_ROOT/bats-$VERSION"
BATS_BIN="$INSTALL_DIR/bin/bats"

if [[ -x "$BATS_BIN" ]]; then
    echo "$BATS_BIN"
    exit 0
fi

mkdir -p "$CACHE_ROOT"
TMP_TGZ="$CACHE_ROOT/bats-core-$VERSION.tar.gz"
URL="https://github.com/bats-core/bats-core/archive/refs/tags/v${VERSION}.tar.gz"

# Allow offline runs by setting BATS_OFFLINE_TARBALL to a pre-fetched copy.
if [[ -n "${BATS_OFFLINE_TARBALL:-}" && -f "$BATS_OFFLINE_TARBALL" ]]; then
    cp "$BATS_OFFLINE_TARBALL" "$TMP_TGZ"
else
    if ! command -v curl >/dev/null 2>&1; then
        echo "bootstrap-bats: curl is required to fetch BATS $VERSION" >&2
        exit 1
    fi
    curl --fail --silent --show-error --location "$URL" -o "$TMP_TGZ"
fi

# Verify the tarball before extracting. On mismatch, leave the file in
# place so the operator can inspect it; non-zero exit aborts the test run.
if ! command -v sha256sum >/dev/null 2>&1; then
    echo "bootstrap-bats: sha256sum is required to verify BATS tarball" >&2
    exit 1
fi
if ! echo "$EXPECTED_SHA256  $TMP_TGZ" | sha256sum -c - >/dev/null; then
    actual="$(sha256sum "$TMP_TGZ" | awk '{print $1}')"
    echo "bootstrap-bats: SHA-256 mismatch for $TMP_TGZ" >&2
    echo "  expected: $EXPECTED_SHA256" >&2
    echo "  actual:   $actual" >&2
    echo "  tarball left at $TMP_TGZ for forensics" >&2
    exit 1
fi

EXTRACT_PARENT="$CACHE_ROOT/extract-$VERSION"
rm -rf "$EXTRACT_PARENT"
mkdir -p "$EXTRACT_PARENT"
tar -xzf "$TMP_TGZ" -C "$EXTRACT_PARENT" --strip-components=1
mv "$EXTRACT_PARENT" "$INSTALL_DIR"
rm -f "$TMP_TGZ"

if [[ ! -x "$BATS_BIN" ]]; then
    echo "bootstrap-bats: $BATS_BIN not found after extraction" >&2
    exit 1
fi

echo "$BATS_BIN"
