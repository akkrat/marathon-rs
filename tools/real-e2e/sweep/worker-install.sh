#!/usr/bin/env bash
# WP 7.5 closure sweep, worker side of the linux-kvm profile. Run from the worker checkout after the bundle was cloned:
#   git clone --quiet "$WK/stage/checkout.bundle" "$WK/checkout"
#   RUSTUP_TOOLCHAIN=<installed toolchain> WK=$HOME/mrs-real-e2e-7.5 "$WK/checkout/tools/real-e2e/sweep/worker-install.sh" BASE_HOST_JSON
# BASE_HOST_JSON is a copy of the 7.2 worker config (ports, image, SDK, JDK); only its paths are rewritten, to absolute ones.
set -euo pipefail
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=common.sh
. "$HERE/common.sh"
WK=${WK:-$HOME/mrs-real-e2e-7.5}; sweep_need_abs WK "$WK"
BASE=${1:?base host config}; sweep_need_abs BASE "$BASE"
: "${RUSTUP_TOOLCHAIN:?the existing toolchain to select (nothing is installed or updated)}"
STAGE=$WK/stage
FROZEN=e47352f25cbc50333900d5d040a5544ff1224501
SRC=$(tr -d '[:space:]' < "$WK/checkout/docs/release-source.sha")
native=$(git -C "$WK/checkout" rev-parse HEAD) || sweep_die "cannot read the checkout's HEAD"
echo "native commit: $native"

git init --bare --quiet "$WK/kotlin-src"
git -C "$WK/kotlin-src" unpack-objects -q < "$STAGE/frozen.pack"
git -C "$WK/kotlin-src" unpack-objects -q < "$STAGE/release.pack"

# Every lookup and archive runs on its own so that a failure stops the installer before the build and the config.
check_sample() {   # check_sample COMMIT -> sets sample_tree and sample_entries
  local commit=$1 tar_file="$WK/archive-check-$1.tar"
  git -C "$WK/kotlin-src" rev-parse --verify --quiet "$commit^{commit}" > /dev/null || sweep_die "commit $commit is not in kotlin-src"
  sample_tree=$(git -C "$WK/kotlin-src" rev-parse "$commit:sample/android-app") || sweep_die "no sample/android-app tree at $commit"
  git -C "$WK/kotlin-src" archive "$commit" sample/android-app > "$tar_file" || sweep_die "git archive failed for $commit"
  tar -tf "$tar_file" > "$tar_file.list" || sweep_die "the sample archive of $commit is not a readable tar"
  sample_entries=$(wc -l < "$tar_file.list") || sweep_die "cannot count the sample archive of $commit"
  [ "$sample_entries" -gt 0 ] || sweep_die "the sample archive of $commit is empty"
  rm -f "$tar_file" "$tar_file.list"
  echo "$commit sample/android-app: $sample_tree entries: $sample_entries"
}
check_sample "$FROZEN"; frozen_tree=$sample_tree; frozen_entries=$sample_entries
check_sample "$SRC"
[ "$frozen_tree" = "$sample_tree" ] || sweep_die "the android sample differs between the frozen and the release source"
[ "$frozen_entries" = "$sample_entries" ] || sweep_die "the sample archives of the frozen and the release source differ in size"

mkdir -p "$WK/jvm" "$WK/runs"
entries=$(tar -tf "$STAGE/jvm.tar") || sweep_die "the staged distribution is not a readable tar"
sweep_reject_appledouble "$entries" "$STAGE/jvm.tar"
tar -C "$WK/jvm" -xf "$STAGE/jvm.tar"
cp "$STAGE/reference.json" "$WK/jvm/reference.json"
(cd "$WK/checkout" && CARGO_HOME="$WK/cargo-home" CARGO_TARGET_DIR="$WK/target" cargo build --release --locked -p marathon-cli --bin marathon)
sweep_sha256 "$WK/target/release/marathon"
python3 -I "$HERE/plan.py" host-config --base "$BASE" --wk "$WK" --out "$WK/host.json"
echo "host config written: $WK/host.json (mode 0600)"
