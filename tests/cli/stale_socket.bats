#!/usr/bin/env bats
# Regression: a socket *file* left behind by a crashed/killed daemon must
# not be mistaken for a live daemon. Bare `loopctl` used to short-circuit
# on `socket.exists()` and print "already running", wedging the documented
# auto-spawn path until the stale file was removed by hand. It now probes
# the socket with a real connect and treats an unconnectable path as dead.

load helpers

@test "bare loopctl treats a stale socket as dead, not a running daemon" {
    # Fabricate a stale AF_UNIX socket: bind then close, leaving the file
    # on disk with nothing accepting connections (ECONNREFUSED on connect).
    local sock="$BATS_TEST_TMPDIR/stale.sock"
    python3 -c "import socket; s=socket.socket(socket.AF_UNIX); s.bind('$sock'); s.close()"
    [ -S "$sock" ]

    # With no config, the run must fall THROUGH the stale socket (rather
    # than reporting "already running") and hit the config pre-flight.
    run loopctl --config "$BATS_TEST_TMPDIR/no-such.toml" --socket "$sock"
    [[ "$output" != *"already running"* ]] \
        || (echo "false 'already running' on a stale socket: $output"; false)
    [ "$status" -ne 0 ]
    [[ "$output" == *"workbench.toml not found"* ]]
}
