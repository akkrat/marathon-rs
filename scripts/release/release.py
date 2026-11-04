#!/usr/bin/env python3
"""Native distribution tooling; source contracts: cli/build.gradle.kts and .gitlab-ci.yml at e47352f2.

GitHub publication follows docs/wp/7.3.md's user decisions, replacing the reference registry.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
import tempfile
import zipfile

ROOT = Path(__file__).resolve().parents[2]
DARWIN = 'aarch64-apple-darwin'
MUSL = 'x86_64-unknown-linux-musl'
GNU = 'x86_64-unknown-linux-gnu'
# TARGETS are exactly the archives a release carries. GNU stays a gate platform and a local build
# (user decision 2026-11-01, docs/wp/7.3b.md); `collect` never attaches it.
TARGETS = (DARWIN, MUSL)
ALL_TARGETS = TARGETS + (GNU,)
# The musl target is native-arch: zig is only the C toolchain for `ring` and `aws-lc-sys`.
MUSL_HOSTS = (GNU, MUSL)
CHANGELOG = ROOT / 'CHANGELOG.md'
LICENSE = ROOT / 'LICENSE'
THIRD_PARTY_SCRIPT = Path(__file__).resolve().with_name('third_party_licenses.py')
ASSET_LIST = Path(__file__).resolve().with_name('embedded_assets.json')
# The tag is for readers; the digest is what Docker resolves. It names the multi-architecture index.
ALPINE_IMAGE = 'alpine:3.24.3@sha256:be2656061b536c1883c4034790e21d0a09c5aaaf51968b89368a3c0371ea2c9a'
# Numeric identifiers without leading zeros, as in semver. No build metadata: `+` has no tag use here.
_NUMBER = r'(?:0|[1-9][0-9]*)'
_PRERELEASE_IDENTIFIER = rf'(?:{_NUMBER}|[0-9]*[A-Za-z-][0-9A-Za-z-]*)'
RELEASE_VERSION = re.compile(rf'{_NUMBER}\.{_NUMBER}\.{_NUMBER}(?:-{_PRERELEASE_IDENTIFIER}(?:\.{_PRERELEASE_IDENTIFIER})*)?')


def run(argv, **kwargs):
    return subprocess.run([str(a) for a in argv], check=True, text=True,
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE, **kwargs).stdout


def version_value(value):
    # A conservative release boundary; direct Cargo builds retain verbatim Kotlin semantics.
    if not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._+-]{0,127}', value) or '..' in value:
        raise ValueError('release version must be 1–128 safe ASCII characters, without traversal')
    return value


def strict_version(value):
    # What a tag or a dry run may publish; version_value stays the wider rule for local builds.
    if not RELEASE_VERSION.fullmatch(value):
        raise ValueError(f'release version must be X.Y.Z or X.Y.Z-prerelease, without a v: {value!r}')
    return value


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def inspect_env():
    # The static check matches English tool messages; a localized glibc would reject a valid binary.
    return dict(os.environ, LC_ALL='C', LANG='C')


def ldd_output(binary):
    # glibc ldd exits non-zero for a static binary, so this cannot go through run().
    result = subprocess.run(['ldd', str(binary)], text=True, stdout=subprocess.PIPE,
                            stderr=subprocess.STDOUT, check=False, env=inspect_env())
    return result.stdout


def require_static(description, ldd):
    if 'statically linked' not in description and 'static-pie linked' not in description:
        raise ValueError(f'binary is not statically linked (file: {description.strip()})')
    if 'interpreter' in description:
        raise ValueError(f'binary requests a dynamic loader (file: {description.strip()})')
    if 'not a dynamic executable' not in ldd and 'statically linked' not in ldd:
        raise ValueError(f'ldd reports a dynamic executable: {ldd.strip()}')


def zig_toolchain():
    try:
        zig = run(['zig', 'version']).strip()
        zigbuild = run(['cargo-zigbuild', '--version']).strip()
    except (OSError, subprocess.CalledProcessError) as error:
        raise ValueError(f'the musl target needs zig and cargo-zigbuild on PATH (docs/wp/7.3b-findings.md): {error}')
    return dict(zig=zig, cargo_zigbuild=zigbuild)


def filename(version, target):
    return f'marathon-{version}-{target}.zip'


def archive_modes(version):
    root = f'marathon-{version}'
    return {f'{root}/LICENSE': 0o644, f'{root}/THIRD-PARTY-LICENSES': 0o644, f'{root}/bin/marathon': 0o755}


def check_archive(z, version):
    expected = archive_modes(version)
    names = z.namelist()
    if len(names) != len(set(names)) or set(names) != set(expected):
        raise ValueError(f'unexpected archive payload: {names}')
    for name, mode in expected.items():
        if z.getinfo(name).external_attr >> 16 != stat.S_IFREG | mode:
            raise ValueError(f'archive mode of {name} must be {mode:04o}')


def check_cli(argv, version, returncode, stdout, stderr):
    rows = [json.loads(line) for line in (ROOT / 'fixtures/oracle/cli-matrix.jsonl').read_text().splitlines()]
    expected = next((row for row in rows[1:] if row['argv'] == argv and row['program'] == 'marathon'), None)
    if expected is None:
        raise ValueError(f'CLI matrix has no marathon row for {argv}')
    expected = expected.get('rust') or expected
    out = re.sub(r'^\d{2}:\d{2}:\d{2}\.\d{3}', '<TIME>', stdout, flags=re.M)
    if argv == ['version']:
        out = out.replace(f'Starting marathon v{version},', 'Starting marathon v<VERSION>,')
        lines = out.splitlines(keepends=True)
        if not lines or lines[-1] != version + '\n':
            raise ValueError('embedded version mismatch')
        lines[-1] = '<VERSION>\n'
        out = ''.join(lines)
        out = re.sub(r'(VersionCommandCliConfiguration@)[0-9a-f]+', r'\1<HASH>', out)
    if (returncode, out, stderr) != (expected['exit'], expected['stdout'], expected['stderr']):
        raise ValueError(f'extracted CLI expectation mismatch: {argv}: {returncode!r} {stdout!r} {stderr!r}')


def smoke(archive, version):
    root = f'marathon-{version}'
    with tempfile.TemporaryDirectory(prefix='marathon smoke with spaces ') as tmp:
        tmp = Path(tmp)
        with zipfile.ZipFile(archive) as z:
            check_archive(z, version)
        run(['unzip', '-q', Path(archive).resolve(), '-d', tmp])
        binary = tmp / root / 'bin/marathon'
        for name, mode in archive_modes(version).items():
            if stat.S_IMODE((tmp / name).stat().st_mode) != mode:
                raise ValueError(f'unzip did not preserve the mode of {name}')
        if (tmp / root / 'LICENSE').read_bytes() != LICENSE.read_bytes():
            raise ValueError('archive LICENSE differs from the repository')
        if not (tmp / root / 'THIRD-PARTY-LICENSES').stat().st_size:
            raise ValueError('archive THIRD-PARTY-LICENSES is empty')
        cwd = tmp / 'unrelated cwd'
        cwd.mkdir()
        no_java = tmp / 'empty PATH'
        no_java.mkdir()
        env = {'PATH': str(no_java), 'HOME': str(cwd), 'TMPDIR': str(cwd),
               'GIT_TAG_NAME': 'runtime-conflict', 'JAVA_HOME': '/missing-java',
               'MARATHON_OPTS': 'ignored', 'JAVA_OPTS': 'ignored'}
        for argv in (['version'], ['--help']):
            result = subprocess.run([str(binary), *argv], cwd=cwd, env=env, capture_output=True,
                                    text=True, timeout=30, check=False)
            check_cli(argv, version, result.returncode, result.stdout, result.stderr)


def prove_musl(archive, version):
    """Runs the packaged binary in a container with no glibc, from the zip, never from the build tree."""
    with tempfile.TemporaryDirectory(prefix='marathon-musl-proof-') as tmp:
        with zipfile.ZipFile(archive) as z:
            check_archive(z, version)
        run(['unzip', '-q', Path(archive).resolve(), '-d', tmp])
        # Pull first: the pull's progress goes to stderr, which the CLI comparison below checks.
        run(['docker', 'pull', ALPINE_IMAGE])
        container = ['docker', 'run', '--rm', '--network', 'none', '--read-only', '-v', f'{tmp}:/proof:ro', ALPINE_IMAGE]
        run([*container, 'sh', '-c', 'test -e /lib/ld-musl-x86_64.so.1 && ! ls /lib/ld-linux* /lib64/ld-linux* 2>/dev/null'])
        for argv in (['version'], ['--help']):
            result = subprocess.run([*container, f'/proof/marathon-{version}/bin/marathon', *argv],
                                    capture_output=True, text=True, timeout=120, check=False)
            check_cli(argv, version, result.returncode, result.stdout, result.stderr)


def third_party_text(target):
    # A subprocess, not an import: `python3 -I` leaves this script's directory off sys.path.
    return run([sys.executable, '-I', THIRD_PARTY_SCRIPT, '--target', target], cwd=ROOT, encoding='utf-8')


def manifest_rows(version, contents):
    return [dict(path=name, mode=f'{mode:04o}', sha256=hashlib.sha256(contents[name]).hexdigest())
            for name, mode in archive_modes(version).items()]


def package(binary, output, version, target, toolchain=None):
    if not binary.is_file() or not os.access(binary, os.X_OK):
        raise ValueError('missing or nonexecutable binary')
    # -b: the description without the file name, which may itself contain ': '.
    description = run(['file', '-b', binary], env=inspect_env())
    required = ('Mach-O', 'arm64') if target == DARWIN else ('ELF', 'x86-64')
    if not all(part in description for part in required):
        raise ValueError(f'binary does not match {target}')
    musl_ldd = None
    if target == MUSL and not toolchain:
        # Without the zig/cargo-zigbuild versions that built it, the archive would carry no C-toolchain provenance.
        raise ValueError('the musl target is packaged only by `build`, which records its build-tool versions')
    if target == MUSL:
        # A "musl" binary that still needs a loader would fail on exactly the hosts it is meant for.
        musl_ldd = ldd_output(binary)
        require_static(description, musl_ldd)
    output.mkdir(parents=True, exist_ok=True)
    archive = output / filename(version, target)
    metadata = output / f'{target}.json'
    if archive.exists() or metadata.exists():
        raise ValueError('refusing to replace existing package outputs')
    root = f'marathon-{version}'
    contents = {f'{root}/LICENSE': LICENSE.read_bytes(),
                f'{root}/THIRD-PARTY-LICENSES': third_party_text(target).encode('utf-8'),
                f'{root}/bin/marathon': binary.read_bytes()}
    with tempfile.TemporaryDirectory(dir=output) as tmp:
        staged = Path(tmp) / archive.name
        with zipfile.ZipFile(staged, 'w', compression=zipfile.ZIP_DEFLATED) as z:
            for name, mode in archive_modes(version).items():
                info = zipfile.ZipInfo(name)
                info.create_system = 3
                info.external_attr = (stat.S_IFREG | mode) << 16
                info.compress_type = zipfile.ZIP_DEFLATED
                z.writestr(info, contents[name])
        smoke(staged, version)
        if target == DARWIN:
            deps, platform_info = run(['otool', '-L', binary]), run(['otool', '-l', binary])
        elif target == MUSL:
            deps, platform_info = musl_ldd, run(['readelf', '-lW', binary], env=inspect_env())
        else:
            deps, platform_info = run(['ldd', binary]), run(['readelf', '--version-info', binary])
        data = dict(version=version, target=target, archive=archive.name, sha256=digest(staged),
                    source_sha=run(['git', 'rev-parse', 'HEAD'], cwd=ROOT).strip(),
                    rustc=run(['rustc', '-Vv']), dependencies=deps, platform=platform_info,
                    manifest=manifest_rows(version, contents))
        if target == MUSL:
            data.update(toolchain=toolchain or {}, file=description.strip())
        staged.rename(archive)
        metadata.write_text(json.dumps(data, indent=2) + '\n')
    return archive


def changelog_section(version):
    """The body of the `## <version>` section, exactly; another version's section is never returned."""
    heading = f'## {version}'
    lines = CHANGELOG.read_text(encoding='utf-8').splitlines()
    starts = [n for n, line in enumerate(lines) if line.strip() == heading]
    if len(starts) != 1:
        raise ValueError(f'CHANGELOG.md must have exactly one section "{heading}", found {len(starts)}')
    body = []
    for line in lines[starts[0] + 1:]:
        if line.startswith('## '):
            break
        body.append(line)
    text = '\n'.join(body).strip()
    if not text:
        raise ValueError(f'CHANGELOG.md section "{heading}" is empty')
    return text


