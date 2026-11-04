#!/usr/bin/env python3
"""Writes an archive's THIRD-PARTY-LICENSES to stdout: what marathon-cli compiles for one target.

Crates come from cargo. Non-crate files compiled in come from embedded_assets.json, and the build fails when that list
and the production `include_*!` calls disagree.

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
ASSETS = Path(__file__).resolve().with_name('embedded_assets.json')
INCLUDE = re.compile(r'include_(?:bytes|str|dir)\s*!\s*(?:\(\s*"([^"\\]+)"\s*,?\s*\))?')
TEST_ITEM = re.compile(r'#\[(?:cfg\((?:all\()?test\b[^\]]*|test|tokio::test[^\]]*)\]')
# Packages that never reach the marathon binary.
NOT_COMPILED_IN = ('crates/marathon-test-support/',)


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


def strip_test_items(text):
    """Blanks items behind #[cfg(test)] or #[test]: to their matching brace, or their `;` if that comes first."""
    out, position = [], 0
    for match in TEST_ITEM.finditer(text):
        if match.start() < position:
            continue
        out.append(text[position:match.start()])
        position = match.end()
        depth = 0
        while position < len(text):
            character = text[position]
            position += 1
            if character == '{':
                depth += 1
            elif character == '}':
                depth -= 1
                if depth == 0:
                    break
            elif character == ';' and depth == 0:
                break
    out.append(text[position:])
    return ''.join(out)


def production_includes(root):
    """Repository paths of the files that production Rust code embeds with include_bytes!, include_str! or include_dir!."""
    root = Path(root)
    sources = [path for base in ('crates', 'vendor') for path in (root / base).glob('*/src/**/*.rs')]
    test_modules = set()
    for path in sources:
        text = path.read_text(encoding='utf-8', errors='replace')
        for name in re.findall(r'#\[cfg\(test\)\]\s*(?:pub(?:\([a-z]+\))? )?mod (\w+);', text):
            base = path.parent if path.name in ('mod.rs', 'lib.rs', 'main.rs') else path.with_suffix('')
            test_modules.update({base / (name + '.rs'), base / name / 'mod.rs'})
    found = set()
    for path in sources:
        relative = path.relative_to(root).as_posix()
        if path in test_modules or relative.startswith(NOT_COMPILED_IN) or path.name == 'tests.rs' or '/tests/' in relative:
            continue
        code = strip_test_items(path.read_text(encoding='utf-8', errors='replace'))
        code = '\n'.join(line for line in code.split('\n') if not line.lstrip().startswith('//'))
        for match in INCLUDE.finditer(code):
            # Only a plain string literal can be checked against the list; a computed path could embed anything.
            if not match.group(1):
                raise ValueError(f'{relative}: cannot resolve the path of {match.group(0)}; use a string literal')
            found.add(os.path.relpath(path.parent / match.group(1), root).replace(os.sep, '/'))
    return found


def load_assets(root=ROOT):
    """The asset list, after checking it against what production code embeds."""
    manifest = json.loads(Path(root, 'scripts/release/embedded_assets.json').read_text(encoding='utf-8'))
    listed = {name for asset in manifest['assets'] for name in asset['files']}
    embedded = production_includes(root)
    if listed != embedded:
        raise ValueError(f'embedded_assets.json differs from the production include_*! calls: '
                         f'not listed {sorted(embedded - listed)}, not embedded {sorted(listed - embedded)}')
    return manifest


def render_assets(manifest, root=ROOT):
    lines = ['', RULE, 'Other files compiled into marathon, not crates', RULE, '']
    for asset in manifest['assets']:
        lines.append(f'{asset["component"]} {asset["version"]}: {asset["spdx"]}')
    for asset in manifest['assets']:
        lines += ['', RULE, f'{asset["component"]}', f'Version: {asset["version"]}', f'SPDX expression: {asset["spdx"]}',
                  f'Source: {asset["source"]}', 'Files: ' + ', '.join(asset['files']), RULE]
        if asset['note']:
            lines += ['', asset['note']]
        for name in asset['licences']:
            text = Path(root, name).read_text(encoding='utf-8').rstrip()
            lines += ['', f'--- {Path(name).name}', '', text]
    adam = manifest['adam']
    lines += ['', RULE, f'adam {adam["version"]}', f'SPDX expression: {adam["spdx"]}', f'Source: {adam["source"]}', RULE, '',
              adam['note'], '', Path(root, adam['licence']).read_text(encoding='utf-8').rstrip()]
    return lines


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
             'Files compiled in that are not crates follow the crates, in their own section.', '',
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
    lines += render_assets(load_assets())
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
