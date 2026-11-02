#!/usr/bin/env bash
# Build from scratch package copies: Cargo's path patches must never rewrite the tracked lockfile.
set -euo pipefail
export PATH=/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH
cd "$(dirname "$0")"
package=$PWD
base=${CARGO_TARGET_DIR:-$package/target}
mkdir -p "$base"
base=$(cd "$base" && pwd)
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
mkdir "$scratch/package"
cp Cargo.toml Cargo.lock "$scratch/package/"
cp -R src "$scratch/package/"
(cd "$scratch/package" && cargo fetch -q --locked)
source_dir=$(find "${CARGO_HOME:-$HOME/.cargo}/registry/src" -maxdepth 2 -type d -name 'russh-0.64.1' | head -n 1)

build_variant() {
    local variant=$1 patch_file=$2
    local crate="$scratch/russh-$variant"
    cp -R "$source_dir" "$crate"
    patch -s -p1 -d "$crate" < "$patch_file"
    # Reset the scratch lock between variants; both resolve from the committed registry lock.
    cp "$package/Cargo.lock" "$scratch/package/Cargo.lock"
    (cd "$scratch/package" && CARGO_TARGET_DIR="$base/$variant" cargo build -q \
        --config "patch.crates-io.russh.path=\"$crate\"")
    echo "built $base/$variant/debug/ssh-russh-spike"
}
build_variant patched russh-close-after-local-close.patch
# Server-only T25 variant; never used as a client.
build_variant fake-keepalive fake-withhold-keepalive-replies.patch
if [[ ${1:-} == --test ]]; then
    (cd "$scratch/russh-patched" && CARGO_TARGET_DIR="$base/patched-tests" cargo test -q --lib client::)
fi
