#!/usr/bin/env bash
# WP 7.5 closure sweep: the orchestrator's steps for one profile (local-27 on the Mac, linux-kvm on the worker).
#
#   MRS_HOST   absolute path of the git-ignored host config of this profile (local.json / the worker's host.json)
#   MRS_RUNS   absolute path of this sweep's NEW runs root
#   MRS_OLD    absolute path of the retained 7.2 runs root (read only; selections and r3 policies are starting points)
#
#   sweep.sh gates                      offline validation and tests
#   sweep.sh reference                  (Mac) release JVM reference through build.sh + receipt, native binary
#   sweep.sh build [DIR]                preflight and release-bound sample builds of the profile; resumable per directory
#                                       (ios-build, macos-build, android-build), DIR limits it to one
#   sweep.sh prepare SCENARIO           stage inputs, JVM discovery from the release reference
#   sweep.sh selection SCENARIO REVIEW  selection from the 7.2 one, re-bound to the new discovery
#   sweep.sh jvm SCENARIO REVIEW        draft from the 7.2 r3 policy, two JVM pairs
#   sweep.sh jvmpair SCENARIO N         one more JVM capture into pair N (macos-cancel's extra A99 pairs)
#   sweep.sh qualify SCENARIO           JVM/JVM qualification of pairs 1 and 2 (needs ffprobe/ffmpeg; offline, any machine)
#   sweep.sh native SCENARIO N          native capture of pair N (write the witnesses afterwards)
#   sweep.sh compare SCENARIO N         compare --release of pair N (offline), into a fresh mismatch directory
#   sweep.sh audit                      every capture and build record names the release source and the native commit the
#                                       build records name (not the current HEAD)
#
# Every step stops on the first failure, including a failed load guard or a leftover after a capture.
set -euo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../../.." && pwd)
MRS_RUN_TOOL=${MRS_RUN_TOOL:-$ROOT/tools/real-e2e/run}
# shellcheck source=common.sh
. "$HERE/common.sh"

if [ -z "${MRS_PY:-}" ]; then
  if [ -x /opt/homebrew/bin/python3.12 ]; then MRS_PY=/opt/homebrew/bin/python3.12; else MRS_PY=python3; fi
fi
export MRS_PY MRS_RUN_TOOL
plan() { "$MRS_PY" -I "$HERE/plan.py" "$@"; }
rt() { "$MRS_PY" -I "$MRS_RUN_TOOL" "$@"; }

: "${MRS_HOST:?MRS_HOST is required}" "${MRS_RUNS:?MRS_RUNS is required}"
sweep_need_abs MRS_HOST "$MRS_HOST"; sweep_need_abs MRS_RUNS "$MRS_RUNS"
SRC=$(tr -d '[:space:]' < "$ROOT/docs/release-source.sha")
NATIVE=$(git -C "$ROOT" rev-parse HEAD)

# One native commit for the whole sweep: capture.json records HEAD, so a later commit would split it.
if [ -z "${MRS_ALLOW_DIRTY:-}" ] && [ -n "$(git -C "$ROOT" status --porcelain)" ]; then
  sweep_die "the sweep worktree has uncommitted changes; the native commit must be one clean HEAD"
fi

capture() { sweep_capture "$MRS_HOST" "$@"; }   # guard, capture-pair --approved, leftover check; fails with their status

