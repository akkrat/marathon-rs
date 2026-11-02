#!/usr/bin/env bash
# run-case.sh <sshj|russh|russh-patched> <remote-mac|container|limits> <case> [args]
# Resolves the target at run time, so no host name, user or address is stored in the repository:
#   remote-mac:      the Mac's own sshd through a local forward over the `ssh remote-mac` master; `ssh -G remote-mac` gives the user;
#              auth with SPIKE_REMOTE_MAC_PASSWORD if set, else through the local ssh-agent (see ssh-sshj/src/Agent.kt);
#   container: ssh-spike-default from targets/container.sh, password auth unless SPIKE_AUTH is set;
#   limits:    ssh-spike-limits (MaxSessions 2, MaxStartups 1).
# Any SPIKE_* variable already set wins over these defaults.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
client=$1 target=$2
shift 2

case $target in
    remote-mac)
        # Port 22 of the remote Mac address is Tailscale SSH in check mode (every new connection waits for a browser
        # approval), so the spike reaches the Mac's own OpenSSH server through a local forward over the already
        # approved `ssh remote-mac` master connection. This changes nothing on the Mac.
        ssh -O check remote-mac >/dev/null 2>&1 || { echo "no ssh remote-mac master connection" >&2; exit 1; }
        ssh -O forward -L 127.0.0.1:22022:127.0.0.1:22 remote-mac >/dev/null 2>&1 || true
        export SPIKE_HOST=${SPIKE_HOST:-127.0.0.1}
        export SPIKE_USER=${SPIKE_USER:-$(ssh -G remote-mac | awk '$1 == "user" { print $2 }')}
        export SPIKE_PORT=${SPIKE_PORT:-22022}
        if [[ -n ${SPIKE_REMOTE_MAC_PASSWORD:-} ]]; then
            export SPIKE_AUTH=${SPIKE_AUTH:-password:$SPIKE_REMOTE_MAC_PASSWORD}
        else
            export SPIKE_AUTH=${SPIKE_AUTH:-agent}
        fi
        ;;
    container | limits)
        export SPIKE_HOST=${SPIKE_HOST:-127.0.0.1}
        export SPIKE_USER=${SPIKE_USER:-test-user}
        if [[ $target == container ]]; then
            export SPIKE_PORT=${SPIKE_PORT:-22220}
            name=ssh-spike-default
        else
            export SPIKE_PORT=${SPIKE_PORT:-22221}
            name=ssh-spike-limits
        fi
        export SPIKE_AUTH=${SPIKE_AUTH:-password:test-password}
        export SPIKE_PAUSE_CMD=${SPIKE_PAUSE_CMD:-docker pause $name >/dev/null}
        export SPIKE_RESUME_CMD=${SPIKE_RESUME_CMD:-docker unpause $name >/dev/null}
        ;;
    *)
        echo "unknown target $target" >&2
        exit 2
        ;;
esac

case $client in
    sshj) exec "$here/ssh-sshj/run.sh" "$@" ;;
    russh) exec "$here/ssh-russh/target/debug/ssh-russh-spike" "$@" ;;
    russh-patched) exec "$here/ssh-russh/target/patched/debug/ssh-russh-spike" "$@" ;;
    *)
        echo "unknown client $client" >&2
        exit 2
        ;;
esac
