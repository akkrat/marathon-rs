#!/usr/bin/env python3
"""Writes the receipt of one successful tools/oracle/build.sh build (read by tools/real-e2e/run reference).

build.sh calls this only after its tree check and the Gradle build have succeeded; it deletes the previous receipt before
building, so a receipt exists exactly when the installed jars were produced by a verified build of its source.
"""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import sys

FORMAT = 'marathon-oracle/build-receipt@1'


def sha256(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def receipt(variant, source, src, verified_tree, patches, java):
    src = Path(src)
    head = subprocess.run(['git', '-C', str(src), 'rev-parse', 'HEAD'], check=True, capture_output=True, text=True).stdout.strip()
    jars = {p.name: sha256(p) for p in sorted((src / 'cli/build/install/marathon/lib').glob('*.jar'))}
    if not jars:
        raise SystemExit('build-receipt: the build installed no jars')
    return dict(format=FORMAT, variant=variant, source=source, head=head, verified_tree=verified_tree,
                patches={Path(p).name: sha256(p) for p in patches}, jars=jars, gradle_exit=0, java=java)


def main(argv):
    p = argparse.ArgumentParser()
    p.add_argument('--variant', required=True)
    p.add_argument('--source', required=True)
    p.add_argument('--src', required=True)
    p.add_argument('--verified-tree', required=True)
    p.add_argument('--java', default='')
    p.add_argument('--out', required=True)
    p.add_argument('patches', nargs='+')
    args = p.parse_args(argv)
    value = receipt(args.variant, args.source, args.src, args.verified_tree, args.patches, args.java)
    out = Path(args.out)
    out.write_text(json.dumps(value, indent=2, sort_keys=True) + '\n')
    print('build receipt: ' + str(out))


if __name__ == '__main__':
    main(sys.argv[1:])
