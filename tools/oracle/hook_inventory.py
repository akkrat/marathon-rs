#!/usr/bin/env python3
"""The search behind docs/design/nondeterminism.md (WP 0.2b): greps every main source file of the Kotlin marathon at
e47352f2 for the families of nondeterminism and keeps docs/design/nondeterminism-sites.csv in step with the hits.

Usage:
  tools/oracle/hook_inventory.py search  <kotlin tree>           print every hit as CSV (file,line,family,text)
  tools/oracle/hook_inventory.py update  <kotlin tree>           add new hits to the sites CSV (kind TODO), drop vanished ones
  tools/oracle/hook_inventory.py check   <kotlin tree> [patched] fail unless the CSV covers exactly the hits, no row is TODO,
                                                                 and, given the patched clone, every OracleHooks label used
                                                                 there is a "hooked" row and every "hooked" row is used

<kotlin tree> is a checkout of e47352f2: the source repository itself (it is only read) or the reference clone
tools/oracle/build/marathon-src. The files searched are the rows of docs/source-inventory.csv, minus buildSrc/ and sample/
(build logic and sample apps never run inside marathon).
"""
import csv
import io
import os
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
SOURCE_INVENTORY = ROOT / "docs" / "source-inventory.csv"
SOURCE_SHA = os.environ.get("ORACLE_SOURCE_SHA", "e47352f25cbc50333900d5d040a5544ff1224501")
if SOURCE_SHA not in ("e47352f25cbc50333900d5d040a5544ff1224501", "c95b212882c7ba7b1ad91b7981916ed175f1530e"):
    raise ValueError("unsupported ORACLE_SOURCE_SHA")
SITES = ROOT / "docs" / "design" / ("nondeterminism-sites.csv" if SOURCE_SHA.startswith("e47352f2") else "nondeterminism-sites-c95b2128.csv")

# Family -> regex over one source line. A line can hit several families; each (file, line, family) is one row.
FAMILIES = {
    "wall": r"Instant\.now\(|currentTimeMillis|Date\(\)|SimpleDateFormat|\bClock\b|\.now\(|Calendar\.getInstance|clock\.millis\("
            r"|measureTimeMillis|lastModified|\btimer\.(elapsedTimeMillis|startTimeMillis|measure)\b",
    "monotonic": r"nanoTime|measureNanoTime|TimeSource|\bmeasureTime\b|markNow|elapsedNow|\bclock\(\)",
    "scheduler": r"\bdelay\(|withTimeout|\bsleep\(|ticker\(|\bonTimeout\b|withRetry\(|\.waitFor\(|\.poll\([^)]|\.tryLock\("
                 r"|\.tryAcquire\(|awaitTermination|\bTimer\(\)|scheduleAtFixedRate|scheduleWithFixedDelay",
    "transport-time": r"\.(connectTimeout|readTimeout|writeTimeout|enableBatch)\(",
    "environment": r"System\.getenv\(",
    "synchronization": r"\bwithLock\b|\bwithPermit\b",
    "random": r"\bRandom\b|\.random\(|shuffle|SecureRandom|ThreadLocalRandom|Math\.random"
              r"|\.next(Int|Long|Double|Boolean|Bytes)\(",
    "id": r"UUID|createTemp|TempDir|tempFile|ProcessHandle|\.pid\b|getLocalHost",
    "concurrency": r"\blaunch\b|\basync\b|\bactor<|\bproduce\b|\.complete\(|completeExceptionally|invokeOnCompletion"
                   r"|\.send\(|trySend|\.offer\(|\bselect\s*[<{]|Dispatchers\.|ThreadPoolContext|newSingleThread"
                   r"|Executors\.|\bthread\s*[({]|Thread\(|runBlocking|GlobalScope|parallelStream|\.parallel\("
                   r"|addShutdownHook|\.subscribe\(|device\.(prepare|execute)\(|(?<![\w.])send\(|\bwithContext\(",
    "library": r"fun toMap\(|allureEnvironmentWriter\(|ResultsUtils\.createHostLabel|PatternLayoutEncoder|PatternLayout\(|<encoder>|System\.getProperties\(|toString\(\)",
    "hash-order": r"ConcurrentHashMap|\bHashMap\b|hashMapOf|\bHashSet\b|hashSetOf|newKeySet|IdentityHashMap"
                  r"|WeakHashMap|identityHashCode|toHashSet",
    "fs-order": r"\.listFiles\(|Files\.(list|walk|newDirectoryStream)\(|walkTopDown|walkBottomUp|listDirectoryEntries",
}
COMPILED = {name: re.compile(rx) for name, rx in FAMILIES.items()}
# Comments, KDoc and imports are not code.
SKIP_LINE = re.compile(r"^\s*(//|\*|/\*|import\s)")

COLUMNS = ["file", "line", "family", "label", "kind", "treatment", "note", "text"]
KINDS = {
    "wall", "monotonic", "scheduler", "random", "id", "concurrency", "hash-order", "fs-order", "library",
    "transport-time", "environment", "synchronization",
    "none",  # the hit is not a source of nondeterminism; the note says why
}
TREATMENTS = {
    "hooked",       # the call goes through OracleHooks under the row's label (tools/oracle/patches/0002-*)
    "via",          # reads a hooked source (the note names it); controlled with it
    "dispatcher",   # coroutine time or order on the core context: the replay test dispatcher controls it
    "script",       # an order the event script decides in replay (vendor and provider events)
    "uncontrolled", # left alone; the note says why that is safe for the replay and what it can change
    "n/a",          # kind none
}