def release_notes(version, sha, assets, repo):
    section = changelog_section(version)
    # CHANGELOG.md links are relative to the repository; a release page would break them.
    section = re.sub(r'\]\((?![a-z]+:|#)([^)]+)\)', rf'](https://github.com/{repo}/blob/{version}/\1)', section)
    labels = {DARWIN: 'Apple Silicon macOS', MUSL: 'x86_64 Linux (static, any distribution)'}
    rows = ''.join(f'| {labels[target]} | `{asset.name}` | `{digest(asset)}` |\n' for target, asset in zip(TARGETS, assets))
    url = f'https://github.com/{repo}/releases/download/{version}'
    sample = f'marathon-{version}-$target.zip'
    return f"""{section}

## Downloads

| Runner host | Archive | SHA-256 |
|---|---|---|
{rows}
Source commit: `{sha}`

Each archive holds `marathon-{version}/bin/marathon`, `LICENSE` and `THIRD-PARTY-LICENSES`.

## Install

```sh
target={TARGETS[0]}  # or {TARGETS[1]}
curl -fLO {url}/{sample}
curl -fLO {url}/SHA256SUMS
sha256sum -c --ignore-missing SHA256SUMS  # on macOS: shasum -a 256 -c --ignore-missing SHA256SUMS
unzip {sample}
./marathon-{version}/bin/marathon version
```

`gh release download {version} --repo {repo} --pattern "{sample}" --pattern SHA256SUMS` also fetches them.

## Verify the build provenance

```sh
gh attestation verify {sample} --repo {repo}
```

## macOS: the binary is not notarised

The macOS binary is ad-hoc signed, not signed with an Apple Developer ID and not notarised. A browser marks
its downloads as quarantined, and extracting such a zip with `unzip` passes the mark to the binary. Gatekeeper then
blocks it: it does not start, and in an unattended session it hangs with no output. Downloading with `curl` or
`gh`, as above, sets no quarantine. For a browser download, clear the mark before the first run:

```sh
xattr -d com.apple.quarantine marathon-{version}/bin/marathon
```

A copy already launched while quarantined can stay blocked after that, so extract it again and clear the new copy.
"""


