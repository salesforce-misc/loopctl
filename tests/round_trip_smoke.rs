// Copyright (c) 2026, Salesforce, Inc.
// SPDX-License-Identifier: Apache-2.0
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

#[test]
fn round_trip_preserves_tick_script_and_schedule() {
    let body = r#"
schema_version = 1

[workbench]
claude_loops_dir = "/x"
terminal = "konsole"
central_group = "1"

[[loops]]
name = "alpha-loop"
dir = "alpha-loop"
readme_anchor = "alpha-loop--foo-bar-perf-tuning"
group_key = "7"
schedule = "every 10m"
dispatch = "tick_script"
tick_script = "skill/scripts/tick.py"
tick_args = ["--cache", "/tmp/x.json"]
"#;
    let cfg = agent_loop_workbench::config::parse(body).unwrap();
    let out = toml::to_string_pretty(&cfg).unwrap();
    eprintln!("---round-tripped output---\n{}", out);
    let cfg2 = agent_loop_workbench::config::parse(&out).unwrap();
    assert_eq!(cfg.loops[0].schedule, cfg2.loops[0].schedule);
    assert_eq!(cfg.loops[0].dispatch, cfg2.loops[0].dispatch);
    assert_eq!(cfg.loops[0].tick_script, cfg2.loops[0].tick_script);
    assert_eq!(cfg.loops[0].tick_args, cfg2.loops[0].tick_args);
}