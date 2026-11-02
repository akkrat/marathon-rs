# Run-flow probes (WP 3.10)

- `capture_scenarios.py [OUT]`: runs the 13 core scenario inputs (`tools/oracle/scenarios`) through the reference build's
  `oracle scenario` driver and condenses each transcript into `fixtures/oracle/run-flow/scenarios.jsonl`. The scripted
  inputs (`scripted`, `scripted-failures`; `entry: run`) run on the patched build with `scenario-tape.json`.
- `run [OUT_DIR]`: compiles `RunFlowProbes.kt` against the reference distribution's jars (Kotlin 1.9.24 from the Gradle
  cache), runs every case in a fresh JVM and writes `probes.jsonl` and `parse-command.jsonl`. The signal rows start
  `signal <debug>`, wait for `PROBE ready`, send SIGINT or SIGTERM and record the exit status and the finish calls.

Both need `tools/oracle/build.sh reference` and the reference JBR 21.0.10. Rust consumers:
`crates/marathon-core/tests/{run_flow_oracle,run_flow_witnesses,parse_command}.rs`.