def collect(directory, version, sha, repo='<owner>/<repo>'):
    assets = []
    for target in TARGETS:
        records = list(directory.rglob(f'{target}.json'))
        if len(records) != 1:
            raise ValueError(f'expected one metadata file for {target}')
        record = json.loads(records[0].read_text())
        archive = records[0].parent / filename(version, target)
        if any(record.get(k) != v for k, v in dict(version=version, target=target, source_sha=sha,
                                                  archive=archive.name, sha256=digest(archive)).items()):
            raise ValueError('artifact metadata mismatch')
        # Recheck contents without executing the foreign architecture.
        with zipfile.ZipFile(archive) as z:
            try:
                check_archive(z, version)
            except ValueError as error:
                raise ValueError(f'invalid release archive manifest: {error}')
            if record['manifest'] != manifest_rows(version, {name: z.read(name) for name in z.namelist()}):
                raise ValueError('archive content digest mismatch')
        assets.append(archive)
    sums = directory / 'SHA256SUMS'
    sums.write_text(''.join(f'{digest(a)}  {a.name}\n' for a in assets))
    notes = directory / 'release-notes.md'
    notes.write_text(release_notes(version, sha, assets, repo), encoding='utf-8')
    return assets, sums, notes


def is_tag_push(env):
    return env.get('GITHUB_EVENT_NAME') == 'push' and env.get('GITHUB_REF_TYPE') == 'tag'


