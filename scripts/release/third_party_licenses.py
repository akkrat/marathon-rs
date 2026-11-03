#!/usr/bin/env python3
"""Writes an archive's THIRD-PARTY-LICENSES to stdout: what marathon-cli compiles for one target.

`cargo tree -e normal` picks the crates, because it resolves features as `cargo build -p marathon-cli`
does. `cargo metadata` resolves the whole workspace at once and would list crates that only another
member's features pull in. Metadata then supplies each crate's licence and source directory. Neither
command needs anything beyond cargo itself, and neither looks at dev or build dependencies.
"""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
ROOT_PACKAGE = 'marathon-cli'
LICENCE_NAME = re.compile(r'(licen[cs]e|copying|notice|copyright|unlicense)', re.IGNORECASE)
# Directories holding one licence per file, whatever each file is called (the REUSE layout).
LICENCE_DIRECTORIES = ('licenses', 'licences')
# license.rs and the like are code about licences, not licence texts.
CODE_SUFFIXES = ('.rs', '.c', '.cc', '.cpp', '.h', '.hpp', '.s', '.asm', '.pl', '.py', '.sh', '.go', '.toml',
                 '.json', '.yml', '.yaml', '.cmake')
RULE = '=' * 80


def cargo(*arguments):
    return subprocess.run(['cargo', *arguments], cwd=ROOT, check=True, encoding='utf-8',
                          stdout=subprocess.PIPE).stdout


def compiled_crates(target):
    tree = cargo('tree', '--locked', '-p', ROOT_PACKAGE, '-e', 'normal', '--target', target,
                 '--prefix', 'none', '--format', '{p}')
    # A line is `name vX.Y.Z`, then optional notes such as the path, `(proc-macro)` or `(*)`.
    return {tuple(line.split()[:2]) for line in tree.splitlines() if line.strip()}


def select_packages(metadata, crates):
    """The metadata package of each compiled crate, without this repository's own workspace members."""
    workspace = set(metadata['workspace_members'])
    known = {}
    for package in metadata['packages']:
        known.setdefault((package['name'], 'v' + package['version']), []).append(package)
    selected = []
    for crate in sorted(crates):
        packages = known.get(crate, [])
        if len(packages) != 1:
            raise ValueError(f'expected one metadata package for {" ".join(crate)}, found {len(packages)}')
        if packages[0]['id'] not in workspace:
            selected.append(packages[0])
    return selected


def licence_files(directory):
    """Licence-like files under a crate, relative to it; bundled C libraries keep theirs in subdirectories."""
    found = []
    for parent, directories, files in os.walk(directory):
        directories[:] = [d for d in directories if d not in ('target', '.git')]
        in_licence_directory = Path(parent).name.lower() in LICENCE_DIRECTORIES
        for name in files:
            named = LICENCE_NAME.match(name) and not name.lower().endswith(CODE_SUFFIXES)
            if named or in_licence_directory:
                found.append(Path(parent, name).relative_to(directory).as_posix())
    return sorted(found)


def source_description(package, workspace_root):
    if package.get('source'):
        return package['source']
    directory = Path(package['manifest_path']).parent
    return 'vendored in this repository at ' + directory.relative_to(workspace_root).as_posix()


def render(target, metadata, crates):
    packages = select_packages(metadata, crates)
    # Crate-relative names only: an absolute path would put the build machine into a published file.
    lines = [f'Third-party Rust crates compiled into marathon for {target}', '',
             "Generated from Cargo.lock when this archive was built. It lists marathon-cli's normal dependency",
             'graph for this target, including procedural macros, which run while compiling. Dev and build',
             "dependencies are not listed. This repository's own crates are covered by the file LICENSE.",
             'Only crates are listed: assets copied from the Kotlin marathon source and compiled into the',
             'binary, such as the HTML report bundles and the libxctest-parser libraries, are not.', '',
             f'{len(packages)} crates:', '']
    for package in packages:
        lines.append(f'{package["name"]} {package["version"]}: {package.get("license") or "no licence declared"}')
    for package in packages:
        directory = Path(package['manifest_path']).parent
        lines += ['', RULE, f'{package["name"]} {package["version"]}',
                  f'SPDX expression: {package.get("license") or "none declared"}',
                  f'Source: {source_description(package, metadata["workspace_root"])}', RULE]
        files = licence_files(directory)
        if not files:
            lines += ['', 'The crate source contains no licence file. Its declared licence is: '
                      + (package.get('license') or 'none declared')]
        for name in files:
            text = (directory / name).read_bytes().decode('utf-8', 'replace')
            lines += ['', f'--- {name}', '', text.rstrip()]
    return '\n'.join(lines) + '\n'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--target', required=True)
    args = parser.parse_args()
    metadata = json.loads(cargo('metadata', '--locked', '--format-version', '1', '--filter-platform', args.target))
    sys.stdout.reconfigure(encoding='utf-8')
    sys.stdout.write(render(args.target, metadata, compiled_crates(args.target)))


if __name__ == '__main__':
    try:
        main()
    except (OSError, ValueError, KeyError, subprocess.SubprocessError) as error:
        print(f'third_party_licenses: {error}', file=sys.stderr)
        sys.exit(1)
