#!/usr/bin/env bash
# container.sh up|down: the OpenSSH targets of the spike, from the image that the Kotlin SshdContainer test helper pins.
#   ssh-spike-default on 127.0.0.1:22220: the image's configuration, as in SshdContainer (password access, test-user);
#   ssh-spike-limits  on 127.0.0.1:22221: the same plus MaxSessions 2 and MaxStartups 1 (rows T08 and T19).
# Both accept the keys of keys.sh and have rsync installed (row T20).
set -euo pipefail
SPIKE_TMP=${SPIKE_TMP:-/tmp/ssh-spike}
IMAGE=lscr.io/linuxserver/openssh-server:9.0_p1-r2-ls99

start() {
    local name=$1 port=$2 extra_config=$3
    docker rm -f "$name" >/dev/null 2>&1 || true
    docker run -d --name "$name" \
        -e PASSWORD_ACCESS=true -e USER_NAME=test-user -e USER_PASSWORD=test-password \
        -p "127.0.0.1:$port:2222" "$IMAGE" >/dev/null
    for _ in $(seq 1 60); do
        if docker exec "$name" nc -z localhost 2222 2>/dev/null; then break; fi
        sleep 1
    done
    docker exec "$name" apk add --no-cache -q rsync >/dev/null
    docker cp "$SPIKE_TMP/keys/authorized_keys" "$name:/config/.ssh/authorized_keys"
    docker exec "$name" sh -c 'chown test-user /config/.ssh/authorized_keys && chmod 600 /config/.ssh/authorized_keys'
    if [[ -n $extra_config ]]; then
        docker exec "$name" sh -c "printf '$extra_config' >> /config/ssh_host_keys/sshd_config && pkill -HUP -f 'sshd.pam -D'"
        sleep 1
    fi
    echo "$name listening on 127.0.0.1:$port"
}

case ${1:-} in
    up)
        [[ -f $SPIKE_TMP/keys/authorized_keys ]] || "$(dirname "$0")/keys.sh"
        start ssh-spike-default 22220 ''
        start ssh-spike-limits 22221 'MaxSessions 2\nMaxStartups 1\n'
        ;;
    down)
        docker rm -f ssh-spike-default ssh-spike-limits >/dev/null 2>&1 || true
        ;;
    *)
        echo "usage: $0 up|down" >&2
        exit 2
        ;;
esac