def release_exists(version, env):
    # A token that can only read does not see drafts, so `check-absent` and `publish` run with contents: write.
    pages = json.loads(run(['gh', 'api', 'repos/{owner}/{repo}/releases', '--paginate', '--slurp'], env=env))
    return any(release['tag_name'] == version for page in pages for release in page)


def commit_of(revision):
    # An annotated tag names a tag object; the metadata and the ancestor check are about its commit.
    return run(['git', 'rev-parse', '--verify', f'{revision}^{{commit}}'], cwd=ROOT).strip()


def check_asset_statuses():
    """An embedded asset whose terms still need a human decision blocks a release. A package build does not check it."""
    assets = json.loads(ASSET_LIST.read_text(encoding='utf-8'))['assets']
    unresolved = [f'{asset["component"]} ({asset.get("status", "no status")})' for asset in assets
                  if asset.get('status') != 'open-source']
    if unresolved:
        raise ValueError(f'unresolved embedded assets in {ASSET_LIST.name}: {", ".join(unresolved)}; '
                         'decide their terms and set their status before releasing')


def preflight(version, env):
    strict_version(version)
    changelog_section(version)
    check_asset_statuses()
    if not is_tag_push(env):
        return
    if env.get('GITHUB_REF_NAME') != version:
        raise ValueError(f'tag {env.get("GITHUB_REF_NAME")!r} is not the version {version!r}')
    commit = commit_of(env['GITHUB_SHA'])
    result = subprocess.run(['git', 'merge-base', '--is-ancestor', commit, 'refs/remotes/origin/main'],
                            cwd=ROOT, check=False)
    if result.returncode == 1:
        raise ValueError(f'tagged commit {commit} is not on origin/main')
    if result.returncode:
        raise ValueError(f'cannot compare {commit} with origin/main (git exit {result.returncode})')