step=${1:-}; [ -n "$step" ] || sweep_die "usage: sweep.sh STEP [ARGS] (see the header)"; shift
case "$step" in
  gates)
    rt validate
    "$MRS_PY" -I "$ROOT/tools/real-e2e/test_real_e2e.py"
    "$MRS_PY" -I "$ROOT/tools/real-e2e/test_outcomes.py"
    ;;
  reference)
    sweep_need_abs MRS_RUNS "$MRS_RUNS"; mkdir -p "$MRS_RUNS"
    [ -n "${JAVA_HOME:-}" ] || sweep_die "JAVA_HOME (the JBR 21.0.10) is required"
    ORACLE_SOURCE_SHA=$SRC "$ROOT/tools/oracle/build.sh" reference
    clone=$ROOT/tools/oracle/build-$SRC/marathon-src
    rt reference --source release --oracle-clone "$clone" --jvm "$clone/cli/build/install/marathon/bin/marathon" \
       --output "$MRS_RUNS/reference.json"
    (cd "$ROOT" && cargo build --release --locked -p marathon-cli --bin marathon)
    shasum -a 256 "$ROOT/target/release/marathon" "$clone"/cli/build/install/marathon/lib/*.jar > "$MRS_RUNS/binaries.sha256"
    echo "release=$SRC native=$NATIVE"
    ;;
  build)
    # Resumable per platform: a build directory that already holds its build.json is only checked, so the macOS part
    # can be run after the iOS one (or vice versa). `build DIR` (e.g. macos-build) limits the step to one directory.
    mkdir -p "$MRS_RUNS"
    only=${1:-}; matched=0
    profile=$(plan profile "$MRS_HOST")
    rows=$(plan builds "$profile")   # a failing plan stops the step here, which a process substitution would not
    [ -n "$rows" ] || sweep_die "no builds are planned for profile $profile"
    while read -r dir vendor first <&3; do
      if [ -n "$only" ] && [ "$only" != "$dir" ]; then continue; fi
      matched=1
      if [ -e "$MRS_RUNS/$dir/build.json" ]; then
        echo "build: $dir is already built; checking its record"
      else
        [ ! -e "$MRS_RUNS/$dir" ] || sweep_die "$MRS_RUNS/$dir exists without a build record (an interrupted build): move it aside, retained, and re-run"
        if [ -e "$MRS_RUNS/preflight-$dir/preflight.json" ]; then
          echo "build: preflight-$dir already passed"
        else
          [ ! -e "$MRS_RUNS/preflight-$dir" ] || sweep_die "$MRS_RUNS/preflight-$dir exists without preflight.json (a blocked preflight): move it aside, retained, and re-run"
          rt preflight --host "$MRS_HOST" --scenario "$first" --output "$MRS_RUNS/preflight-$dir" --approved < /dev/null
        fi
        rt build --host "$MRS_HOST" --vendor "$vendor" --source release --output "$MRS_RUNS/$dir" --approved < /dev/null
      fi
      plan check-build "$MRS_RUNS/$dir/build.json" "$SRC"
    done 3<<< "$rows"
    [ -z "$only" ] || [ "$matched" = 1 ] || sweep_die "no build directory $only for this profile"
    ;;
  prepare)
    s=${1:?scenario}; inputs=$MRS_RUNS/$s-inputs
    rt stage-inputs --source release --build "$MRS_RUNS/$(plan get "$s" build)" --application "$(plan get "$s" application)" \
       --tests "$(plan get "$s" tests)" --output "$inputs"
    capture --scenario "$s" --side jvm --discover --source release --inputs "$inputs" --output "$MRS_RUNS/$s-discovery"
    cp "$MRS_RUNS/$s-discovery/discovery/in/report/discovery.yml" "$inputs/discovery.yml"
    echo "discovery sha256: $(sweep_sha256 "$inputs/discovery.yml")"
    echo "Review it against the 7.2 one, then: sweep.sh selection $s '<review text>'"
    ;;
  selection)
    s=${1:?scenario}; review=${2:?review text}; sweep_need_abs MRS_OLD "${MRS_OLD:-}"
    plan selection --old "$MRS_OLD/$(plan get "$s" old_selection)" --inputs "$MRS_RUNS/$s-inputs" --review "$review"
    ;;
  jvm)
    s=${1:?scenario}; review=${2:?review text}; sweep_need_abs MRS_OLD "${MRS_OLD:-}"
    plan draft --old "$MRS_OLD/$(plan get "$s" old_draft)" --out "$MRS_RUNS/$s-draft.json" --review "$review"
    for n in 1 2; do
      capture --scenario "$s" --side jvm --source release --inputs "$MRS_RUNS/$s-inputs" --output "$MRS_RUNS/$s-pair-$n"
    done
    ;;
  jvmpair)
    s=${1:?scenario}; n=${2:?pair number}
    capture --scenario "$s" --side jvm --source release --inputs "$MRS_RUNS/$s-inputs" --output "$MRS_RUNS/$s-pair-$n"
    ;;
  qualify)
    s=${1:?scenario}
    rt qualify --reference "$MRS_RUNS/$s-pair-1" --rerun "$MRS_RUNS/$s-pair-2" --draft "$MRS_RUNS/$s-draft.json" \
       --output "$MRS_RUNS/$s-outcomes.json" --mismatch "$MRS_RUNS/$s-jvm-mismatch-1"
    ;;
  native)
    s=${1:?scenario}; n=${2:?pair number}
    capture --scenario "$s" --side native --source release --inputs "$MRS_RUNS/$s-inputs" --output "$MRS_RUNS/$s-pair-$n"
    ;;
  compare)
    s=${1:?scenario}; n=${2:?pair number}; k=1
    while [ -e "$MRS_RUNS/$s-native-mismatch-$n-$k" ]; do k=$((k + 1)); done
    rt compare "$MRS_RUNS/$s-pair-$n" --release --policy "$MRS_RUNS/$s-outcomes.json" --mismatch "$MRS_RUNS/$s-native-mismatch-$n-$k"
    ;;
  audit)
    plan audit --root "$MRS_RUNS" --source "$SRC"
    sweep_leftovers
    ;;
  *) sweep_die "unknown step: $step" ;;
esac
