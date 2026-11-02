#!/usr/bin/env bash
# WP 7.5 closure sweep, Mac side of the linux-kvm profile: everything the worker needs, in one new directory.
#   linux-stage.sh STAGE_DIR      (MRS_HOST = local-27 host config, MRS_RUNS = its runs root holding reference.json,
#                                  MRS_OLD = the retained 7.2 runs root)
# The Kotlin checkout is only read. Copy STAGE_DIR to the worker over the approved channel afterwards.
set -euo pipefail
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../../.." && pwd)
# shellcheck source=common.sh
. "$HERE/common.sh"
MRS_PY=${MRS_PY:-/opt/homebrew/bin/python3.12}
STAGE=${1:?stage directory}; sweep_need_abs STAGE "$STAGE"
: "${MRS_HOST:?}" "${MRS_RUNS:?}" "${MRS_OLD:?}"
sweep_need_abs MRS_HOST "$MRS_HOST"; sweep_need_abs MRS_RUNS "$MRS_RUNS"; sweep_need_abs MRS_OLD "$MRS_OLD"
plan() { "$MRS_PY" -I "$HERE/plan.py" "$@"; }
SRC=$(tr -d '[:space:]' < "$ROOT/docs/release-source.sha")
FROZEN=e47352f25cbc50333900d5d040a5544ff1224501
K=$(plan host-get "$MRS_HOST" kotlin_source)
if [ -z "${MRS_ALLOW_DIRTY:-}" ] && [ -n "$(git -C "$ROOT" status --porcelain)" ]; then sweep_die "uncommitted changes in the sweep worktree"; fi
mkdir "$STAGE"

# The harness and native source: a bundle, so that the worker checkout's HEAD is exactly the sweep commit.
git -C "$ROOT" bundle create "$STAGE/checkout.bundle" HEAD

# The sample: one path-limited pack per commit, the same recipe for the frozen and the release commit (7.2 staged the
# frozen one the same way). Trees of the whole commit are needed by `git archive`, blobs only for the android sample.
pack() { { git -C "$K" rev-list --objects --no-walk --filter=blob:none "$1"
           git -C "$K" rev-list --objects "$1:sample/android-app"; } | git -C "$K" pack-objects --stdout --quiet; }
pack "$FROZEN" > "$STAGE/frozen.pack"
pack "$SRC" > "$STAGE/release.pack"
frozen_tree=$(git -C "$K" rev-parse "$FROZEN:sample/android-app")
release_tree=$(git -C "$K" rev-parse "$SRC:sample/android-app")
[ "$frozen_tree" = "$release_tree" ] || sweep_die "the android sample differs between the frozen and the release source"
echo "android sample tree (both): $release_tree"

# The JVM reference distribution and its record (the worker re-hashes the jars against the record at capture time).
clone=$ROOT/tools/oracle/build-$SRC/marathon-src
# macOS tar adds AppleDouble `._*` companions of files with extended attributes unless COPYFILE_DISABLE is set; the worker
# hashed them as jars and refused the distribution (7.5 sweep). Set it, and refuse an archive that has one anyway.
COPYFILE_DISABLE=1 tar -C "$clone/cli/build/install" -cf "$STAGE/jvm.tar" marathon
entries=$(tar -tf "$STAGE/jvm.tar") || sweep_die "cannot list $STAGE/jvm.tar"
sweep_reject_appledouble "$entries" "$STAGE/jvm.tar"
cp "$MRS_RUNS/reference.json" "$STAGE/reference.json"

# The 7.2 starting points (selections and r3 policies) of the worker's scenarios, with their relative paths.
for s in $(plan scenarios linux-kvm); do
  for key in old_selection old_draft; do
    rel=$(plan get "$s" "$key")
    mkdir -p "$STAGE/old/$(dirname "$rel")"
    cp "$MRS_OLD/$rel" "$STAGE/old/$rel"
  done
done
echo "staged in $STAGE"