def check_absent(version, env):
    if not is_tag_push(env):
        raise ValueError('the release check belongs to a tag push; a dry run publishes nothing')
    if release_exists(strict_version(version), env):
        raise ValueError('release or draft already exists for this tag; operator review required')


def prepare(directory, version, env):
    """Collect and notes without publishing; this path never calls gh."""
    return collect(directory, strict_version(version), commit_of('HEAD'), env.get('GITHUB_REPOSITORY', '<owner>/<repo>'))


def publish(directory, env):
    if not is_tag_push(env):
        raise ValueError('publication requires a tag push; branches and pull requests cannot publish')
    version = strict_version(env['GITHUB_REF_NAME'])
    assets, sums, notes = collect(directory, version, commit_of(env['GITHUB_SHA']), env['GITHUB_REPOSITORY'])
    if release_exists(version, env):
        raise ValueError('release or draft already exists for this tag; operator review required')
    prerelease = '-' in version
    # No --clobber or edit of an existing release.
    # Keep failed uploads in a draft so a partial upload never becomes latest.
    run(['gh', 'release', 'create', version, *assets, sums, '--verify-tag', '--draft',
         '--title', version, '--notes-file', notes, *(['--prerelease'] if prerelease else [])], env=env, cwd=ROOT)
    run(['gh', 'release', 'edit', version, '--draft=false',
         *(['--prerelease', '--latest=false'] if prerelease else ['--latest'])], env=env, cwd=ROOT)


def check_sums(directory, assets):
    expected = {}
    for line in (directory / 'SHA256SUMS').read_text().splitlines():
        hash_value, _, name = line.partition('  ')
        expected[name] = hash_value
    if set(expected) != set(assets):
        raise ValueError(f'SHA256SUMS lists {sorted(expected)}, expected {sorted(assets)}')
    for name, hash_value in expected.items():
        if digest(directory / name) != hash_value:
            raise ValueError(f'checksum mismatch: {name}')


