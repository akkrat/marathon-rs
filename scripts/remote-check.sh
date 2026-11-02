#!/usr/bin/env bash
# Runs the gate (scripts/check.sh) for the current HEAD on the Linux build box, over SSH.
# GitHub Actions does not run for this repository, so this is the Linux half of the gate.
#
# The SSH destination comes from MARATHON_LINUX_HOST, or from ~/.config/marathon-rs/remote.env
# (a line `MARATHON_LINUX_HOST=user@host`), so no host name is kept in the repository.
# The box needs git, a C compiler and internet access; rustup is installed into its home on first use.
set -euo pipefail

cd "$(dirname "$0")/.."
config=$HOME/.config/marathon-rs
if [[ -z "${MARATHON_LINUX_HOST:-}" && -f $config/remote.env ]]; then
    # shellcheck disable=SC1091
    source "$config/remote.env"
fi
host=${MARATHON_LINUX_HOST:?set MARATHON_LINUX_HOST or ~/.config/marathon-rs/remote.env}
ssh_opts=(-o BatchMode=yes -o ConnectTimeout=15)

head=$(git rev-parse HEAD)
echo "==> remote gate for $(git rev-parse --short HEAD) on the Linux box"

# A bare repository on the box receives HEAD and main; main stands in for origin/main there.
ssh "${ssh_opts[@]}" "$host" 'mkdir -p ~/marathon-ci && { [ -d ~/marathon-ci/repo.git ] || git init -q --bare ~/marathon-ci/repo.git; }'
GIT_SSH_COMMAND="ssh ${ssh_opts[*]}" git push -q --force "$host:marathon-ci/repo.git" \
    "$head:refs/heads/ci/$head" "main:refs/heads/main"

ssh "${ssh_opts[@]}" "$host" bash -s -- "$head" <<'REMOTE'
set -euo pipefail
head=$1
ci=~/marathon-ci
# Runs share the work tree and target dir, so a second run waits for the first instead of deleting its tree.
exec 9>"$ci/lock"
flock 9
trap 'git -C "$ci/repo.git" update-ref -d "refs/heads/ci/$head" || true' EXIT
if ! [ -x ~/.cargo/bin/rustup ]; then
    curl -sSf https://sh.rustup.rs | sh -s -- -y -q --profile minimal --default-toolchain none
fi
export PATH=$HOME/.cargo/bin:$PATH
rm -rf "$ci/work"
git clone -q "$ci/repo.git" "$ci/work"
cd "$ci/work"
git checkout -q --detach "$head"
# rust-toolchain.toml pins the version; this installs it with its components.
rustup toolchain install >/dev/null
# A shared target dir keeps incremental builds across runs.
export CARGO_TARGET_DIR=$ci/target
scripts/check.sh
REMOTE
