#!/usr/bin/env bash
# Regenerates fixtures/oracle/config-dump/: config-dump over every fixtures/configuration/**/sample_*.yaml.
#
# Each sample runs on a fresh copy of fixtures/configuration/fixture/config (parsing an Apple bundle extracts archives next to
# the Marathonfile, and the fixtures must never change). Paths are normalised as docs/design/config-dump-format.md specifies
# (the copy's path reads <FIXTURE_CONFIG>), and a "golden" key records the input, environment, "now" and host. Every sample runs with ANDROID_HOME=/android-sdk as the only environment variable (as ConfigurationSerializationTest
# does) and "now" fixed at 1970-01-12T13:46:40Z (ConfigurationFactoryTest's reference instant); the samples whose Kotlin tests
# expect a failure without ANDROID_HOME also run with an empty environment (*.no-android-home.json).
#
# fixtures/oracle/config-dump-inputs/ holds Marathonfiles added for the oracle (no copied sample uses an .ipa/.zip bundle, so
# none shows the extraction side effects); they are copied into the same place of the sandbox and dumped as oracle_*.json.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
SRC="$ROOT/fixtures/configuration/fixture/config"
EXTRA="$ROOT/fixtures/oracle/config-dump-inputs"
OUT=${ORACLE_CONFIG_DUMP_OUT:-"$ROOT/fixtures/oracle/config-dump"}
NOW=1970-01-12T13:46:40Z
RUN="$ROOT/tools/oracle/runs/config-dump-goldens-$(date +%Y%m%d%H%M%S)-$$"

mkdir -p "$RUN"
rm -rf "$OUT"
mkdir -p "$OUT"
printf 'ANDROID_HOME=/android-sdk\n' > "$RUN/android-home.env"
: > "$RUN/empty.env"

dump() { # <sample path relative to SRC> <env file> <output name>
    local sample=$1 env=$2 name=$3
    local sandbox="$RUN/$name/config"
    mkdir -p "$RUN/$name"
    cp -R "$SRC" "$sandbox"
    (cd "$EXTRA" && find . -name '*.yaml' | while read -r f; do cp "$f" "$sandbox/$f"; done)
    local canonical
    canonical=$(cd "$sandbox" && pwd -P)
    "$ROOT/tools/oracle/config-dump" "$sandbox/$sample" --env-file "$env" --now "$NOW" > "$RUN/$name.raw.json"
    python3 -I - "$RUN/$name.raw.json" "$OUT/$name.json" "$sample" "$env" "$NOW" "$canonical" "$sandbox" <<'PY'
# Normalises the dump (docs/design/config-dump-format.md, "Path normalisation") and appends the golden metadata. The
# document is edited as text so that config-dump's own number spellings and layout are kept.
import json, platform, sys
raw, out, sample, env_file, now, *sandbox = sys.argv[1:]
text = open(raw, encoding="utf-8").read()
for p in sorted(set(sandbox), key=len, reverse=True):
    text = text.replace(p, "<FIXTURE_CONFIG>")
rules = []
if "/System/Volumes/Data/" in text:
    text = text.replace("/System/Volumes/Data/", "/")
    rules.append("macOS data volume: /System/Volumes/Data/ -> /")
env = {}
for line in open(env_file, encoding="utf-8").read().splitlines():
    if line and not line.startswith("#"):
        k, _, v = line.partition("=")
        env[k] = v
meta = {
    "input": sample,
    "env": env,
    "now": now,
    "host": {"os": platform.system(), "release": platform.release(), "arch": platform.machine()},
    "normalisations": rules,
}
assert text.endswith("\n}\n")
golden = json.dumps(meta, indent=2, ensure_ascii=False).replace("\n", "\n  ")
text = text[: -len("\n}\n")] + ',\n  "golden": ' + golden + "\n}\n"
json.loads(text)
open(out, "w", encoding="utf-8").write(text)
PY
    printf '%-40s %s\n' "$name" "$(python3 -I -c 'import json,sys; d=json.load(open(sys.argv[1])); e=d["error"]; print(d["stage"], (e["class"].rsplit(".",1)[1] + ": " + (e["message"] or "")[:110]) if e else "")' "$OUT/$name.json")"
}

cd "$SRC"
for sample in $(find . -name 'sample_*.yaml' | sed 's|^\./||' | sort); do
    dump "$sample" "$RUN/android-home.env" "$(echo "${sample%.yaml}" | sed 's|/|__|g')"
done
cd "$EXTRA"
for sample in $(find . -name '*.yaml' | sed 's|^\./||' | sort); do
    dump "$sample" "$RUN/android-home.env" "$(echo "${sample%.yaml}" | sed 's|/|__|g')"
done
dump sample_7.yaml "$RUN/empty.env" sample_7.no-android-home
dump android/sample_1.yaml "$RUN/empty.env" android__sample_1.no-android-home
echo "sandboxes kept in $RUN"
