#!/usr/bin/env bash
# Runs a marathon binary (Kotlin or Rust) on an input directory in a fresh sandbox (PLAN.md §6.3, "Isolated runs").
#
# usage: run-isolated.sh [--id ID] [--timeout SECONDS] [--env NAME=VALUE]... [--pass NAME]... [--] BINARY INPUT_DIR [ARG...]
#
#   - INPUT_DIR is copied to tools/oracle/runs/<id>/in, which is also the working directory. In ARGs, @IN@ stands for that
#     directory, e.g. `-m @IN@/Marathonfile`.
#   - The environment holds only HOME=<run>/home, TMPDIR=<run>/tmp, PATH=/usr/bin:/bin:/usr/sbin:/sbin, the --env pairs
#     (which may override those three) and the --pass variables copied from the caller's environment.
#   - The binary runs with a timeout (default 600 s; its whole process group is killed when it expires).
#   - stdout, stderr and manifest.json are written to tools/oracle/runs/<id>/. The manifest records the run metadata, the
#     input tree before the run and the tree of in/, home/ and tmp/ after it, with sizes and sha256.
#   - The id defaults to <UTC timestamp>-<random>; an existing run directory is never reused, so two runs never share files.
#   - Before running, the sandbox is rejected (exit 3, run directory removed) when a path it would write through leads out of
#     the run directory: a symlink in the input that resolves outside it, HOME or TMPDIR overridden to a path outside the run
#     directory, an absolute argument outside it, an outputDir/application/testApplication of the Marathonfile (-m, or
#     ./Marathonfile) that resolves outside it, or a `parse -o NAME` whose <outputDir>/NAME.yml does. The Marathonfile is
#     decoded by the oracle (tools/oracle/oracle marathonfile-writes), so the oracle must be built. Arrangements the checks
#     do not model are rejected too: a subcommand (run, parse, version) anywhere but first, `--`, and environment names
#     or values with line breaks (the helper gets the environment as KEY=VALUE lines).
#     tools/oracle/test-run-isolated.sh tests this.
set -euo pipefail

ORACLE_DIR=$(cd "$(dirname "$0")" && pwd)
export ORACLE_DIR
id=""
timeout=600
envs=()
passes=()
while [[ $# -gt 0 ]]; do
    case $1 in
        --id) id=$2; shift 2 ;;
        --timeout) timeout=$2; shift 2 ;;
        --env) envs+=("$2"); shift 2 ;;
        --pass) passes+=("$2"); shift 2 ;;
        --) shift; break ;;
        -*) echo "run-isolated.sh: unknown option $1" >&2; exit 2 ;;
        *) break ;;
    esac
