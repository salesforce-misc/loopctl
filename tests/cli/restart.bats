#!/usr/bin/env bats
# `loopctl restart` smoke. The full restart path requires a real
# daemon (and would touch the developer's actual KDE/Konsole world),
# so we limit BATS coverage to the pre-flight failure modes that
# don't spawn anything.

load helpers

@test "restart with no config refuses cleanly without spawning" {
    run loopctl --config "$BATS_TEST_TMPDIR/no-such.toml" restart
    [ "$status" -ne 0 ]
    [[ "$output" == *"workbench.toml not found"* ]] \
        || (echo "expected pre-flight error in: $output"; false)
    # Pre-flight failure must NOT have created a runtime dir or log.
    [ ! -e "$BATS_TEST_TMPDIR/runtime/agent-loop-workbench.sock" ]
}

@test "restart --help mentions --up flag" {
    run loopctl restart --help
    [ "$status" -eq 0 ]
    [[ "$output" == *"--up"* ]]
    [[ "$output" == *"Also call"* ]]
}

@test "restart with invalid config refuses with parser diagnostic" {
    write_workbench '
schema_version = 1

[workbench]
claude_loops_dir = "~/x"
terminal = "tmux"
central_group = "1"

[[loops]]
name = "alpha"
dir = "alpha"
readme_anchor = "alpha"
group_key = "7"
schedule = "none"
dispatch = "resume_prompt"
'
    run loopctl --config "$BATS_TEST_TMPDIR/workbench.toml" restart
    [ "$status" -ne 0 ]
    # Updated wording after the iterm2 backend landed: error now lists
    # all valid terminal names rather than a "v1 only" qualifier.
    [[ "$output" == *"not supported"* ]]
    [ ! -e "$BATS_TEST_TMPDIR/runtime/agent-loop-workbench.sock" ]
}
