#!/usr/bin/env bash
# run-matrix.sh <sshj|russh> <remote-mac|container> [case ...]: runs the T01-T19 cases of one client against one target and
# writes each case's output to $SPIKE_TMP/results/<client>-<target>/<case>.txt. Expects targets/container.sh up.
# Rows that need sshd configuration changes or pause the server (T08, T09, T10, T18, the MaxStartups and reset cases
# of T19) run against the container only.
set -uo pipefail
status=0
failed=0
here=$(cd "$(dirname "$0")" && pwd)
SPIKE_TMP=${SPIKE_TMP:-/tmp/ssh-spike}
client=$1 target=$2
shift 2
results=$SPIKE_TMP/results/$client-$target
mkdir -p "$results"
keys=$SPIKE_TMP/keys
run() { "$here/run-case.sh" "$client" "$target" "$@" || failed=1; }
container_only() {
    if [[ $target == container ]]; then return 0; fi
    skipped=1
    return 1
}
# A closed port: on the remote Mac, port 1 of the Mac's own address (resolved here, never stored); locally, an unused port
refused() {
    if [[ $target == remote-mac ]]; then
        SPIKE_HOST=$(ssh -G remote-mac | awk '$1 == "hostname" { print $2 }') SPIKE_PORT=1 "$@"
    else
        SPIKE_PORT=22999 "$@"
    fi
}

# known_hosts variants for T17, built from the target's real host key
kh=$SPIKE_TMP/known_hosts/$target
mkdir -p "$kh"
if [[ $target == remote-mac ]]; then kh_port=22022; else kh_port=22220; fi
ssh-keyscan -p "$kh_port" -t ed25519 127.0.0.1 2>/dev/null > "$kh/plain"
awk -v k="$(cut -d' ' -f2 "$keys/ed25519_openssh.pub")" '{ print $1, $2, k }' "$kh/plain" > "$kh/mismatch"
cp "$kh/plain" "$kh/hashed" && ssh-keygen -q -H -f "$kh/hashed" 2>/dev/null && rm -f "$kh/hashed.old"
sed 's/^\[127.0.0.1\]/[127.0.0.2]/' "$kh/plain" > "$kh/unknown"
awk -v k="$(cut -d" " -f2 "$keys/rsa_openssh.pub")" '{ print $1, "ssh-rsa", k }' "$kh/plain" > "$kh/other_algorithm_only"
{ awk '{ print $1, "ssh-ed25519", "AAAA-not-base64" }' "$kh/plain"; cat "$kh/plain"; } > "$kh/malformed_line_first"
# R3: several keys of one algorithm, field whitespace, host-name case, patterns, negation, host lists
cat "$kh/mismatch" "$kh/plain" > "$kh/mismatch_then_match"
cat "$kh/plain" "$kh/mismatch" > "$kh/match_then_mismatch"
awk '{ print $1 "\t\t" $2 "   " $3 }' "$kh/plain" > "$kh/tabs_and_spaces"
sed 's/^\[127.0.0.1\]/[LOCALHOST]/' "$kh/plain" > "$kh/uppercase_host"
sed 's/^\[127.0.0.1\]/[127.0.0.?]/' "$kh/plain" > "$kh/wildcard"
awk '{ print "*", $2, $3 }' "$kh/plain" > "$kh/star"
awk '{ print "!" $1 ",*", $2, $3 }' "$kh/plain" > "$kh/negated"
awk '{ print "[127.0.0.2]:'"$kh_port"'," $1, $2, $3 }' "$kh/plain" > "$kh/host_list"

cases=("$@")
if [[ ${#cases[@]} -eq 0 ]]; then
    cases=(t01 t02 t03 t04 t05 t06 t07 t08 t09 t10 t11 t12 t13 t14 t15t t16 t17 t18 t19 t21 t22)
fi

for c in "${cases[@]}"; do
    echo "== $client $target $c"
    failed=0
    skipped=0
    echo running > "$results/$c.status"
    {
        case $c in
            t18) container_only && run t18 ;;
            t08) container_only && SPIKE_PORT=22221 SPIKE_PAUSE_CMD=: run t08 ;;
            t09) container_only && SPIKE_KA_MS=2000 SPIKE_KA_MAX=2 run t09 20000 ;;
            t10)
                if container_only; then
                    for ka in 1500 999 0; do
                        echo "-- keep-alive ${ka} ms, max 2"
                        SPIKE_KA_MS=$ka SPIKE_KA_MAX=2 run t09 15000
                    done
                fi
                ;;
            t11) SPIKE_KA_MS=1000 SPIKE_KA_MAX=2 run t11 ;;
            t12)
                echo "-- refused"; refused run connect
                echo "-- wrong password"; SPIKE_AUTH=password:wrong-password run connect
                ;;
            t13)
                if container_only; then
                    for k in ed25519_openssh ecdsa_openssh rsa_openssh rsa_pem rsa_pkcs8 ed25519_openssh_encrypted \
                        rsa_pem_encrypted ed25519_v3.ppk ed25519_v2.ppk rsa_v3.ppk rsa_v2.ppk ed25519_v3_encrypted.ppk; do
                        echo "-- key $k"; SPIKE_AUTH=key:$keys/$k run connect
                    done
                    echo "-- missing key file"; SPIKE_AUTH=key:$keys/does-not-exist run connect
                else
                    echo "-- key ed25519_openssh (not authorised on the remote Mac)"; SPIKE_AUTH=key:$keys/ed25519_openssh run connect
                fi
                ;;
            t17)
                for v in plain mismatch hashed unknown other_algorithm_only malformed_line_first mismatch_then_match \
                    match_then_mismatch tabs_and_spaces wildcard star negated host_list; do
                    echo "-- known_hosts $v"; SPIKE_KNOWN_HOSTS=$kh/$v run connect
                done
                # The entry says LOCALHOST; the client connects to localhost
                echo "-- known_hosts uppercase_host"; SPIKE_HOST=localhost SPIKE_KNOWN_HOSTS=$kh/uppercase_host run connect
                echo "-- known_hosts missing file"; SPIKE_KNOWN_HOSTS=$kh/does-not-exist run connect
                ;;
            t19)
                echo "-- refused"; SPIKE_ATTEMPTS=1 refused run connect
                echo "-- dns"; SPIKE_HOST=nonexistent.invalid SPIKE_ATTEMPTS=1 run connect
                echo "-- auth"; SPIKE_AUTH=password:wrong-password run connect
                echo "-- host key"; SPIKE_KNOWN_HOSTS=$kh/mismatch run connect
                if container_only; then
                    python3 -I "$here/targets/reset-server.py" 22998 & reset_pid=$!
                    sleep 1
                    echo "-- reset"; SPIKE_PORT=22998 SPIKE_ATTEMPTS=1 run connect
                    kill $reset_pid 2>/dev/null
                    # MaxStartups 1: one unauthenticated connection is held open, the next one is dropped
                    sleep 30 | nc 127.0.0.1 22221 >/dev/null & holder=$!
                    sleep 2
                    echo "-- max startups"; SPIKE_PORT=22221 SPIKE_ATTEMPTS=1 run connect
                    kill $holder 2>/dev/null
                    pkill -f "nc 127.0.0.1 22221" 2>/dev/null
                fi
                ;;
            *) run "$c" ;;
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