done
if [[ $# -lt 2 ]]; then
    sed -n '4p' "$0" | sed 's/^# //' >&2
    exit 2
fi
binary=$1
input=$2
shift 2

if [[ -z $id ]]; then
    id="$(date -u +%Y%m%dT%H%M%SZ)-$(od -An -N4 -tx4 /dev/urandom | tr -d ' ')"
fi
run="$ORACLE_DIR/runs/$id"
if [[ -e $run ]]; then
    echo "run-isolated.sh: $run already exists" >&2
    exit 1
fi
mkdir -p "$run/home" "$run/tmp"
cp -R "$input" "$run/in"

pass_pairs=()
for name in ${passes[@]+"${passes[@]}"}; do
    if [[ -n ${!name+x} ]]; then pass_pairs+=("$name=${!name}"); fi
done

python3 -I - "$run" "$binary" "$timeout" "$(cd "$input" && pwd)" "${#envs[@]}" ${envs[@]+"${envs[@]}"} \
    "${#pass_pairs[@]}" ${pass_pairs[@]+"${pass_pairs[@]}"} "$@" <<'PY'
import datetime, hashlib, json, os, signal, subprocess, sys

run, binary, timeout, source = sys.argv[1:5]
rest = sys.argv[5:]
n = int(rest[0]); env_pairs = rest[1:1 + n]; rest = rest[1 + n:]
m = int(rest[0]); pass_pairs = rest[1:1 + m]; args = rest[1 + m:]
run = os.path.realpath(run)
inp = os.path.join(run, "in")
args = [a.replace("@IN@", inp) for a in args]

env = {"HOME": os.path.join(run, "home"), "TMPDIR": os.path.join(run, "tmp") + "/", "PATH": "/usr/bin:/bin:/usr/sbin:/sbin"}
for pair in pass_pairs + env_pairs:
    k, _, v = pair.partition("=")
    env[k] = v

# Isolation checks, before anything runs. A run may only write inside its own directory, so the sandbox is rejected (and
# removed) when a path it would write through leads elsewhere.
import re, shutil

def inside(path, base=run):
    real = os.path.realpath(path)
    return real == base or real.startswith(base + os.sep)

problems = []
for d, dirs, files in os.walk(inp):
    for name in dirs + files:
        p = os.path.join(d, name)
        if os.path.islink(p) and not inside(p, inp):
            problems.append(f"symlink {os.path.relpath(p, inp)} -> {os.readlink(p)} leads outside the input directory")
for k in ("HOME", "TMPDIR"):
    if not inside(env[k]):
        problems.append(f"{k}={env[k]} is outside the run directory")
for a in args:
    # Absolute paths in arguments, including --option=/path, must lie in the run directory.
    for v in [a] + ([a.split("=", 1)[1]] if a.startswith("-") and "=" in a else []):
        if v.startswith("/") and not inside(v):
            problems.append(f"argument {a} names a path outside the run directory")

# What marathon writes outside HOME/TMPDIR is decided by the Marathonfile: outputDir, and application/testApplication (an
# .ipa/.zip is extracted next to itself); `parse -o NAME` writes <outputDir>/NAME.yml. The values are decoded by the oracle's
# marathonfile-writes helper with marathon's own substitution and YAML mapper, under the environment the binary gets, so
# quoting, flow and block styles and aliases cannot hide a path. The Marathonfile is -m/--marathonfile, else ./Marathonfile.
import tempfile

def option(names, short):
    """The last value of a CLI option: `--name V`, `--name=V`, `-x V` or `-xV` (Clikt accepts all four)."""
    found = None
    for i, a in enumerate(args):
        if a in names + [short] and i + 1 < len(args):
            found = args[i + 1]
        elif any(a.startswith(n + "=") for n in names):
            found = a.split("=", 1)[1]
        elif a.startswith(short) and len(a) > len(short) and not a.startswith("--"):
            found = a[len(short):]
    return found

# Clikt scopes options to the command they follow, and root options are not passed down: in `-m a.yml parse`, parse reads
# ./Marathonfile. Rather than model that, accept only arrangements whose scope is unambiguous: no subcommand at all, or the
# subcommand as the first argument. A subcommand name anywhere else (even as an option value) and `--` are rejected.
SUBCOMMANDS = ("run", "parse", "version")
subcommand = args[0] if args and args[0] in SUBCOMMANDS else None
for i, a in enumerate(args):
    if a in SUBCOMMANDS and i != 0:
        problems.append(f"argument {i + 1} is the subcommand name {a!r} after other arguments; put the subcommand first and "
                        "give no root options with it")
    if a == "--":
        problems.append("`--` is not supported")

# The helper reads KEY=VALUE lines, so a key or value with a line break (or a key with `=`) would reach it changed and it
# would check a different path from the one marathon decodes.
for k, v in env.items():
    if any(c in k + v for c in "\n\r") or "=" in k or not k:
        problems.append(f"environment variable {k!r}: names and values with line breaks (and names with = or empty names) "
                        "are not supported")

marathonfile = option(["--marathonfile"], "-m") or "Marathonfile"
if not os.path.isabs(marathonfile):
    marathonfile = os.path.join(inp, marathonfile)
parse_output = option(["--output"], "-o") if subcommand == "parse" else None
if problems:
    pass  # rejected below; the Marathonfile is not decoded under an arrangement the checks do not model
elif os.path.isfile(marathonfile):
    fd, env_file = tempfile.mkstemp(prefix="run-isolated-env-")
    with os.fdopen(fd, "w", encoding="utf-8") as f:
        for k, v in env.items():
            f.write(f"{k}={v}\n")
    try:
        helper = subprocess.run([os.path.join(os.environ["ORACLE_DIR"], "oracle"), "marathonfile-writes", marathonfile, "--env-file", env_file],
                                capture_output=True, text=True)
    finally:
        os.unlink(env_file)
    if helper.returncode != 0:
        problems.append(f"cannot decode {marathonfile} with the oracle (run tools/oracle/build.sh): {helper.stderr.strip()}")
    else:
        decoded = json.loads(helper.stdout)
        base_dir = decoded["marathonfileDir"]
        for v in decoded["values"]:
            if v.get("nonScalar"):
                continue  # marathon rejects a non-scalar path at parse time, before writing
            target = os.path.join(base_dir, v["value"])
            if not inside(target):
                problems.append(f"{v['key']}: {v['value']} leads outside the run directory")
            if v["key"] == "outputDir" and parse_output is not None and not inside(os.path.join(target, parse_output + ".yml")):
                problems.append(f"parse -o {parse_output} writes {os.path.join(v['value'], parse_output)}.yml outside the run directory")
elif parse_output is not None and "/" in parse_output:
    problems.append(f"parse -o {parse_output}: no Marathonfile to check the destination against")

if problems:
    for p in problems:
        print(f"run-isolated.sh: rejected: {p}", file=sys.stderr)
    shutil.rmtree(run)
    sys.exit(3)

# Both isolation drivers use this walker; config-dump owns its separate
# before/after snapshot and effect rendering.
sys.path.insert(0, os.environ["ORACLE_DIR"])
from isolation import tree

input_tree = tree(inp, inp)
started = datetime.datetime.now(datetime.timezone.utc)
timed_out = False
with open(os.path.join(run, "stdout"), "wb") as so, open(os.path.join(run, "stderr"), "wb") as se:
    proc = subprocess.Popen([binary] + args, cwd=inp, env=env, stdout=so, stderr=se, stdin=subprocess.DEVNULL,
                            start_new_session=True)
    try:
        code = proc.wait(timeout=float(timeout))
    except subprocess.TimeoutExpired:
        timed_out = True
        os.killpg(proc.pid, signal.SIGKILL)
        code = proc.wait()
finished = datetime.datetime.now(datetime.timezone.utc)

tree_after = []
for sub in ("in", "home", "tmp"):
    tree_after += tree(os.path.join(run, sub), run)

manifest = {
    "id": os.path.basename(run),
    "binary": binary,
    "args": args,
    "source": source,
    "cwd": inp,
    "env": env,
    "timeoutSeconds": float(timeout),
    "timedOut": timed_out,
    "exitCode": code,
    "startedAt": started.isoformat(),
    "finishedAt": finished.isoformat(),
    "durationMs": int((finished - started).total_seconds() * 1000),
    "stdout": {"path": "stdout", "size": os.path.getsize(os.path.join(run, "stdout"))},
    "stderr": {"path": "stderr", "size": os.path.getsize(os.path.join(run, "stderr"))},
    "inputTree": input_tree,
    "tree": tree_after,
}
with open(os.path.join(run, "manifest.json"), "w", encoding="utf-8") as f:
    json.dump(manifest, f, indent=2, ensure_ascii=False)
    f.write("\n")
print(f"{run}: exit {code}{' (timed out)' if timed_out else ''}")
PY
