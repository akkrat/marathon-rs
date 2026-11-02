#!/usr/bin/env bash
# run-fake-rows.sh <sshj|russh|russh-patched> [case ...]: the rows that need a misbehaving server, against
# `ssh-russh-spike serve` in the matching mode on 127.0.0.1, and against targets/kex-fail-server.py. Output goes to
# $SPIKE_TMP/results/<client>-fake/<case>.txt, like run-matrix.sh. Needs ssh-russh/build-patched.sh for T25.
set -uo pipefail
status=0
failed=0
here=$(cd "$(dirname "$0")" && pwd)
SPIKE_TMP=${SPIKE_TMP:-/tmp/ssh-spike}
client=$1
shift
keys=$SPIKE_TMP/keys
results=$SPIKE_TMP/results/$client-fake
mkdir -p "$results"
fake=$here/ssh-russh/target/debug/ssh-russh-spike
fake_keepalive=$here/ssh-russh/target/fake-keepalive/debug/ssh-russh-spike

servers=()
serve() {
    local binary=$1 port=$2
    shift 2
    "$binary" serve "$port" "$keys/authorized_keys" "$keys/fake_host_ed25519" "$@" > "$results/server-$port.log" 2>&1 &
    servers+=($!)
}
trap 'kill "${servers[@]}" 2>/dev/null' EXIT

serve "$fake" 22231 --hang-on-close
serve "$fake" 22232 --reject-exec
serve "$fake" 22233 --disconnect-on-exec=11
serve "$fake" 22234 --disconnect-on-auth=12
serve "$fake" 22235 --disconnect-on-auth=2
serve "$fake" 22236 --disconnect-on-auth=11
RUSSH_SPIKE_WITHHOLD_KEEPALIVE_REPLIES=1 serve "$fake_keepalive" 22237
python3 -I "$here/targets/kex-fail-server.py" 22238 > "$results/server-22238.log" 2>&1 &
servers+=($!)
for spec in "22239 12" "22240 2" "22241 11"; do
    read -r port reason <<<"$spec"
    python3 -I "$here/targets/kex-fail-server.py" "$port" "$reason" > "$results/server-$port.log" 2>&1 &
    servers+=($!)
done
sleep 1

# "fake-password" is the fake's fixture password, not a credential
run() {
    local port=$1
    shift
    SPIKE_HOST=127.0.0.1 SPIKE_PORT=$port SPIKE_USER=spike SPIKE_AUTH=${SPIKE_AUTH:-password:fake-password} \
        "$here/run-case.sh" "$client" container "$@" || failed=1
}

cases=("$@")
if [[ ${#cases[@]} -eq 0 ]]; then
    cases=(t15 t15h t23 t24 t25 t26)
fi
for c in "${cases[@]}"; do
    echo "== $client fake $c"
    failed=0
    skipped=0
    echo running > "$results/$c.status"
    {
        case $c in
            t15 | t15h) SPIKE_KA_MS=0 run 22231 "$c" ;;
            t23) run 22232 t23 ;;
            t24) run 22233 t24 ;;
            t25) SPIKE_KA_MS=1000 SPIKE_KA_MAX=2 run 22237 t25 ;;
            t26)
                # Disconnects right after authentication (they surface at the first command) and before the key exchange,
                # and a failed key exchange; classified as at connect time
                echo "-- disconnect TOO_MANY_CONNECTIONS (12) after auth"; SPIKE_ATTEMPTS=1 run 22234 connect
                echo "-- disconnect PROTOCOL_ERROR (2) after auth"; SPIKE_ATTEMPTS=1 run 22235 connect
                echo "-- disconnect BY_APPLICATION (11) after auth"; SPIKE_ATTEMPTS=1 run 22236 connect
                echo "-- no common key-exchange algorithm"; run 22238 connect
                echo "-- disconnect TOO_MANY_CONNECTIONS (12) before kex"; run 22239 connect
                echo "-- disconnect PROTOCOL_ERROR (2) before kex"; run 22240 connect
                echo "-- disconnect BY_APPLICATION (11) before kex"; run 22241 connect
                ;;
        esac
    } > "$results/$c.txt" 2> "$results/$c.stderr"
    if [[ $failed == 1 ]]; then
        echo failed > "$results/$c.status"
        status=1
    elif [[ $skipped == 1 ]]; then
        echo skipped > "$results/$c.status"
    elif [[ ! -s $results/$c.txt ]]; then
        echo failed > "$results/$c.status"
        status=1
    else
        echo completed > "$results/$c.status"
    fi
    grep -v '^\s*at ' "$results/$c.txt"
done

exit "$status"
