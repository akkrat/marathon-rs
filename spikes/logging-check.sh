#!/usr/bin/env bash
# logging-check.sh: the log rules of ssh.debug (F5). Runs T18 (writeFile of "hello\n", base64 aGVsbG8K) against the
# container and counts, per client and log setting, the log lines and the lines that contain the file content or the
# exec command line.
# - sshj: PerformanceDefaultConfig logs sshj at ERROR, or at DEBUG with ssh.debug (SPIKE_SSH_DEBUG)
# - russh: the `log` facade, filtered by RUST_LOG
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
SPIKE_TMP=${SPIKE_TMP:-/tmp/ssh-spike}
out=$SPIKE_TMP/logging
mkdir -p "$out"
check() {
    local label=$1
    shift
    env "$@" "$here/run-case.sh" "${CLIENT}" container t18 > "$out/$label.out" 2> "$out/$label.err"
    printf '%-28s log_lines=%-6s content_lines=%-4s exec_lines=%s\n' "$label" \
        "$(grep -c -v '^SLF4J' "$out/$label.err")" \
        "$(grep -c 'aGVsbG8K' "$out/$label.err")" \
        "$(grep -c -i -E 'exec|umask' "$out/$label.err")"
}
CLIENT=sshj check sshj-default SPIKE_SSH_DEBUG=false
CLIENT=sshj check sshj-ssh.debug SPIKE_SSH_DEBUG=true
CLIENT=russh check russh-warn RUST_LOG=warn
CLIENT=russh check russh-debug RUST_LOG=russh=debug
CLIENT=russh check russh-trace RUST_LOG=russh=trace

for debug in false true; do
    SPIKE_SSH_DEBUG=$debug "$here/run-case.sh" sshj container logging \
        > "$out/policy-$debug.out" 2> "$out/policy-$debug.err"
    cat "$out/policy-$debug.out"
done
