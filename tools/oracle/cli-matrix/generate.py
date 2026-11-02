#!/usr/bin/env python3
"""Records the CLI argument and exit matrix of the Kotlin reference build (PLAN.md §3.1, WP 2.5).

usage: python3 -I tools/oracle/cli-matrix/generate.py [OUTPUT]

Runs the reference `marathon` once per case below, each in a fresh sandbox under tools/oracle/runs/cli-matrix-<time>/<id>/
(`in/` is the working directory and holds the case's input files; HOME and TMPDIR point into the sandbox; stdin is
/dev/null; the environment is only HOME, TMPDIR, PATH, JAVA_HOME and the case's variables), and writes OUTPUT (default
fixtures/oracle/cli-matrix.jsonl): a header line, then one row per case with argv, inputs, environment and the normalised
stdout, stderr and exit status. crates/marathon-cli/tests/cli_matrix.rs runs the Rust binary on the same rows and applies
the same normalisation (NORMALISATION below; keep the two in step).

Environment:
  MARATHON_CLI  the reference start script (default: tools/oracle/build/marathon-src/cli/build/install/marathon/bin/marathon,
                which `tools/oracle/build.sh reference` builds from e47352f2)
  CONFIG_DUMP   the JVM config-dump for the rows whose program is config-dump (default: tools/oracle/config-dump, which needs
                the oracle tools of `tools/oracle/build.sh reference`); the Rust side runs `marathon config-dump`
  JAVA_HOME     the reference JDK (default: Android Studio's JBR)

The cases stop at the configuration stage: a valid Android/iOS/macOS Marathonfile would start a real run (adb, simulators),
so the run-outcome rows are covered in-process by crates/marathon-cli/tests/run_outcome.rs and end to end by WP 3.10/7.1.
tools/oracle/run-isolated.sh is not used: its isolation checks need the full oracle build, refuse `--` and subcommands
that are not first, and none of these cases writes outside its sandbox.
"""
import json, os, re, shutil, subprocess, sys, time

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__)))))
CLI = os.environ.get("MARATHON_CLI", os.path.join(ROOT, "tools/oracle/build/marathon-src/cli/build/install/marathon/bin/marathon"))
CONFIG_DUMP = os.environ.get("CONFIG_DUMP", os.path.join(ROOT, "tools/oracle/config-dump"))
JAVA_HOME = os.environ.get("JAVA_HOME", "/Applications/Android Studio.app/Contents/jbr/Contents/Home")
OUT = sys.argv[1] if len(sys.argv) > 1 else os.path.join(ROOT, "fixtures/oracle/cli-matrix.jsonl")
VERSION = "0.10.1"

NORMALISATION = [
    "D12: the SLF4J(I) provider line is removed",
    "D12: stack frame lines (\\tat ..., \\t... N more, \\t... N common frames omitted) are removed",
    "the sandbox working directory, given and canonical spelling, reads <SANDBOX>",
    "a leading HH:mm:ss.SSS log timestamp reads <TIME>",
    "D12: the identity hash after VersionCommandCliConfiguration@ reads <HASH>",
    "the version in 'Starting marathon v<version>,' and a stdout line that is exactly the version read <VERSION>",
    "D1: the reporter line ('Noop exceptions reporter started' in Kotlin, the unsupported-reporting line in Rust) reads"
    " '<TIME> [main] INFO  <D1>'; Kotlin's 'Noop exceptions reporter finished' line is removed",
]

# Marathonfiles used by several cases.
STUB = "name: x\noutputDir: out\nvendorConfiguration:\n  type: stub\n"
EMPTY_VENDOR = "name: x\noutputDir: out\nvendorConfiguration:\n  type: empty\n"
NO_VENDOR = "name: x\noutputDir: out\n"
BAD_KEY = "name: x\noutputDir: out\nbogusKey: 1\n"
NULL_DOC = "---\n---\nname: x\n"
CYCLE = "name: ${A}\noutputDir: out\nvendorConfiguration:\n  type: stub\n"


D1_LINE = "<TIME> [main] INFO  <D1>\n"
D1_ADDED = ("D1: Kotlin logs the reporter line only when bugsnagReporting resolves false (NoopExceptionsReporter); Rust logs"
            " the D1 line after every successful parse")


