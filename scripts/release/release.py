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
# TARGETS are the archives every release must carry; MUSL is the static Linux build, collected
# when it was built (docs/wp/7.3b.md) so that existing two-target pipelines keep working.
TARGETS = ('aarch64-apple-darwin', 'x86_64-unknown-linux-gnu')
MUSL = 'x86_64-unknown-linux-musl'
ALL_TARGETS = TARGETS + (MUSL,)
# The musl target is native-arch: zig is only the C toolchain for `ring` and `aws-lc-sys`.
MUSL_HOSTS = ('x86_64-unknown-linux-gnu', MUSL)


def run(argv, **kwargs):
    return subprocess.run([str(a) for a in argv], check=True, text=True,
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE, **kwargs).stdout


def version_value(value):
    # A conservative release boundary; direct Cargo builds retain verbatim Kotlin semantics.
    if not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._+-]{0,127}', value) or '..' in value:
        raise ValueError('release version must be 1–128 safe ASCII characters, without traversal')
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


def smoke(archive, version):
    member = f'marathon-{version}/bin/marathon'
    with tempfile.TemporaryDirectory(prefix='marathon smoke with spaces ') as tmp:
        tmp = Path(tmp)
        with zipfile.ZipFile(archive) as z:
            if z.namelist() != [member]:
                raise ValueError('unexpected archive payload')
            info = z.getinfo(member)
            mode = info.external_attr >> 16
            if not stat.S_ISREG(mode) or stat.S_IMODE(mode) != 0o755:
                raise ValueError('archive executable mode must be 0755')
            binary = tmp / member
        run(['unzip', '-q', Path(archive).resolve(), '-d', tmp])
        if stat.S_IMODE(binary.stat().st_mode) != 0o755:
            raise ValueError('unzip did not preserve executable mode')
        cwd = tmp / 'unrelated cwd'
        cwd.mkdir()
        no_java = tmp / 'empty PATH'
        no_java.mkdir()
        env = {'PATH': str(no_java), 'HOME': str(cwd), 'TMPDIR': str(cwd),
               'GIT_TAG_NAME': 'runtime-conflict', 'JAVA_HOME': '/missing-java',
               'MARATHON_OPTS': 'ignored', 'JAVA_OPTS': 'ignored'}
        rows = [json.loads(line) for line in (ROOT / 'fixtures/oracle/cli-matrix.jsonl').read_text().splitlines()]
        for argv in (['version'], ['--help']):
            result = subprocess.run([str(binary), *argv], cwd=cwd, env=env, capture_output=True,
                                    text=True, timeout=30, check=False)
            expected = next((row for row in rows[1:] if row['argv'] == argv and row['program'] == 'marathon'), None)
            if expected is None:
                raise ValueError(f'CLI matrix has no marathon row for {argv}')
            expected = expected.get('rust') or expected
            out = re.sub(r'^\d{2}:\d{2}:\d{2}\.\d{3}', '<TIME>', result.stdout, flags=re.M)
            if argv == ['version']:
                out = out.replace(f'Starting marathon v{version},', 'Starting marathon v<VERSION>,')
                lines = out.splitlines(keepends=True)
                if not lines or lines[-1] != version + '\n':
                    raise ValueError('embedded version mismatch')
                lines[-1] = '<VERSION>\n'
                out = ''.join(lines)
                out = re.sub(r'(VersionCommandCliConfiguration@)[0-9a-f]+', r'\1<HASH>', out)
            if (result.returncode, out, result.stderr) != (expected['exit'], expected['stdout'], expected['stderr']):
                raise ValueError(f'extracted CLI expectation mismatch: {argv}: {result!r}')