def verify(version, target, directory, env):
    """Checks the published release from outside: what a user would download, not what was built."""
    strict_version(version)
    release = json.loads(run(['gh', 'release', 'view', version, '--json', 'tagName,isDraft,isPrerelease,assets'], env=env))
    archives = {filename(version, t) for t in TARGETS}
    published = {asset['name'] for asset in release['assets']}
    if release['tagName'] != version or release['isDraft'] or release['isPrerelease'] != ('-' in version):
        raise ValueError(f'unexpected release state: {release["tagName"]}, draft {release["isDraft"]}, '
                         f'prerelease {release["isPrerelease"]}')
    if published != archives | {'SHA256SUMS'}:
        raise ValueError(f'unexpected release assets: {sorted(published)}')
    run(['gh', 'release', 'download', version, '--dir', directory], env=env)
    check_sums(directory, archives)
    archive = directory / filename(version, target)
    with zipfile.ZipFile(archive) as z:
        check_archive(z, version)
    extracted = directory / 'extracted'
    run(['unzip', '-q', archive, '-d', extracted])
    out = run([extracted / f'marathon-{version}/bin/marathon', 'version'])
    lines = out.splitlines()
    if not lines or lines[-1] != version:
        raise ValueError(f'published binary reports {lines[-1:]}, expected {version}')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='command', required=True)
    for name in ('build', 'package'):
        p = sub.add_parser(name)
        p.add_argument('--target', choices=ALL_TARGETS, required=True)
        p.add_argument('--version', default=os.environ.get('GIT_TAG_NAME', '0.10.1'))
        p.add_argument('--output', type=Path, required=True)
        if name == 'package':
            p.add_argument('--binary', type=Path, required=True)
    p = sub.add_parser('publish')
    p.add_argument('--artifacts', type=Path, required=True)
    p = sub.add_parser('prepare')
    p.add_argument('--artifacts', type=Path, required=True)
    p.add_argument('--version', required=True)
    for name in ('preflight', 'check-absent'):
        sub.add_parser(name).add_argument('--version', required=True)
    p = sub.add_parser('prove-musl')
    p.add_argument('--archive', type=Path, required=True)
    p.add_argument('--version', required=True)
    p = sub.add_parser('verify')
    p.add_argument('--version', required=True)
    p.add_argument('--target', choices=TARGETS, required=True)
    p.add_argument('--dir', type=Path, required=True)
    args = parser.parse_args()
    env = dict(os.environ)
    if args.command == 'publish':
        publish(args.artifacts.resolve(), env)
        return
    if args.command == 'prepare':
        prepare(args.artifacts.resolve(), args.version, env)
        return
    if args.command == 'preflight':
        preflight(args.version, env)
        return
    if args.command == 'check-absent':
        check_absent(args.version, env)
        return
    if args.command == 'prove-musl':
        prove_musl(args.archive, version_value(args.version))
        return
    if args.command == 'verify':
        verify(args.version, args.target, args.dir.resolve(), env)
        return
    version = version_value(args.version)
    host = run(['rustc', '-vV']).split('host: ')[1].splitlines()[0]
    if args.target == MUSL:
        if host not in MUSL_HOSTS:
            raise ValueError(f'the musl target builds on a Linux x86_64 host only: {host}')
    elif host != args.target:
        raise ValueError(f'native host required: {host} != {args.target}')
    toolchain = zig_toolchain() if args.target == MUSL and args.command == 'build' else None
    if args.command == 'build':
        env = dict(os.environ, GIT_TAG_NAME=version)
        if args.target == DARWIN:
            env['MACOSX_DEPLOYMENT_TARGET'] = '11.0'
        cargo = ['cargo', 'zigbuild'] if args.target == MUSL else ['cargo', 'build']
        result = subprocess.run([*cargo, '--locked', '--release', '-p', 'marathon-cli',
                                 '--bin', 'marathon', '--target', args.target], cwd=ROOT, env=env, check=False)
        if result.returncode:
            raise ValueError(f'cargo build failed ({result.returncode})')
        target_dir = Path(env.get('CARGO_TARGET_DIR', ROOT / 'target'))
        if not target_dir.is_absolute():
            target_dir = ROOT / target_dir
        binary = target_dir / args.target / 'release/marathon'
    else:
        binary = args.binary.resolve()
    print(package(binary, args.output.resolve(), version, args.target, toolchain))


if __name__ == '__main__':
    try:
        main()
    except (OSError, ValueError, KeyError, subprocess.SubprocessError, zipfile.BadZipFile) as error:
        print(f'release: {error}', file=sys.stderr)
        # run() captures stderr, and a failed gh call explains itself only there.
        if getattr(error, 'stderr', None):
            print(error.stderr, file=sys.stderr)
        sys.exit(1)