def case(id, argv, files=None, dirs=(), symlinks=None, modes=None, env=None, deviation=None, rust=None, d1=False,
         rust_stderr=None, program="marathon"):
    """d1: the parse succeeds with bugsnagReporting true, so the Rust row has the D1 line where Kotlin has none.
    rust_stderr: the Rust stderr where a deviation changes only that stream.
    program: "marathon", or "config-dump" for the hidden oracle command (PLAN §6.3)."""
    return {"id": id, "program": program, "argv": argv, "files": files or {}, "dirs": list(dirs), "symlinks": symlinks or {},
            "modes": modes or {}, "env": env or {}, "deviation": D1_ADDED if d1 else deviation, "rust": rust, "d1": d1,
            "rust_stderr": rust_stderr}


CASES = [
    # Help (stdout, exit 0), the 79-column layout off a terminal.
    case("help-root-long", ["--help"]),
    case("help-root-short", ["-h"]),
    case("help-root-columns-ignored", ["--help"], env={"COLUMNS": "200"}),
    case("help-run", ["run", "--help"]),
    case("help-run-short", ["run", "-h"]),
    case("help-parse", ["parse", "--help"]),
    case("help-version", ["version", "--help"]),
    case("help-wins-over-unknown-option", ["--bogus", "-h"]),
    case("help-wins-in-subcommand", ["run", "--bogus", "--help"]),
    case("help-grouped-short", ["-hm"]),
    case("help-after-root-error-not-reached", ["--bogus", "run", "--help"]),
    case("help-unknown-short-in-group", ["-hx"]),
    case("help-as-option-value", ["parse", "-o", "-h"]),
    case("help-with-value", ["--help=x"]),
    case("help-with-value-run", ["run", "--help=x"]),
    case("help-short-equals-is-a-group", ["--bogus", "-h=x"]),
    # Usage errors (stderr, exit 1).
    case("error-unknown-long-suggestion", ["--bogus"]),
    case("error-unknown-long-no-suggestion", ["--frob"]),
    case("error-unknown-long-two", ["--bogus", "--frob", "run"]),
    case("error-unknown-long-equals", ["--bogus=x"]),
    case("error-unknown-run", ["run", "--bogus"]),
    case("error-unknown-parse", ["parse", "--bogus"]),
    case("error-unknown-version", ["version", "--bogus"]),
    case("error-unknown-version-extra", ["version", "--bogus", "extra"]),
    case("error-version-root-option", ["version", "-m", "x"]),
    case("error-unknown-short", ["-x"]),
    case("error-unknown-short-run", ["run", "-x"]),
    case("error-unknown-short-group", ["-xm", "a"]),
    case("error-short-single-dash-long", ["-bugsnag"]),
    case("error-unknown-triple-dash", ["---x"]),
    case("error-partial-long-name", ["--marathon", "x"]),
    case("error-partial-help", ["--hel"]),
    case("error-missing-value-root", ["-m"]),
    case("error-missing-value-long", ["--analyticsTracking"]),
    case("error-missing-value-parse-o", ["parse", "-o"]),
    case("error-missing-value-parse-f", ["parse", "-f"]),
    case("error-missing-value-run-bugsnag", ["run", "--bugsnag"]),
    case("error-missing-value-after-unknown", ["run", "--bogus", "--bugsnag"]),
    case("error-no-such-subcommand", ["frob"]),
    case("error-no-such-subcommand-typo", ["rnu"]),
    case("error-no-such-subcommand-near", ["parsee"]),
    case("error-no-such-subcommand-dash", ["-"]),
    case("error-subcommand-and-option", ["frob", "--bogus"]),
    case("error-option-then-subcommand", ["--frob", "frob"]),
    case("error-run-extra", ["run", "run"]),
    case("error-run-extra-many", ["run", "-m", "a", "b", "c"]),
    case("error-run-extra-after-unknown", ["run", "--frob", "a"]),
    case("error-parse-extra-two", ["parse", "a", "b"]),
    case("error-parse-extra-four", ["parse", "a", "b", "c", "d"]),
    case("error-version-extra", ["version", "extra"]),
    case("error-root-extra-two", ["a", "b"]),
    case("error-version-flag", ["--version"]),
    case("error-version-short-flag", ["-V"]),
    case("error-plus-prefix-equals", ["+a=b"]),
    case("error-double-dash-options-become-arguments", ["--", "-m", "x"]),
    case("error-double-dash-then-unknown-subcommand", ["-m", "a", "--", "b"]),
    case("error-run-double-dash-extra", ["run", "--", "x"]),
    case("argfile-not-expanded", ["@args.txt"], files={"args.txt": "-m\nstub.yaml\n", "stub.yaml": STUB},
         deviation="D12: no @argfile expansion; Kotlin reads args.txt and runs with -m stub.yaml",
         rust={"stdout": "", "stderr": "Usage: marathon [<options>] <command> [<args>]...\n\nError: no such subcommand @args.txt\n",
               "exit": 1}),
    # Values, defaults and placement; every case below stops at the missing-Marathonfile check or a config error.
    case("bare-invocation-runs", []),
    case("double-dash-alone-runs", ["--"]),
    case("double-dash-then-run", ["--", "run"]),
    case("run-double-dash", ["run", "--"]),
    case("run-default-marathonfile", ["run"]),
    case("root-m-space", ["-m", "missing.yaml"]),
    case("root-m-attached", ["-mmissing.yaml"]),
    case("root-m-equals", ["-m=missing.yaml"]),
    case("root-long-equals", ["--marathonfile=missing.yaml"]),
    case("root-long-space", ["--marathonfile", "missing.yaml"]),
    case("root-m-repeated-last-wins", ["-m", "a.yaml", "-m", "b.yaml"]),
    case("root-m-takes-option-like-value", ["-m", "--bogus"]),
    case("root-m-takes-subcommand-name", ["-m", "run"]),
    case("root-options-dropped-for-run", ["-m", "x.yaml", "--bugsnag=false", "run"]),
    case("root-grouped-value-dropped-for-run", ["-mx", "run"]),
    case("root-options-dropped-for-version", ["-m", "x", "version"]),
    case("root-bad-boolean-dropped-for-version", ["--bugsnag=a=b", "version"]),
    case("booleans-true-any-case", ["--analyticsTracking=TRUE", "--bugsnag", "tRuE"]),
    case("booleans-false-forms", ["--analyticsTracking", "yes", "--bugsnag=1"]),
    case("booleans-empty-value", ["--bugsnag="]),
    case("run-booleans", ["run", "--bugsnag=no", "--analyticsTracking=True", "-m", "missing.yaml"]),
    case("parse-defaults", ["parse"]),
    case("parse-all-options", ["parse", "-m", "missing.yaml", "-o", "tests", "-f", "TRUE"]),
    case("parse-long-options", ["parse", "--marathonfile=missing.yaml", "--output=tests", "--include-flaky-tests=false"]),
    case("parse-flaky-yes", ["parse", "--include-flaky-tests", "yes"]),
    case("parse-output-empty", ["parse", "-o", ""]),
    case("parse-output-repeated", ["parse", "-o", "a", "-o", "b", "-m", "x.yaml"]),
    # version
    case("version", ["version"]),
    case("version-double-dash", ["version", "--"]),
    # The Marathonfile check.
    case("missing-relative-dot", ["-m", "./sub/../missing.yaml"]),
    case("marathonfile-is-directory", ["-m", "dir.yaml"], dirs=["dir.yaml"]),
    case("marathonfile-dangling-symlink", ["-m", "link.yaml"], symlinks={"link.yaml": "nowhere.yaml"}),
    case("marathonfile-symlink-to-config", ["-m", "link.yaml"], files={"real/stub.yaml": STUB},
         symlinks={"link.yaml": "real/stub.yaml"}, d1=True),
    case("marathonfile-unreadable", ["-m", "locked.yaml"], files={"locked.yaml": STUB}, modes={"locked.yaml": "000"}),
    # Configuration errors escape execute: uncaught, exit 1.
    case("config-binding-error", ["-m", "bad.yaml"], files={"bad.yaml": BAD_KEY}),
    case("config-no-vendor", ["run", "-m", "novendor.yaml"], files={"novendor.yaml": NO_VENDOR}),
    case("config-empty-vendor", ["-m", "empty.yaml"], files={"empty.yaml": EMPTY_VENDOR}),
    case("config-null-document", ["-m", "null.yaml"], files={"null.yaml": NULL_DOC}),
    case("config-empty-file", ["-m", "blank.yaml"], files={"blank.yaml": ""}),
    case("config-substitution-cycle", ["-m", "cycle.yaml"], files={"cycle.yaml": CYCLE}, env={"A": "${A}"}),
    case("config-default-marathonfile-binding-error", [], files={"Marathonfile": BAD_KEY}),
    case("stub-gate-run", ["-m", "stub.yaml"], files={"stub.yaml": STUB}, d1=True),
    case("stub-gate-run-flags-false", ["run", "-m", "stub.yaml", "--bugsnag=false", "--analyticsTracking=false"],
         files={"stub.yaml": STUB}),
    case("stub-gate-subdir", ["-m", "conf/stub.yaml"], files={"conf/stub.yaml": STUB}, d1=True),
    case("parse-stub-gate", ["parse", "-m", "stub.yaml", "-o", "tests"], files={"stub.yaml": STUB}),
    case("parse-binding-error", ["parse", "-m", "bad.yaml"], files={"bad.yaml": BAD_KEY}),
    case("parse-no-vendor", ["parse", "--marathonfile", "novendor.yaml", "-f", "true"], files={"novendor.yaml": NO_VENDOR}),
    # Clikt splits a short-option group by UTF-16 unit; the lone high surrogate prints as '?'.
    case("error-unknown-short-supplementary", ["-\U0001F600"],
         deviation="simplification (WP 2.5 R1-sol-3): short options iterate Unicode scalars, so the error echoes the"
                   " emoji where Kotlin prints -?",
         rust_stderr="Usage: marathon [<options>] <command> [<args>]...\n\nError: no such option -\U0001F600\n"),
    # config-dump parses each --now as it reads it (ConfigDump.kt's Instant.parse in the argument loop): a bad instant
    # is an uncaught DateTimeParseException, exit 1, before the usage checks and the env file.
    case("config-dump-now-invalid", ["--now", "bogus"], program="config-dump"),
    case("config-dump-now-invalid-extra-arguments", ["--now", "bogus", "a", "b"], program="config-dump"),
    case("config-dump-now-invalid-before-env-file", ["--now", "bogus", "--env-file", "missing", "f"], program="config-dump"),
    case("config-dump-now-invalid-then-valid", ["missing", "--now", "bad", "--now", "2026-01-01T00:00:00Z"],
         program="config-dump"),
    case("config-dump-now-unparsed-text", ["missing", "--now", "2026-01-01T00:00:00Zx"], program="config-dump"),
    case("config-dump-now-truncated", ["missing", "--now", "2026-01-01T00:00"], program="config-dump"),
    case("config-dump-now-invalid-date", ["missing", "--now", "2026-02-30T00:00:00Z"], program="config-dump"),
    case("config-dump-now-offset-hour-24", ["missing", "--now", "2026-01-01T00:00:00+24:00"], program="config-dump"),
    case("config-dump-now-missing-value", ["missing", "--now"], program="config-dump"),
    case("config-dump-now-valid-no-file", ["--now", "2026-01-01T00:00:00Z"], program="config-dump"),
    case("config-dump-env-file-then-invalid-now", ["--env-file", "missing", "f", "--now", "x"], program="config-dump"),
]