def package(binary, output, version, target, toolchain=None):
    if not binary.is_file() or not os.access(binary, os.X_OK):
        raise ValueError('missing or nonexecutable binary')
    # -b: the description without the file name, which may itself contain ': '.
    description = run(['file', '-b', binary], env=inspect_env())
    required = ('Mach-O', 'arm64') if target == TARGETS[0] else ('ELF', 'x86-64')
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
    with tempfile.TemporaryDirectory(dir=output) as tmp:
        staged = Path(tmp) / archive.name
        with zipfile.ZipFile(staged, 'w', compression=zipfile.ZIP_DEFLATED) as z:
            info = zipfile.ZipInfo(f'marathon-{version}/bin/marathon')
            info.create_system = 3
            info.external_attr = (stat.S_IFREG | 0o755) << 16
            info.compress_type = zipfile.ZIP_DEFLATED
            z.writestr(info, binary.read_bytes())
        smoke(staged, version)
        if target == TARGETS[0]:
            deps, platform_info = run(['otool', '-L', binary]), run(['otool', '-l', binary])
        elif target == MUSL:
            deps, platform_info = musl_ldd, run(['readelf', '-lW', binary], env=inspect_env())
        else:
            deps, platform_info = run(['ldd', binary]), run(['readelf', '--version-info', binary])
        data = dict(version=version, target=target, archive=archive.name, sha256=digest(staged),
                    source_sha=run(['git', 'rev-parse', 'HEAD'], cwd=ROOT).strip(),
                    rustc=run(['rustc', '-Vv']), dependencies=deps, platform=platform_info,
                    manifest=[dict(path=f'marathon-{version}/bin/marathon', mode='0755', sha256=digest(binary))])
        if target == MUSL:
            data.update(toolchain=toolchain or {}, file=description.strip())
        staged.rename(archive)
        metadata.write_text(json.dumps(data, indent=2) + '\n')
    return archive


def collect(directory, version, sha):
    assets = []
    for target in ALL_TARGETS:
        records = list(directory.rglob(f'{target}.json'))
        if target == MUSL and not records:
            continue
        if len(records) != 1:
            raise ValueError(f'expected one metadata file for {target}')
        record = json.loads(records[0].read_text())
        archive = records[0].parent / filename(version, target)
        if any(record.get(k) != v for k, v in dict(version=version, target=target, source_sha=sha,
                                                  archive=archive.name, sha256=digest(archive)).items()):
            raise ValueError('artifact metadata mismatch')
        # Recheck contents without executing the foreign architecture.
        with zipfile.ZipFile(archive) as z:
            path = f'marathon-{version}/bin/marathon'
            if z.namelist() != [path] or z.getinfo(path).external_attr >> 16 != stat.S_IFREG | 0o755:
                raise ValueError('invalid release archive manifest')
            if record['manifest'] != [dict(path=path, mode='0755', sha256=hashlib.sha256(z.read(path)).hexdigest())]:
                raise ValueError('binary digest mismatch')
        assets.append(archive)
    sums = directory / 'SHA256SUMS'
    sums.write_text(''.join(f'{digest(a)}  {a.name}\n' for a in assets))
    notes = directory / 'release-notes.md'
    notes.write_text(f'Marathon {version}\n\nSource: {sha}\n\n' +
                     ''.join(f'- `{a.name}` — SHA-256 `{digest(a)}`\n' for a in assets) +
                     '\nSee docs/releasing.md for platform requirements and release gates.\n')
    return assets, sums, notes


def publish(directory, env):
    if env.get('GITHUB_EVENT_NAME') != 'push' or env.get('GITHUB_REF_TYPE') != 'tag':
        raise ValueError('publication requires a tag push; branches and pull requests cannot publish')
    version = version_value(env['GITHUB_REF_NAME'])
    assets, sums, notes = collect(directory, version, env['GITHUB_SHA'])
    pages = json.loads(run(['gh', 'api', 'repos/{owner}/{repo}/releases', '--paginate', '--slurp'], env=env))
    if any(release['tag_name'] == version for page in pages for release in page):
        raise ValueError('release or draft already exists for this tag; operator review required')
    # No --clobber or edit of an existing release.
    # Keep failed uploads in a draft so a partial upload never becomes latest.
    run(['gh', 'release', 'create', version, *assets, sums, '--verify-tag', '--draft',
         '--title', version, '--notes-file', notes], env=env, cwd=ROOT)
    run(['gh', 'release', 'edit', version, '--draft=false', '--latest'], env=env, cwd=ROOT)


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
    args = parser.parse_args()
    if args.command == 'publish':
        publish(args.artifacts.resolve(), dict(os.environ))
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
        if args.target == TARGETS[0]:
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
        sys.exit(1)
