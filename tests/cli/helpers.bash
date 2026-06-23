#!/usr/bin/env bash
# Shared helpers for the BATS CLI test suites.
#
# Each test runs with HOME and XDG_CONFIG_HOME pointed at a fresh tempdir
# so it can't read or write the developer's real ~/.config/loopctl,
# ~/claude-loops, or ~/.local/state/agent-loop-workbench.

# Path to the loopctl binary built by `cargo build`. Override with
# LOOPCTL_BIN if you want to test a release binary.
loopctl_bin() {
    if [[ -n "${LOOPCTL_BIN:-}" ]]; then
        echo "$LOOPCTL_BIN"
        return
    fi
    local manifest_dir
    manifest_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
    echo "$manifest_dir/target/debug/loopctl"
}

# Run loopctl with the test sandbox in effect. Captures stdout/stderr
# into BATS's `output` and `status` per the standard `run` convention.
loopctl() {
    HOME="$BATS_TEST_TMPDIR" \
    XDG_CONFIG_HOME="$BATS_TEST_TMPDIR/.config" \
    XDG_STATE_HOME="$BATS_TEST_TMPDIR/.local/state" \
    XDG_RUNTIME_DIR="$BATS_TEST_TMPDIR/runtime" \
    LOOPCTL_TEST_INHIBIT_USER_CONFIG=1 \
    "$(loopctl_bin)" "$@"
}

# Convenience: write a workbench.toml to $BATS_TEST_TMPDIR/workbench.toml.
write_workbench() {
    local body="$1"
    mkdir -p "$BATS_TEST_TMPDIR"
    printf '%s' "$body" > "$BATS_TEST_TMPDIR/workbench.toml"
}
