#!/usr/bin/env bash
# Runs the Kotlin reference marathon against fake-adb for one scenario under fixtures/fake-adb/<name>
# (PLAN.md §6.4, WP 0.5 part B).
#
#   tools/fake-adb/run-scenario.sh fixtures/fake-adb/android-two-devices [--update]
#
# The scenario directory holds `in/` (the Marathonfile and anything else marathon reads; the fixture APKs from
# fixtures/vendor-android/apk are copied in), `script/devices.yaml` (the fake-adb script, run in strict mode),
# `expected-exit-code`, and `expected/`: the fake-adb transcript plus marathon's normalised outputs from
# tools/fake-adb/scenario_report.py (report-manifest.json, results.json, marathon.stdout, marathon.stderr).
#
# fake-adb starts first, on a free port that reaches the Marathonfile as ${FAKE_ADB_PORT}; marathon must find it
# up, or it would start a real adb server. The script fails if fake-adb fails (strict: any warning), if marathon's
# exit code differs from `expected-exit-code` (0 if there is none; marathon exits 1 when a test fails), or, without
# --update, if results.json or report-manifest.json differ from expected/. --update re-records expected/. The
# transcript and the logs are not compared: the two devices' requests and log lines interleave differently from run
# to run.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
SCENARIO="$(cd "$1" && pwd)"
UPDATE="${2:-}"
export PATH=/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH
JAVA_HOME="${JAVA_HOME:-/Applications/Android Studio.app/Contents/jbr/Contents/Home}"
MARATHON="$ROOT/tools/oracle/build/marathon-src/cli/build/install/marathon/bin/marathon"
[[ -x "$MARATHON" ]] || { echo "error: build the Kotlin reference first (tools/oracle/build.sh)" >&2; exit 2; }

cargo build -q -p fake-adb --manifest-path "$ROOT/Cargo.toml"
FAKE_ADB="$ROOT/target/debug/fake-adb"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/fake-adb-scenario.XXXXXX")"
mkdir "$WORK/in"
cp -R "$SCENARIO/in/." "$WORK/in/"
cp "$ROOT/fixtures/vendor-android/apk/app-debug.apk" "$ROOT/fixtures/vendor-android/apk/app-debug-androidTest.apk" "$WORK/in/"

"$FAKE_ADB" --script "$SCENARIO/script/devices.yaml" --port 0 --port-file "$WORK/port" \
    --transcript "$WORK/transcript.jsonl" --strict >"$WORK/fake-adb.out" 2>"$WORK/fake-adb.err" &
FAKE_PID=$!
trap 'kill -TERM $FAKE_PID 2>/dev/null || true' EXIT
for _ in $(seq 1 500); do [[ -s "$WORK/port" ]] && break; sleep 0.02; done
PORT="$(cat "$WORK/port")"

ID="fake-adb-$(basename "$SCENARIO")-$(date +%Y%m%d%H%M%S)"
set +e
"$ROOT/tools/oracle/run-isolated.sh" --id "$ID" --timeout 300 \
    --env "JAVA_HOME=$JAVA_HOME" --env "FAKE_ADB_PORT=$PORT" \
    -- "$MARATHON" "$WORK/in" -m @IN@/Marathonfile
RUN="$ROOT/tools/oracle/runs/$ID"
python3 -I "$ROOT/tools/fake-adb/scenario_report.py" "$RUN" "$PORT" "$WORK/normalised"
MARATHON_EXIT=$?
kill -TERM $FAKE_PID
wait $FAKE_PID
FAKE_EXIT=$?
set -e
trap - EXIT
echo "marathon exit: $MARATHON_EXIT; fake-adb exit: $FAKE_EXIT; run: $RUN; work: $WORK"
cat "$WORK/fake-adb.err" >&2

SAME=0
if [[ "$UPDATE" == "--update" ]]; then
    rm -rf "$SCENARIO/expected"
    mkdir -p "$SCENARIO/expected"
    cp "$WORK/transcript.jsonl" "$WORK/normalised/"* "$SCENARIO/expected/"
else
    for f in results.json report-manifest.json; do
        diff -u "$SCENARIO/expected/$f" "$WORK/normalised/$f" || SAME=1
    done
fi
EXPECTED_EXIT=0
[[ -f "$SCENARIO/expected-exit-code" ]] && EXPECTED_EXIT="$(cat "$SCENARIO/expected-exit-code")"
[[ $MARATHON_EXIT -eq $EXPECTED_EXIT && $FAKE_EXIT -eq 0 && $SAME -eq 0 ]]