def normalise(text, sandbox_spellings):
    lines = []
    for line in text.split("\n"):
        if line.startswith("SLF4J(I): Connected with provider of type "):
            continue
        if line.startswith("\tat ") or re.fullmatch(r"\t\.\.\. \d+ (more|common frames omitted)", line):
            continue
        if line.endswith(" - Noop exceptions reporter finished"):
            continue
        for s in sandbox_spellings:
            line = line.replace(s, "<SANDBOX>")
        line = re.sub(r"^\d\d:\d\d:\d\d\.\d\d\d ", "<TIME> ", line)
        line = re.sub(r"VersionCommandCliConfiguration@[0-9a-f]+", "VersionCommandCliConfiguration@<HASH>", line)
        line = line.replace(f"Starting marathon v{VERSION},", "Starting marathon v<VERSION>,")
        if line == VERSION:
            line = "<VERSION>"
        if re.fullmatch(r"<TIME> \[main\] INFO  \S+ - Noop exceptions reporter started", line):
            line = "<TIME> [main] INFO  <D1>"
        lines.append(line)
    return "\n".join(lines)


def prepare(sandbox, c):
    inp = os.path.join(sandbox, "in")
    for d in ("in", "home", "tmp"):
        os.makedirs(os.path.join(sandbox, d))
    for d in c["dirs"]:
        os.makedirs(os.path.join(inp, d), exist_ok=True)
    for path, content in c["files"].items():
        p = os.path.join(inp, path)
        os.makedirs(os.path.dirname(p), exist_ok=True)
        with open(p, "w", encoding="utf-8") as f:
            f.write(content)
    for path, target in c["symlinks"].items():
        os.symlink(target, os.path.join(inp, path))
    for path, mode in c["modes"].items():
        os.chmod(os.path.join(inp, path), int(mode, 8))
    return inp


