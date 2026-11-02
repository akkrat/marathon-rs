#!/usr/bin/env bash
# Row T20: the Kotlin marathon's RsyncFileBridge sends a directory to a target and receives it back, with key and with
# password auth, through the argv-logging shims. Targets: the container and the fake worker (ssh-russh-spike serve on
# 127.0.0.1:22230, which must be running). The recorded argv is printed with temporary paths normalised.
set -uo pipefail
here=$(cd "$(dirname "$0")" && pwd)
spikes=$(dirname "$here")
SPIKE_TMP=${SPIKE_TMP:-/tmp/ssh-spike}
keys=$SPIKE_TMP/keys
work=$SPIKE_TMP/t20
rm -rf "$work"
mkdir -p "$work/src/sub" "$work/out"
head -c 100000 /dev/urandom > "$work/src/blob.bin"
echo hello > "$work/src/sub/hello.txt"
export PATH=$here/shims:$PATH RSYNC4J_RSYNC=$here/shims/rsync RSYNC4J_SSHPASS=$here/shims/sshpass

run() {
    local label=$1 port=$2 user=$3 auth=$4 remote=$5
    echo "== $label"
    export SPIKE_ARGV_LOG=$work/$label.argv SPIKE_T20_SRC=$work/src SPIKE_T20_REMOTE=$remote
    export SPIKE_T20_BACK=$work/back-$label SPIKE_T20_OUT=$work/out
    mkdir -p "$SPIKE_T20_BACK"
    SPIKE_HOST=127.0.0.1 SPIKE_PORT=$port SPIKE_USER=$user SPIKE_AUTH=$auth "$spikes/run-case.sh" sshj container t20 2>"$work/$label.stderr"
    diff -r "$work/src" "$SPIKE_T20_BACK" >/dev/null && echo "T20.roundtrip_identical: true" || echo "T20.roundtrip_identical: false"
    sed -E -e "s#$work#<work>#g" -e "s#$keys#<keys>#g" "$SPIKE_ARGV_LOG"
}

run container-key 22220 test-user "key:$keys/ed25519_openssh" /config/t20/
run container-password 22220 test-user password:test-password /config/t20/
run fake-key 22230 spike "key:$keys/ed25519_openssh" "$work/fake-remote/"
run fake-password 22230 spike password:fake-password "$work/fake-remote/"