def scope(tree: pathlib.Path):
    with SOURCE_INVENTORY.open(newline="") as f:
        for row in csv.DictReader(f):
            path = row["kotlin_file"]
            if path.startswith(("buildSrc/", "sample/")):
                continue
            if (tree / path).is_file():
                yield path
            elif SOURCE_SHA.startswith("e47352f2") and path.rsplit("/", 1)[-1] in ("CodeCoverageTargets.kt", "MachO.kt"):
                continue
            else:
                sys.exit(f"error: {tree / path} is missing; is {tree} a checkout of e47352f2?")


def search(tree: pathlib.Path):
    hits = []
    for path in list(scope(tree)) + ["cli/src/main/resources/logback.xml"]:
        lines = (tree / path).read_text(encoding="utf-8").splitlines()
        for number, text in enumerate(lines, 1):
            if SKIP_LINE.match(text):
                continue
            for family, rx in COMPILED.items():
                if rx.search(text):
                    hits.append({"file": path, "line": str(number), "family": family, "text": text.strip()[:160]})
    return hits


def key(row):
    return (row["file"], row["line"], row["family"])


def read_sites():
    if not SITES.exists():
        return []
    with SITES.open(newline="") as f:
        return list(csv.DictReader(f))


def write_sites(rows):
    rows.sort(key=lambda r: (r["file"], int(r["line"]), r["family"]))
    out = io.StringIO()
    writer = csv.DictWriter(out, fieldnames=COLUMNS, lineterminator="\n")
    writer.writeheader()
    writer.writerows(rows)
    SITES.write_text(out.getvalue())


def update(tree):
    hits = search(tree)
    old = {key(r): r for r in read_sites()}
    rows = []
    for hit in hits:
        row = old.get(key(hit))
        if row is None:
            row = {c: "" for c in COLUMNS}
            row.update(hit, kind="TODO", treatment="TODO")
        row["text"] = hit["text"]
        rows.append(row)
    write_sites(rows)
    gone = set(old) - {key(h) for h in hits}
    print(f"{len(rows)} rows, {sum(r['kind'] == 'TODO' for r in rows)} TODO, {len(gone)} dropped")


HOOK_CALL = re.compile(r'(?:OracleHooks|oracle\.hooks)\.(\w+)\(\s*"([^"]+)"')


def check(tree, patched):
    problems = []
    hits = search(tree)
    rows = read_sites()
    by_key = {key(r): r for r in rows}
    for hit in hits:
        row = by_key.get(key(hit))
        if row is None:
            problems.append(f"no row for hit {hit['file']}:{hit['line']} [{hit['family']}] {hit['text']}")
        elif row["text"] != hit["text"]:
            problems.append(f"row text differs at {hit['file']}:{hit['line']}: {row['text']!r} != {hit['text']!r}")
    hit_keys = {key(h) for h in hits}
    for row in rows:
        if key(row) not in hit_keys:
            problems.append(f"row without a hit: {row['file']}:{row['line']} [{row['family']}]")
        if row["kind"] not in KINDS:
            problems.append(f"bad kind {row['kind']!r} at {row['file']}:{row['line']}")
        if row["treatment"] not in TREATMENTS:
            problems.append(f"bad treatment {row['treatment']!r} at {row['file']}:{row['line']}")
        if (row["kind"] == "none") != (row["treatment"] == "n/a"):
            problems.append(f"kind none goes with treatment n/a only: {row['file']}:{row['line']}")
        if row["kind"] != "none" and not row["label"]:
            problems.append(f"no label at {row['file']}:{row['line']}")
        if row["treatment"] in ("uncontrolled", "via", "n/a") and not row["note"]:
            problems.append(f"treatment {row['treatment']} needs a note: {row['file']}:{row['line']}")
    hooked = {r["label"] for r in rows if r["treatment"] == "hooked"}
    if patched is not None:
        used = {}
        for path in scope(patched):
            for number, text in enumerate((patched / path).read_text(encoding="utf-8").splitlines(), 1):
                for m in HOOK_CALL.finditer(text):
                    used.setdefault(m.group(2), []).append(f"{path}:{number}")
        for label, where in sorted(used.items()):
            if label not in hooked:
                problems.append(f"hook label {label} used at {where[0]} is not a hooked row")
        for label in sorted(hooked - set(used)):
            problems.append(f"hooked row {label} is not used in {patched}")
    for p in problems:
        print(p)
    kinds = {}
    for r in rows:
        kinds[r["kind"]] = kinds.get(r["kind"], 0) + 1
    print(f"{len(hits)} hits, {len(rows)} rows; by kind: " + ", ".join(f"{k} {v}" for k, v in sorted(kinds.items())))
    if problems:
        sys.exit(f"{len(problems)} problems")
    print("inventory OK")


def main():
    if len(sys.argv) < 3 or sys.argv[1] not in ("search", "update", "check"):
        sys.exit(__doc__)
    tree = pathlib.Path(sys.argv[2]).resolve()
    if sys.argv[1] == "search":
        writer = csv.DictWriter(sys.stdout, fieldnames=["file", "line", "family", "text"], lineterminator="\n")
        writer.writeheader()
        writer.writerows(search(tree))
    elif sys.argv[1] == "update":
        update(tree)
    else:
        check(tree, pathlib.Path(sys.argv[3]).resolve() if len(sys.argv) > 3 else None)


if __name__ == "__main__":
    main()