def main():
    if not os.access(CLI, os.X_OK):
        sys.exit(f"no reference marathon at {CLI}: run tools/oracle/build.sh reference or set MARATHON_CLI")
    if not os.access(CONFIG_DUMP, os.X_OK):
        sys.exit(f"no config-dump at {CONFIG_DUMP}: set CONFIG_DUMP")
    run_dir = os.path.join(ROOT, "tools/oracle/runs", time.strftime("cli-matrix-%Y%m%d%H%M%S") + f"-{os.getpid()}")
    rows = []
    for c in CASES:
        sandbox = os.path.join(run_dir, c["id"])
        inp = prepare(sandbox, c)
        env = {"HOME": os.path.join(sandbox, "home"), "TMPDIR": os.path.join(sandbox, "tmp") + "/",
               "PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "JAVA_HOME": JAVA_HOME, **c["env"]}
        program = CONFIG_DUMP if c["program"] == "config-dump" else CLI
        p = subprocess.run([program, *c["argv"]], cwd=inp, env=env, stdin=subprocess.DEVNULL, capture_output=True, timeout=120)
        spellings = sorted({inp, os.path.realpath(inp)}, key=len, reverse=True)
        out = normalise(p.stdout.decode("utf-8"), spellings)
        err = normalise(p.stderr.decode("utf-8"), spellings)
        for path in c["modes"]:
            os.chmod(os.path.join(inp, path), 0o644)
        row = {k: v for k, v in c.items() if k not in ("d1", "rust_stderr")}
        if c["rust_stderr"] is not None:
            row["rust"] = {"stdout": out, "stderr": c["rust_stderr"], "exit": p.returncode}
        if c["d1"]:
            # The D1 line follows "Checking ... config" and precedes the gate's uncaught exception (stderr).
            assert out.endswith(" config\n"), out
            row["rust"] = {"stdout": out + D1_LINE, "stderr": err, "exit": p.returncode}
        rows.append({**row, "stdout": out, "stderr": err, "exit": p.returncode})
        print(f"{c['id']:<48} exit {p.returncode}", file=sys.stderr)
    header = {
        "table": "cli-matrix",
        "description": "WP 2.5: the Kotlin CLI per argv/inputs/environment, stdout, stderr and exit status after normalisation",
        "schema": {
            "id": "case name", "program": "marathon, or config-dump (Rust: marathon config-dump)",
            "argv": "arguments after the program name",
            "files": "input files (path relative to the working directory -> UTF-8 content)",
            "dirs": "directories to create", "symlinks": "symlinks to create (path -> target)",
            "modes": "octal modes to set after creating the inputs",
            "env": "variables added to HOME, TMPDIR and PATH (and JAVA_HOME for the Kotlin build)",
            "deviation": "null, or the approved deviation that changes this row for Rust",
            "rust": "null, or the Rust expectation (stdout, stderr, exit) where 'deviation' applies",
            "stdout": "normalised stdout", "stderr": "normalised stderr", "exit": "exit status",
        },
        "normalisation": NORMALISATION,
        "rows": len(rows),
        "jdk": subprocess.run([os.path.join(JAVA_HOME, "bin/java"), "-XshowSettings:properties", "-version"],
                              capture_output=True, text=True).stderr.split("java.runtime.version = ")[1].split("\n")[0],
        "generator": "tools/oracle/cli-matrix/generate.py (reference :cli:installDist of e47352f2; tools/oracle/config-dump)",
    }
    with open(OUT, "w", encoding="utf-8") as f:
        f.write(json.dumps(header, ensure_ascii=False, separators=(",", ":")) + "\n")
        for r in rows:
            f.write(json.dumps(r, ensure_ascii=False, separators=(",", ":")) + "\n")
    shutil.rmtree(run_dir)
    print(f"wrote {OUT}: {len(rows)} rows", file=sys.stderr)


if __name__ == "__main__":
    main()
