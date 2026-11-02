#!/usr/bin/env bash
# compare.sh <target> [other client]: compares the sshj results of run-matrix.sh / run-fake-rows.sh with another
# client's (default russh) for one target (container, remote-mac or fake).
#
# Comparison policy:
# - every `key: value` line is compared, close and await outcomes included (they are printed as normalised words:
#   returned, failed: <kind>, still waiting after N s);
# - `*.info.*` keys (timings, raw messages, class names) and keep-alive thread names are left out: they are expected
#   to differ and are reported in the results document instead;
# - explicit skipped status on both clients means skipped (container-only rows, for example);
#   a case file present for one client only is missing.
# Prints one line per case (same, differs, skipped, missing) and exits non-zero when any case differs or is missing.
set -uo pipefail
SPIKE_TMP=${SPIKE_TMP:-/tmp/ssh-spike}
target=$1 other=${2:-russh}
left=$SPIKE_TMP/results/sshj-$target right=$SPIKE_TMP/results/$other-$target
filter() {
    # Legacy files sometimes put a failed close inside .info.close_ms; never discard that outcome.
    awk '/^[^:]*\.info\.([a-z_]*_)?close_ms: / && $0 !~ /: [0-9]+$/ { print; next }
         /^[^:]*\.info\.[^:]*:|^[^:]*keep_alive_threads[^:]*:|^[[:space:]]+at |^SLF4J/ { next }
         { print }' "$1"
}
status=0
same=0 differs=0 skipped=0 missing=0
if [[ ! -d $left || ! -d $right ]]; then
    echo "missing result directory for $target" >&2
    exit 1
fi
for name in $( (ls "$left" "$right" 2>/dev/null) | grep -E '^t[0-9]+[a-z]*\.txt$' | sort -u); do
    case_name=${name%.txt}
    if [[ ! -f $left/$name || ! -f $right/$name ]]; then
        echo "== $case_name missing ($( [[ -f $left/$name ]] && echo "no $other result" || echo "no sshj result"))"
        missing=$((missing + 1)); status=1
    elif [[ $(cat "$left/$case_name.status" 2>/dev/null) == skipped && $(cat "$right/$case_name.status" 2>/dev/null) == skipped ]]; then
        echo "== $case_name skipped on $target"
        skipped=$((skipped + 1))
    elif [[ $(cat "$left/$case_name.status" 2>/dev/null) != completed || $(cat "$right/$case_name.status" 2>/dev/null) != completed || ! -s $left/$name || ! -s $right/$name ]]; then
        echo "== $case_name missing completion (failed, interrupted, empty, or legacy result)"
        missing=$((missing + 1)); status=1
    elif diff <(filter "$left/$name") <(filter "$right/$name") >/dev/null; then
        echo "== $case_name same"
        same=$((same + 1))
    else
        echo "== $case_name differs (< sshj, > $other)"
        diff <(filter "$left/$name") <(filter "$right/$name")
        differs=$((differs + 1)); status=1
    fi
done
if [[ $((same + differs + skipped + missing)) == 0 ]]; then
    echo "no case results for $target" >&2
    status=1
fi
echo "== $target sshj vs $other: $same same, $differs differ, $skipped skipped, $missing missing"
exit $status
