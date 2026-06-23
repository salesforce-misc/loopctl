#!/usr/bin/env bats
# `loopctl delete-loop` smoke. The full delete path requires a real
# daemon (touches state + KDE), so BATS coverage is limited to:
#   - --help renders
#   - bare invocation fails on missing NAME
#   - --yes flag is documented

load helpers

@test "delete-loop --help renders and mentions destructive + --yes" {
    run loopctl delete-loop --help
    [ "$status" -eq 0 ]
    [[ "$output" == *"Destructive"* || "$output" == *"destructive"* ]]
    [[ "$output" == *"--yes"* ]]
}

@test "bare delete-loop fails with NAME requirement" {
    run loopctl delete-loop
    [ "$status" -ne 0 ]
    [[ "$output" == *"<NAME>"* ]]
}
