#!/usr/bin/env python3
"""Offline release boundary checks; no network, credentials or Kotlin test ownership."""
import contextlib
import importlib.util
import io
import json
from pathlib import Path
import stat
import subprocess
import tempfile
import unittest
from unittest.mock import patch
import zipfile

SPEC = importlib.util.spec_from_file_location('release', Path(__file__).with_name('release.py'))
r = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(r)


class ReleaseTests(unittest.TestCase):
    def fixture(self, root, version='v1'):
        for target in r.TARGETS:
            directory = root / target
            directory.mkdir()
            archive = directory / r.filename(version, target)
            info = zipfile.ZipInfo(f'marathon-{version}/bin/marathon')
            info.create_system = 3
            info.external_attr = (stat.S_IFREG | 0o755) << 16
            payload = target.encode()
            with zipfile.ZipFile(archive, 'w') as z:
                z.writestr(info, payload)
            (directory / f'{target}.json').write_text(json.dumps(dict(
                version=version, target=target, archive=archive.name, source_sha='abc', sha256=r.digest(archive),
                manifest=[dict(path=info.filename, mode='0755', sha256=r.hashlib.sha256(payload).hexdigest())])))

    def test_version_boundary(self):
        for value in ('', '.', '..', '../v1', 'a/b', 'a\\b', 'a\n', 'x y', '-v1', 'é'):
            with self.subTest(value=value), self.assertRaises(ValueError):
                r.version_value(value)
        for value in ('0.10.1', 'v1.2.3-rc.1+build', 'release_1'):
            self.assertEqual(r.version_value(value), value)

    def test_collection_and_corruption(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.fixture(root)
            assets, sums, notes = r.collect(root, 'v1', 'abc')
            self.assertEqual(len(assets), 2)
            self.assertEqual(sums.read_text(), ''.join(f'{r.digest(a)}  {a.name}\n' for a in assets))
            self.assertTrue(all(a.name in notes.read_text() for a in assets))
            with self.assertRaises(ValueError):
                r.collect(root, 'v1', 'different-sha')
            assets[0].write_bytes(b'corrupted')
            with self.assertRaises(ValueError):
                r.collect(root, 'v1', 'abc')

    def test_missing_target(self):
        with tempfile.TemporaryDirectory() as tmp, self.assertRaises(ValueError):
            r.collect(Path(tmp), 'v1', 'abc')

    def test_no_branch_or_pull_request_publish(self):
        for event, ref in [('pull_request', 'branch'), ('push', 'branch'), ('workflow_dispatch', 'tag')]:
            with self.subTest(event=event), patch.object(r, 'run') as run, self.assertRaises(ValueError):
                r.publish(Path('/unused'), dict(GITHUB_EVENT_NAME=event, GITHUB_REF_TYPE=ref))
            run.assert_not_called()

    def test_publication_order_payloads_and_failure(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.fixture(root)
            env = dict(GITHUB_EVENT_NAME='push', GITHUB_REF_TYPE='tag', GITHUB_REF_NAME='v1', GITHUB_SHA='abc')
            calls = []
            def record(argv, **kwargs):
                calls.append([str(x) for x in argv])
                self.assertEqual(kwargs['env'], env)
                if argv[2] == 'create':
                    for asset in argv[4:7]:
                        self.assertTrue(Path(asset).is_file())
                return '[[]]' if argv[1] == 'api' else ''
            with patch.object(r, 'run', side_effect=record):
                r.publish(root, env)
            self.assertEqual(calls[1][:4], ['gh', 'release', 'create', 'v1'])
            self.assertIn('--draft', calls[1])
            self.assertIn('--verify-tag', calls[1])
            self.assertNotIn('--clobber', calls[1])
            self.assertEqual(calls[2], ['gh', 'release', 'edit', 'v1', '--draft=false', '--latest'])
            # Includes an existing-release rejection, authentication failure, or failed upload.
            with patch.object(r, 'run', side_effect=subprocess.CalledProcessError(1, 'gh')) as run:
                with self.assertRaises(subprocess.CalledProcessError):
                    r.publish(root, env)
                self.assertEqual(run.call_count, 1)

    def test_existing_draft_or_release_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.fixture(root)
            env = dict(GITHUB_EVENT_NAME='push', GITHUB_REF_TYPE='tag', GITHUB_REF_NAME='v1', GITHUB_SHA='abc')
            for draft in (True, False):
                with patch.object(r, 'run', return_value=json.dumps([[dict(tag_name='v1', draft=draft)]])) as run:
                    with self.assertRaises(ValueError):
                        r.publish(root, env)
                    self.assertEqual(run.call_count, 1)

    def test_failed_build_stops_packaging(self):
        with tempfile.TemporaryDirectory() as tmp:
            args = ['release.py', 'build', '--target', r.TARGETS[0], '--output', tmp + '/out']
            with patch.object(r.sys, 'argv', args), patch.object(r, 'run', return_value='host: ' + r.TARGETS[0]), patch.object(r.subprocess, 'run', return_value=subprocess.CompletedProcess([], 19)), patch.object(r, 'package') as package:
                with self.assertRaisesRegex(ValueError, 'cargo build failed'):
                    r.main()
                package.assert_not_called()

    def test_package_failure_leaves_no_archive(self):
        with tempfile.TemporaryDirectory(prefix='release paths with spaces ') as tmp:
            root = Path(tmp)
            with self.assertRaises(ValueError):
                r.package(root / 'missing', root / 'out', 'v1', r.TARGETS[0])
            binary = root / 'marathon'
            binary.write_bytes(b'placeholder')
            binary.chmod(0o755)
            with patch.object(r, 'run', return_value='Mach-O arm64'), patch.object(r, 'smoke', side_effect=ValueError('bad CLI')):
                with self.assertRaises(ValueError):
                    r.package(binary, root / 'out', 'v1', r.TARGETS[0])
            self.assertEqual(list((root / 'out').iterdir()), [])
            with patch.object(r, 'run', return_value='Mach-O arm64'), patch.object(r.zipfile, 'ZipFile', side_effect=OSError('full disk')):
                with self.assertRaises(OSError):
                    r.package(binary, root / 'out', 'v1', r.TARGETS[0])
            self.assertEqual(list((root / 'out').iterdir()), [])

    STATIC_PIE = ('ELF 64-bit LSB pie executable, x86-64, version 1 (SYSV), static-pie linked, stripped')
    DYNAMIC = ('ELF 64-bit LSB pie executable, x86-64, version 1 (SYSV), dynamically linked, '
               'interpreter /lib64/ld-linux-x86-64.so.2, for GNU/Linux 3.2.0')

    def test_static_check(self):
        r.require_static(self.STATIC_PIE, '\tstatically linked\n')
        r.require_static(self.STATIC_PIE, 'not a dynamic executable\n')
        r.require_static('ELF 64-bit LSB executable, x86-64, statically linked', 'not a dynamic executable')
        for description, ldd in [(self.DYNAMIC, 'libc.so.6 => /lib/libc.so.6'),
                                 (self.STATIC_PIE, '\tlibc.so.6 => /lib/libc.so.6\n'),
                                 ('ELF 64-bit, dynamically linked', 'not a dynamic executable'),
                                 (self.STATIC_PIE + ', interpreter /lib/ld-musl-x86_64.so.1', 'not a dynamic executable')]:
            with self.subTest(description=description), self.assertRaisesRegex(ValueError, 'static|loader|dynamic'):
                r.require_static(description, ldd)

    def musl_package(self, root, description, ldd, toolchain=dict(zig='0.0', cargo_zigbuild='cargo-zigbuild 0.0')):
        binary = root / 'marathon'
        binary.write_bytes(b'placeholder')
        binary.chmod(0o755)
        def fake_run(argv, **kwargs):
            if argv[0] in ('file', 'readelf'):
                self.assertEqual(kwargs.get('env', {}).get('LC_ALL'), 'C')
            if argv[0] == 'file':
                self.assertEqual(argv[1], '-b')
            return {'file': description, 'readelf': 'program headers', 'git': 'abc\n', 'rustc': 'rustc 1.99.0'}[argv[0]]
        with patch.object(r, 'run', side_effect=fake_run), patch.object(r, 'ldd_output', return_value=ldd), \
                patch.object(r, 'smoke'):
            return r.package(binary, root / 'out', 'v1', r.MUSL, toolchain)

    def test_musl_package_records_static_evidence(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            archive = self.musl_package(root, self.STATIC_PIE, '\tstatically linked\n')
            self.assertEqual(archive.name, 'marathon-v1-x86_64-unknown-linux-musl.zip')
            record = json.loads((root / 'out' / f'{r.MUSL}.json').read_text())
            self.assertEqual(record['target'], r.MUSL)
            self.assertEqual(record['toolchain'], dict(zig='0.0', cargo_zigbuild='cargo-zigbuild 0.0'))
            self.assertIn('static-pie', record['file'])
            self.assertNotIn(str(root), record['file'])
            self.assertEqual(record['manifest'][0]['mode'], '0755')
            with zipfile.ZipFile(archive) as z:
                self.assertEqual(z.namelist(), ['marathon-v1/bin/marathon'])
                self.assertEqual(z.getinfo('marathon-v1/bin/marathon').external_attr >> 16, stat.S_IFREG | 0o755)

    def test_musl_package_without_build_tool_versions_is_refused(self):
        # M-sol-2: a standalone `package` has no zig/cargo-zigbuild provenance to record.
        for toolchain in (None, {}):
            with tempfile.TemporaryDirectory() as tmp, self.subTest(toolchain=toolchain):
                root = Path(tmp)
                with self.assertRaisesRegex(ValueError, 'build-tool versions'):
                    self.musl_package(root, self.STATIC_PIE, '\tstatically linked\n', toolchain)
                self.assertFalse((root / 'out').exists())

    def test_inspection_runs_in_the_c_locale(self):
        # M-sol-1: ldd's "not a dynamic executable" is matched in English.
        with patch.object(r.subprocess, 'run', return_value=subprocess.CompletedProcess([], 1, 'x')) as run:
            r.ldd_output(Path('/bin/x'))
        self.assertEqual(run.call_args.kwargs['env']['LC_ALL'], 'C')

    def test_recorded_file_description_keeps_paths_with_separators_out(self):
        # M-sol-3: `file -b` output is recorded whole; the old split leaked a path containing ': '.
        with tempfile.TemporaryDirectory(prefix='private: build-user ') as tmp:
            root = Path(tmp)
            self.musl_package(root, self.STATIC_PIE, '\tstatically linked\n')
            record = json.loads((root / 'out' / f'{r.MUSL}.json').read_text())
            self.assertEqual(record['file'], self.STATIC_PIE)
            self.assertNotIn('build-user', record['file'])

    def test_musl_package_rejects_dynamic_binary_and_leaves_nothing(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            with self.assertRaisesRegex(ValueError, 'statically'):
                self.musl_package(root, self.DYNAMIC, 'libc.so.6 => /lib/libc.so.6')
            self.assertFalse((root / 'out').exists() and list((root / 'out').iterdir()))

    def musl_main(self, host, zig_ok=True):
        def fake_run(argv, **kwargs):
            if argv[0] == 'rustc':
                return f'host: {host}\n'
            if not zig_ok:
                raise subprocess.CalledProcessError(1, argv)
            return '0.0\n'
        with tempfile.TemporaryDirectory() as tmp:
            args = ['release.py', 'build', '--target', r.MUSL, '--version', 'v1', '--output', tmp + '/out']
            with patch.object(r.sys, 'argv', args), patch.object(r, 'run', side_effect=fake_run), \
                    patch.object(r.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0)) as sub, \
                    patch.object(r, 'package', return_value=Path('x')) as package:
                try:
                    with contextlib.redirect_stdout(io.StringIO()):
                        r.main()
                except ValueError:
                    return sub, package, True
                return sub, package, False

    def test_musl_builds_with_zigbuild_on_linux_x86_64_only(self):
        for host in ('x86_64-unknown-linux-gnu', r.MUSL):
            sub, package, failed = self.musl_main(host)
            self.assertFalse(failed)
            argv = sub.call_args.args[0]
            self.assertEqual(argv[:3], ['cargo', 'zigbuild', '--locked'])
            self.assertEqual(argv[argv.index('--target') + 1], r.MUSL)
            self.assertEqual(sub.call_args.kwargs['env']['GIT_TAG_NAME'], 'v1')
            self.assertEqual(package.call_args.args[3:], (r.MUSL, dict(zig='0.0', cargo_zigbuild='0.0')))
        for host in ('aarch64-apple-darwin', 'aarch64-unknown-linux-gnu'):
            sub, package, failed = self.musl_main(host)
            self.assertTrue(failed)
            sub.assert_not_called()

    def test_musl_without_zig_fails_before_building(self):
        sub, package, failed = self.musl_main('x86_64-unknown-linux-gnu', zig_ok=False)
        self.assertTrue(failed)
        sub.assert_not_called()

    def test_gnu_build_still_uses_plain_cargo(self):
        with tempfile.TemporaryDirectory() as tmp:
            args = ['release.py', 'build', '--target', 'x86_64-unknown-linux-gnu', '--output', tmp + '/out']
            with patch.object(r.sys, 'argv', args), patch.object(r, 'run', return_value='host: x86_64-unknown-linux-gnu'), \
                    patch.object(r.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0)) as sub, \
                    patch.object(r, 'package', return_value=Path('x')), contextlib.redirect_stdout(io.StringIO()):
                r.main()
            self.assertEqual(sub.call_args.args[0][:2], ['cargo', 'build'])

    def test_collect_includes_musl_only_when_built(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.fixture(root)
            assets, sums, notes = r.collect(root, 'v1', 'abc')
            self.assertEqual([a.name for a in assets], [r.filename('v1', t) for t in r.TARGETS])
            directory = root / r.MUSL
            directory.mkdir()
            archive = directory / r.filename('v1', r.MUSL)
            info = zipfile.ZipInfo('marathon-v1/bin/marathon')
            info.create_system = 3
            info.external_attr = (stat.S_IFREG | 0o755) << 16
            with zipfile.ZipFile(archive, 'w') as z:
                z.writestr(info, b'musl')
            (directory / f'{r.MUSL}.json').write_text(json.dumps(dict(
                version='v1', target=r.MUSL, archive=archive.name, source_sha='abc', sha256=r.digest(archive),
                manifest=[dict(path=info.filename, mode='0755', sha256=r.hashlib.sha256(b'musl').hexdigest())])))
            assets, sums, notes = r.collect(root, 'v1', 'abc')
            self.assertEqual([a.name for a in assets], [r.filename('v1', t) for t in r.ALL_TARGETS])
            self.assertIn(archive.name, sums.read_text())
            archive.write_bytes(b'corrupted')
            with self.assertRaises(ValueError):
                r.collect(root, 'v1', 'abc')


if __name__ == '__main__':
    unittest.main()
