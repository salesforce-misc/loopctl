#!/usr/bin/env bats
# `loopctl init` validates a workbench.toml without side effects.

load helpers

setup() {
    CFG="$BATS_TEST_TMPDIR/workbench.toml"
}

@test "init: valid minimal config exits 0 and lists the loops" {
    write_workbench '
schema_version = 1

[workbench]
claude_loops_dir = "~/claude-loops"
terminal = "konsole"
central_group = "1"

[[loops]]
name = "alpha"
dir = "alpha"
readme_anchor = "alpha"
group_key = "7"
schedule = "every 10m"
dispatch = "tick_script"
tick_script = "skill/scripts/tick.py"
'
    run loopctl --config "$CFG" init
    [ "$status" -eq 0 ]
    [[ "$output" == *"alpha"* ]]
    [[ "$output" == *"every 10m"* ]]
    [[ "$output" == *"TickScript"* ]]
}

@test "init: legacy slug field still parses (serde alias compat)" {
    write_workbench '
schema_version = 1

[workbench]
claude_loops_dir = "~/x"
terminal = "konsole"
central_group = "1"

[[loops]]
slug = "legacy"
dir = "legacy"
readme_anchor = "legacy"
group_key = "7"
schedule = "none"
dispatch = "resume_prompt"
'
    run loopctl --config "$CFG" init
    [ "$status" -eq 0 ]
    [[ "$output" == *"legacy"* ]]
}

@test "init: duplicate name fails with the must-be-unique error" {
    write_workbench '
schema_version = 1

[workbench]
claude_loops_dir = "~/x"
terminal = "konsole"
central_group = "1"

[[loops]]
name = "alpha"
dir = "alpha"
readme_anchor = "alpha"
group_key = "7"
schedule = "none"
dispatch = "resume_prompt"

[[loops]]
name = "alpha"
dir = "alpha2"
readme_anchor = "alpha2"
group_key = "8"
schedule = "none"
dispatch = "resume_prompt"
'
    run loopctl --config "$CFG" init
    [ "$status" -ne 0 ]
    [[ "$output" == *"duplicate loop name"* ]]
    [[ "$output" == *"alpha"* ]]
    [[ "$output" == *"must be unique"* ]]
}

@test "init: empty name is rejected" {
    write_workbench '
schema_version = 1

[workbench]
claude_loops_dir = "~/x"
terminal = "konsole"
central_group = "1"

[[loops]]
name = ""
dir = "x"
readme_anchor = "x"
group_key = "7"
schedule = "none"
dispatch = "resume_prompt"
'
    run loopctl --config "$CFG" init
    [ "$status" -ne 0 ]
    [[ "$output" == *"empty name"* ]]
}

@test "init: unknown terminal is rejected with not-supported message" {
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
    run loopctl --config "$CFG" init
    [ "$status" -ne 0 ]
    [[ "$output" == *"not supported"* ]]
}

@test "init: bad schedule string is rejected" {
    write_workbench '
schema_version = 1

[workbench]
claude_loops_dir = "~/x"
terminal = "konsole"
central_group = "1"

[[loops]]
name = "alpha"
dir = "alpha"
readme_anchor = "alpha"
group_key = "7"
schedule = "hourly"
dispatch = "resume_prompt"
'
    run loopctl --config "$CFG" init
    [ "$status" -ne 0 ]
    [[ "$output" == *"schedule"* ]] \
        || (echo "expected schedule diagnostic; got:"; echo "$output"; false)
}

@test "init: tick_script dispatch missing tick_script field is rejected" {
    write_workbench '
schema_version = 1

[workbench]
claude_loops_dir = "~/x"
terminal = "konsole"
central_group = "1"

[[loops]]
name = "alpha"
dir = "alpha"
readme_anchor = "alpha"
group_key = "7"
schedule = "every 10m"
dispatch = "tick_script"
'
    run loopctl --config "$CFG" init
    [ "$status" -ne 0 ]
    [[ "$output" == *"requires a"* ]]
    [[ "$output" == *"tick_script"* ]]
}

@test "init: missing config file produces a clear error" {
    run loopctl --config "$BATS_TEST_TMPDIR/no-such.toml" init
    [ "$status" -ne 0 ]
    [[ "$output" == *"read workbench.toml"* ]] \
        || [[ "$output" == *"No such file"* ]] \
        || (echo "expected file-not-found in: $output"; false)
}
