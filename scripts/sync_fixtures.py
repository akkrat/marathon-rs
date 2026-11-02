#!/usr/bin/env python3
"""Verify that fixtures/ holds byte-identical copies of the Kotlin test-resources directories.

Usage: scripts/sync_fixtures.py [--write] /path/to/marathon

Without --write it compares every file in both directions and exits non-zero on any difference.
With --write it replaces each destination directory with a fresh copy of its source.
fixtures/PROVENANCE.md is not managed by this script.
"""
import filecmp
import pathlib
import shutil
import sys

# (source directory relative to the Kotlin repo, destination directory relative to fixtures/)
MAPPING = [
    ("core/src/test/resources", "core"),
    ("configuration/src/test/resources", "configuration"),
    ("report/execution-timeline/src/test/resources", "report-execution-timeline"),
    ("vendor/vendor-android/src/test/resources", "vendor-android"),
    ("vendor/vendor-apple/ios/src/test/resources", "vendor-apple-ios"),
    ("vendor/vendor-apple/base/src/test/resources", "vendor-apple-base"),
]

FIXTURES = pathlib.Path(__file__).resolve().parent.parent / "fixtures"


def files_under(d: pathlib.Path):
    return {str(p.relative_to(d)) for p in d.rglob("*") if p.is_file() or p.is_symlink()}


def verify(root: pathlib.Path) -> int:
    problems = 0
    total = 0
    for src_rel, dst_rel in MAPPING:
        src, dst = root / src_rel, FIXTURES / dst_rel
        if not dst.is_dir():
            print(f"MISSING {dst}")
            problems += 1
            continue
        src_files, dst_files = files_under(src), files_under(dst)
        for rel in sorted(src_files - dst_files):
            print(f"MISSING fixtures/{dst_rel}/{rel}")
            problems += 1
        for rel in sorted(dst_files - src_files):
            print(f"EXTRA   fixtures/{dst_rel}/{rel}")
            problems += 1
        for rel in sorted(src_files & dst_files):
            total += 1
            if not filecmp.cmp(src / rel, dst / rel, shallow=False):
                print(f"DIFFERS fixtures/{dst_rel}/{rel}")
                problems += 1
    print(f"fixtures: {total} files compared, {problems} problems")
    return 1 if problems else 0


def write(root: pathlib.Path) -> int:
    for src_rel, dst_rel in MAPPING:
        dst = FIXTURES / dst_rel
        if dst.exists():
            shutil.rmtree(dst)
        shutil.copytree(root / src_rel, dst, symlinks=True)
        print(f"{src_rel} -> fixtures/{dst_rel}")
    return verify(root)


def main() -> int:
    args = sys.argv[1:]
    do_write = "--write" in args
    args = [a for a in args if a != "--write"]
    if len(args) != 1:
        print(__doc__, file=sys.stderr)
        return 2
    root = pathlib.Path(args[0]).resolve()
    return write(root) if do_write else verify(root)


if __name__ == "__main__":
    sys.exit(main())
