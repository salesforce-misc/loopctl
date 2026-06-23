#!/usr/bin/env bats
# Pin clap's required-arg / xor enforcement so regressions surface here
# instead of as confused user reports.

load helpers

@test "bare add-loop lists name, group, AND prompt source as required" {
    run loopctl add-loop
    [ "$status" -ne 0 ]
    [[ "$output" == *"--name <NAME>"* ]]
    [[ "$output" == *"--group <GROUP>"* ]]
    # The xor for --message / --prompt-file must show in the usage line.
    [[ "$output" == *"--message <MESSAGE>"* ]]
    [[ "$output" == *"--prompt-file <PROMPT_FILE>"* ]]
}

@test "add-loop with name+group but no prompt source fails with prompt requirement" {
    run loopctl add-loop --name foo --group 7
    [ "$status" -ne 0 ]
    [[ "$output" == *"--message"* ]]
    [[ "$output" == *"--prompt-file"* ]]
}

@test "add-loop refuses --message and --prompt-file together" {
    local f="$BATS_TEST_TMPDIR/p.txt"
    echo body > "$f"
    run loopctl add-loop --name foo --group 7 --message body --prompt-file "$f"
    [ "$status" -ne 0 ]
    [[ "$output" == *"cannot be used with"* ]]
}

@test "bare update-prompt requires both NAME and a prompt source" {
    run loopctl update-prompt
    [ "$status" -ne 0 ]
    [[ "$output" == *"<NAME>"* ]]
    [[ "$output" == *"--message <MESSAGE>"* ]]
    [[ "$output" == *"--prompt-file <PROMPT_FILE>"* ]]
}

@test "update-prompt with name only fails with prompt requirement" {
    run loopctl update-prompt foo
    [ "$status" -ne 0 ]
    [[ "$output" == *"--message"* ]]
}

@test "tick / attach / close / reopen all need a name" {
    for cmd in tick attach close reopen; do
        run loopctl "$cmd"
        [ "$status" -ne 0 ] \
            || (echo "$cmd should have errored without name"; false)
        [[ "$output" == *"<NAME>"* ]] \
            || (echo "$cmd missing <NAME> hint:"; echo "$output"; false)
    done
}

@test "unknown subcommand exits non-zero with a clap-style error" {
    run loopctl not-a-real-subcommand
    [ "$status" -ne 0 ]
    # clap's wording across versions: "unrecognized subcommand" or
    # "found argument" — accept either.
    [[ "$output" == *"unrecognized subcommand"* || "$output" == *"unexpected argument"* ]]
}
