#!/usr/bin/env bats
# Smoke: --help and --version exit cleanly and list every subcommand.

load helpers

@test "loopctl --help exits 0 and lists every subcommand" {
    run loopctl --help
    [ "$status" -eq 0 ]
    # Sanity-check every subcommand we ship.
    for cmd in init daemon up down tick status attach daemon-stop tui \
               recover reload restart add-loop delete-loop set-repo load-repo \
               close reopen update-prompt; do
        echo "checking for: $cmd"
        echo "$output" | grep -qE "^[[:space:]]+$cmd[[:space:]]" \
            || (echo "missing subcommand: $cmd"; echo "---output---"; echo "$output"; false)
    done
}

@test "loopctl --version prints something matching Cargo.toml" {
    run loopctl --version
    [ "$status" -eq 0 ]
    [[ "$output" =~ ^loopctl[[:space:]] ]]
    # Pull the version field from Cargo.toml and assert it appears.
    local crate_version
    crate_version="$(grep -E '^version = ' \
        "$(dirname "${BASH_SOURCE[0]}")/../../Cargo.toml" \
        | head -1 | sed -E 's/^version = "([^"]+)"/\1/')"
    [[ "$output" == *"$crate_version"* ]]
}

@test "no-subcommand requires a daemon path; exits non-zero with helpful message when config is missing" {
    # We give it a config path that doesn't exist; bare `loopctl` should
    # validate and refuse to spawn rather than silently failing.
    run loopctl --config "$BATS_TEST_TMPDIR/no-such.toml"
    [ "$status" -ne 0 ]
    [[ "$output" == *"workbench.toml not found"* ]] \
        || (echo "expected 'workbench.toml not found' in: $output"; false)
}

@test "every subcommand has a --help that exits 0" {
    for cmd in init up down tick status attach daemon-stop tui recover \
               reload restart add-loop delete-loop set-repo load-repo close reopen update-prompt; do
        run loopctl "$cmd" --help
        [ "$status" -eq 0 ] \
            || (echo "$cmd --help exit=$status output=$output"; false)
    done
}
