#!/usr/bin/env bash
# Tests the isolation checks of run-isolated.sh with /bin/echo as the binary. The Marathonfile checks use the oracle, so
# run tools/oracle/build.sh first.
set -euo pipefail

ORACLE_DIR=$(cd "$(dirname "$0")" && pwd)
RI="$ORACLE_DIR/run-isolated.sh"
WORK="$ORACLE_DIR/runs/test-run-isolated-$$"
mkdir -p "$WORK"
trap 'rm -rf "$WORK"' EXIT
failures=0

input() { # <name> <Marathonfile text>: a fresh input directory
    local dir="$WORK/$1"
    mkdir -p "$dir"
    printf '%s\n' "$2" > "$dir/Marathonfile"
    echo "$dir"
}

expect() { # <expected exit> <description> <run-isolated args...>
    local want=$1 what=$2
    shift 2
    local id="test-run-isolated-$$-$RANDOM"
    local got=0
    "$RI" --id "$id" --timeout 10 "$@" > "$WORK/out" 2>&1 || got=$?
    if [[ $got -ne $want ]]; then
        echo "FAIL: $what: exit $got, expected $want"
        sed 's/^/    /' "$WORK/out"
        failures=$((failures + 1))
    else
        echo "ok:   $what"
    fi
    if [[ $want -eq 3 && -e "$ORACLE_DIR/runs/$id" ]]; then
        echo "FAIL: $what: the rejected run directory was left behind"
        failures=$((failures + 1))
    fi
    rm -rf "${ORACLE_DIR:?}/runs/$id"
}

ok=$(input ok 'outputDir: "./marathon"')
expect 0 "a clean input runs" /bin/echo "$ok" -m @IN@/Marathonfile

esc=$(input symlink 'outputDir: "./marathon"')
ln -s "$WORK" "$esc/escape"
expect 3 "a symlink leading outside the input is rejected" /bin/echo "$esc"

abs_link=$(input symlink-into-source 'outputDir: "./marathon"')
ln -s "$abs_link/Marathonfile" "$abs_link/link"
expect 3 "an absolute symlink back into the source directory is rejected" /bin/echo "$abs_link"

rel_link=$(input symlink-relative 'outputDir: "./marathon"')
ln -s Marathonfile "$rel_link/link"
expect 0 "a relative symlink inside the input is kept" /bin/echo "$rel_link"

abs_out=$(input absolute-output 'outputDir: "/tmp/marathon-shared-output"')
expect 3 "an absolute outputDir is rejected" /bin/echo "$abs_out"

up_out=$(input escaping-output 'outputDir: "../../../shared"')
expect 3 "an outputDir that climbs out of the run is rejected" /bin/echo "$up_out"

subst=$(input substituted-output 'outputDir: "${OUT}"')
expect 3 "an outputDir substituted to an outside path is rejected" --env OUT=/tmp/shared /bin/echo "$subst"
expect 0 "an outputDir substituted to an inside path runs" --env OUT=./out /bin/echo "$subst"

quoted=$(input quoted-key '"outputDir": "/tmp/shared"')
expect 3 "a quoted outputDir key is decoded" /bin/echo "$quoted"

single=$(input single-quoted-key "'outputDir': '/tmp/shared'")
expect 3 "a single-quoted outputDir key is decoded" /bin/echo "$single"

flow=$(input flow-map '{name: x, outputDir: /tmp/shared}')
expect 3 "an outputDir in a flow mapping is decoded" /bin/echo "$flow"

block=$(input block-scalar 'outputDir: >-
  /tmp/shared')
expect 3 "an outputDir in a folded block scalar is decoded" /bin/echo "$block"

literal=$(input literal-scalar 'outputDir: |-
  ../../../shared')
expect 3 "a relative outputDir in a literal block scalar is decoded" /bin/echo "$literal"

alias=$(input alias 'a: &p "/tmp/shared"
outputDir: *p')
expect 0 "an aliased outputDir is the anchor name, as marathon reads it" /bin/echo "$alias"

dup=$(input duplicate 'outputDir: "./out"
outputDir: "/tmp/shared"')
expect 3 "the last of duplicate outputDir keys is the one checked" /bin/echo "$dup"

flowapp=$(input flow-application 'vendorConfiguration: {type: iOS, bundle: {application: "/tmp/shared/app.ipa"}}')
expect 3 "an application in nested flow mappings is decoded" /bin/echo "$flowapp"

parse_in=$(input parse 'outputDir: "./marathon"')
expect 0 "parse -o NAME inside the output directory runs" /bin/echo "$parse_in" parse -o tests
expect 3 "parse -o with ../ escaping the run is rejected" /bin/echo "$parse_in" parse -o ../../../shared
expect 3 "parse --output= escaping the run is rejected" /bin/echo "$parse_in" parse --output=../../../shared
expect 3 "parse -oNAME escaping the run is rejected" /bin/echo "$parse_in" parse -o../../../shared
expect 3 "parse -o with an absolute name is rejected" /bin/echo "$parse_in" parse -o /tmp/shared

# Root options do not reach subcommands in Clikt: `-m safe.yml parse` makes parse read ./Marathonfile, not safe.yml.
scoped=$(input scoped 'outputDir: "../../../../../../shared"')
printf 'outputDir: "./marathon"\n' > "$scoped/safe.yml"
expect 3 "a root -m before a subcommand is rejected" /bin/echo "$scoped" -m safe.yml parse -o tests
expect 3 "a root --marathonfile= before a subcommand is rejected" /bin/echo "$scoped" --marathonfile=safe.yml run
expect 3 "a subcommand name as an option value is rejected" /bin/echo "$scoped" parse -m parse
expect 3 "-- is rejected" /bin/echo "$scoped" parse -- -m safe.yml
expect 0 "the subcommand first with its own -m runs" /bin/echo "$scoped" parse -m safe.yml -o tests
expect 3 "the subcommand first without -m checks ./Marathonfile" /bin/echo "$scoped" parse -o tests

# The helper receives the environment as KEY=VALUE lines, so a value with a line break would reach it changed: here it would
# see OUT=./inside while marathon substitutes the whole two-line value.
newline=$(input newline 'outputDir: "${OUT}"')
expect 3 "an environment value with a newline is rejected" --env "OUT=../../../../../../shared
OUT=./inside" /bin/echo "$newline"
expect 3 "an environment value with a carriage return is rejected" --env "OUT=./in"$'\r'"side" /bin/echo "$newline"

bundle=$(input bundle 'vendorConfiguration:
  type: "iOS"
  bundle:
    application: "/tmp/shared/app.ipa"')
expect 3 "an absolute application bundle is rejected" /bin/echo "$bundle"

expect 3 "an absolute argument outside the run is rejected" /bin/echo "$ok" -m /tmp/Marathonfile
expect 3 "HOME outside the run is rejected" --env HOME=/tmp /bin/echo "$ok"

if [[ $failures -ne 0 ]]; then
    echo "$failures failure(s)"
    exit 1
fi
echo "all run-isolated.sh checks passed"
