#!/usr/bin/env bash
# Fake-worker feasibility (WP 0.3a deliverable 5, input for 0.4a), against `ssh-russh-spike serve` on 127.0.0.1:
#   1. a stock OpenSSH client (/usr/bin/ssh, as rsync's --rsh runs it) logs in with a key, and with a password via sshpass;
#   2. exec and signal requests from the sshj and russh executors reach the server handler (case t06: TERM gives 143);
#   3. rsync -e ssh runs `rsync --server` on the fake in both directions, with RsyncFileBridge's flags;
#   4. row T15: close() against a server that never completes the close, for both clients.
# The ssh options are those of RsyncFileBridge's --rsh string plus UserKnownHostsFile=/dev/null, so the fake's host key
# never lands in the user's known_hosts.
set -uo pipefail
here=$(cd "$(dirname "$0")" && pwd)
SPIKE_TMP=${SPIKE_TMP:-/tmp/ssh-spike}
keys=$SPIKE_TMP/keys
work=$SPIKE_TMP/fake-worker
bin=$here/ssh-russh/target/debug/ssh-russh-spike
rm -rf "$work"
mkdir -p "$work"

"$bin" serve 22230 "$keys/authorized_keys" "$keys/fake_host_ed25519" > "$work/server.log" 2>&1 &
server=$!
"$bin" serve 22231 "$keys/authorized_keys" "$keys/fake_host_ed25519" --hang-on-close > "$work/server-hang.log" 2>&1 &
hang_server=$!
trap 'kill $server $hang_server 2>/dev/null' EXIT
sleep 1

rsh="ssh -o 'StrictHostKeyChecking no' -F /dev/null -o UserKnownHostsFile=/dev/null -o ServerAliveInterval=15 -o ServerAliveCountMax=4"
# "fake-password" below is the fake worker's fixture password (ssh-russh-spike serve), not a credential
ssh_opts=(-o 'StrictHostKeyChecking no' -F /dev/null -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -l spike -p 22230)

mark() { echo "== $*"; echo "MARK $*" >> "$work/server.log"; }

mark "1a stock ssh, key auth"
/usr/bin/ssh "${ssh_opts[@]}" -i "$keys/ed25519_openssh" 127.0.0.1 'echo key-auth-ok' 2>&1
echo "exit=$?"

mark "1b stock ssh through sshpass, password auth"
sshpass -p fake-password /usr/bin/ssh "${ssh_opts[@]}" 127.0.0.1 'echo password-auth-ok' 2>&1
echo "exit=$?"

mark "1c stock ssh through sshpass, wrong password"
sshpass -p wrong /usr/bin/ssh "${ssh_opts[@]}" 127.0.0.1 'echo should-not-run' 2>&1
echo "exit=$?"

for client in sshj russh; do
    mark "2 exec and signal from $client (case t06)"
    SPIKE_HOST=127.0.0.1 SPIKE_PORT=22230 SPIKE_USER=spike SPIKE_AUTH=password:fake-password \
        "$here/run-case.sh" "$client" container t06 2>/dev/null
done

mark "3 rsync both directions"
mkdir -p "$work/src/sub" "$work/remote" "$work/back"
head -c 300000 /dev/urandom > "$work/src/blob.bin"
echo hello > "$work/src/sub/hello.txt"
flags=(-r -l -p -t -g -o -D --delete --partial --partial-dir=.rsync-partial --delay-updates --rsync-path=/usr/bin/rsync)
echo "local rsync: $(/usr/bin/rsync --version 2>&1 | head -n 1)"
/usr/bin/rsync "${flags[@]}" --rsh "$rsh -i $keys/ed25519_openssh -l spike -p 22230 " "$work/src/" "127.0.0.1:$work/remote/" 2>&1
echo "send exit=$?"
sshpass -p fake-password /usr/bin/rsync "${flags[@]}" --rsh "$rsh -l spike -p 22230 " "127.0.0.1:$work/remote/" "$work/back/" 2>&1
echo "receive (password) exit=$?"
diff -r "$work/src" "$work/back" && echo "roundtrip identical"

for client in sshj russh russh-patched; do
    mark "4 T15 close against a hung close, $client"
    SPIKE_HOST=127.0.0.1 SPIKE_PORT=22231 SPIKE_USER=spike SPIKE_AUTH=password:fake-password \
        "$here/run-case.sh" "$client" container t15 2>/dev/null
done

echo "== server events"
grep -E '^(EVENT|MARK)' "$work/server.log" | grep -v -E 'EVENT (exit|eof from client|channel open)' | sed -E 's#/tmp/[^ ]*#<path>#g'
echo "== hang server events"
grep -E '^EVENT (signal|close|hanging)' "$work/server-hang.log"
