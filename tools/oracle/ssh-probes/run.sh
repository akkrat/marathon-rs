#!/usr/bin/env bash
# WP 4.2: reference JDK, the Kotlin checkout's installed CLI distribution (sshj 0.39.0) and the fake worker.
# Regenerates fixtures/oracle/ssh/*.jsonl; the Kotlin repository stays read-only. `run.sh flood` regenerates only
# flood.jsonl (the other tables carry per-run keys).
set -euo pipefail
only=${1:-all}
cd "$(dirname "$0")/../../.."
export PATH=/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH
java_home=${JAVA_HOME:-/Applications/Android Studio.app/Contents/jbr/Contents/Home}
kotlin_repo=${MARATHON_KT:-$HOME/src/marathon}
lib="$kotlin_repo/cli/build/install/marathon/lib/*"
probe=tools/oracle/ssh-probes/SshProbe.java
out=fixtures/oracle/ssh
mkdir -p target "$out"
work=$(mktemp -d target/wp-4.2-ssh-probe.XXXXXX)
worker_pid=
cleanup() { [[ -n ${KEEP:-} ]] && { echo "kept $work"; return; };  [[ -n $worker_pid ]] && kill "$worker_pid" 2>/dev/null || true; rm -rf "$work"; }
trap cleanup EXIT
# Throwaway keys, generated per run; only public halves reach the tables.
for spec in ed25519:ed25519 other:ed25519 rsa:rsa ecdsa:ecdsa; do
    ssh-keygen -q -t "${spec#*:}" -N '' -C oracle-only -f "$work/${spec%%:*}"
done
# The fake-worker validation's logback pattern and ROOT=WARN, as in fixtures/fake-worker/evidence.
java() { "$java_home/bin/java" -Dlogback.configurationFile=spikes/ssh-sshj/logback-spike.xml -cp "$lib" "$probe" "$@"; }
if [[ $only == all ]]; then
    java shell > "$out/shell_command.jsonl"
    java known-hosts "$work" > "$out/known_hosts.jsonl"
    # Committed test-only keys (fixtures/PROVENANCE.md); paths stay repository-relative in the table.
    java keys $(ls fixtures/ssh-keys/*/* | grep -v '\.pub$') fixtures/fake-worker/russh/keys-mod-0[2-6].txt > "$out/key_formats.jsonl"
fi

cargo build -q -p fake-worker
cat > "$work/config.json" <<JSON
{"listen":"127.0.0.1:0","root":"$PWD/$work/root","transcript":"$PWD/$work/transcript.jsonl","host_key":"$PWD/$work/ed25519",
 "auth":{"user":"fixture","password":"fake-password","authorized_keys":[],"password_auth":true,"keyboard_interactive":true},
 "max_sessions":4,"rsync_binary":"/usr/bin/rsync","rsync_remote":"/usr/bin/rsync","fixtures":"$PWD/fixtures/fake-worker","run":"oracle",
 "script":{"rules":[]}}
JSON
# R2-sol-1's flood: 32 x (1 MiB stdout, one stderr byte), too large to inline above.
python3 -I - "$work/config.json" <<'PY'
import json, sys
config = json.load(open(sys.argv[1]))
chunks = [c for _ in range(32) for c in ({"data": "x" * 1024 * 1024}, {"data": "e", "extended": 1})]
config["script"]["rules"].append({"name": "flood", "matcher": {"exact": "flood"}, "action": {"type": "output", "chunks": chunks}})
json.dump(config, open(sys.argv[1], "w"))
PY
target/debug/fake-worker --config "$work/config.json" > "$work/ready" 2> "$work/worker.err" &
worker_pid=$!
for _ in $(seq 100); do [[ -s $work/ready ]] && break; sleep 0.1; done
port=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["endpoint"].rsplit(":",1)[1])' "$work/ready")
: > "$work/empty_known_hosts"
printf '[127.0.0.1]:%s %s\n' "$port" "$(cut -d' ' -f1,2 "$work/other.pub")" > "$work/wrong_known_hosts"
# One result row per probe run, with its logback lines.
row() {
    python3 -I - "$@" <<'PY'
import json, re, sys
case, port, work, result, log = sys.argv[1:]
row = json.loads(open(result).read())
row["case"] = case
text = json.dumps(row).replace(port, "<port>").replace(work, "<dir>")
row = json.loads(text)
# Logback lines: drop the time and thread, keep level, logger and message; stack frames are not compared.
lines = []
for line in open(log, encoding="utf-8"):
    m = re.match(r"^\d\d:\d\d:\d\d\.\d+ (\w+) +\[[^\]]*\] (\S+) - (.*)$", line.rstrip("\n"))
    if m:
        lines.append({"level": m[1], "logger": m[2], "message": m[3].replace(port, "<port>").replace(work, "<dir>")})
row["log"] = lines
print(json.dumps(row, ensure_ascii=False))
PY
}
if [[ $only == all ]]; then
    : > "$out/connect.jsonl"
    for kind in empty wrong; do
        java connect "$port" "$work/${kind}_known_hosts" "$work/ed25519.pub" > "$work/$kind.out" 2> "$work/$kind.err"
        row "connect_$kind" "$port" "$work" "$work/$kind.out" "$work/$kind.err" >> "$out/connect.jsonl"
    done
    echo "wrote $out/shell_command.jsonl $out/known_hosts.jsonl $out/key_formats.jsonl $out/connect.jsonl"
fi
java flood "$port" > "$work/flood.out" 2> "$work/flood.err"
row flood_stderr_only "$port" "$work" "$work/flood.out" "$work/flood.err" > "$out/flood.jsonl"
echo "wrote $out/flood.jsonl"
