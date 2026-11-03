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

def load(name):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(f'{name}.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


r = load('release')
tpl = load('third_party_licenses')
VERSION = '1.2.3'
SHA = 'abc'
REPO = 'owner/name'
REAL_CHANGELOG = r.CHANGELOG
TAG_ENV = dict(GITHUB_EVENT_NAME='push', GITHUB_REF_TYPE='tag', GITHUB_REF_NAME=VERSION, GITHUB_SHA=SHA,
               GITHUB_REPOSITORY=REPO)


def zipped(archive, entries):
    with zipfile.ZipFile(archive, 'w') as z:
        for name, (mode, payload) in entries.items():
            info = zipfile.ZipInfo(name)
            info.create_system = 3
            info.external_attr = (stat.S_IFREG | mode) << 16
            z.writestr(info, payload)


def entries_for(version, label):
    return {name: (mode, f'{label} {name}'.encode()) for name, mode in r.archive_modes(version).items()}


def fake_git(argv, **kwargs):
    """Answers `git rev-parse` as the commit itself; everything else is the caller's business."""
    return SHA + '\n'


class ReleaseTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        changelog = Path(directory.name) / 'CHANGELOG.md'
        changelog.write_text('# Changelog\n\n## 2.0.0\n\nOther text.\n\n'
                             '## 1.2.3\n\nSection text, see [the guide](docs/guide.md#a) and [web](https://example.org/x).\n\n'
                             '### Known limits\n\nA limit.\n\n## 1.2.3-rc.1\n\nCandidate text.\n')
        patcher = patch.object(r, 'CHANGELOG', changelog)
        patcher.start()
        self.addCleanup(patcher.stop)
        self.changelog = changelog

    def archive(self, root, target, version=VERSION, sha=SHA):
        directory = root / target
        directory.mkdir()
        archive = directory / r.filename(version, target)
        entries = entries_for(version, target)
        zipped(archive, entries)
        (directory / f'{target}.json').write_text(json.dumps(dict(
            version=version, target=target, archive=archive.name, source_sha=sha, sha256=r.digest(archive),
            manifest=r.manifest_rows(version, {name: payload for name, (_, payload) in entries.items()}))))
        return archive

    def fixture(self, root, version=VERSION):
        for target in r.TARGETS:
            self.archive(root, target, version)

    def test_strict_version(self):
        for value in ('0.99.0', '1.2.3', '10.20.30', '0.99.0-rc.1', '1.0.0-alpha', '1.0.0-alpha-1', '1.0.0-0.3.7', '1.0.0-x.7.z.92'):
            with self.subTest(value=value):
                self.assertEqual(r.strict_version(value), value)
        for value in ('', 'v1.2.3', '1.2', '1', '1.2.3.4', '01.2.3', '1.02.3', '1.2.03', '1.2.3-', '1.2.3-a..b', '1.2.3-01',
                      '1.2.3+build', '1.2.3-rc+build', '1.2.3\n', ' 1.2.3', '1.2.3 ', '1.2.3-é', 'latest', '../1.2.3', '-1.2.3'):
            with self.subTest(value=value), self.assertRaises(ValueError):
                r.strict_version(value)

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
            assets, sums, notes = r.collect(root, VERSION, SHA)
            self.assertEqual([a.name for a in assets], [r.filename(VERSION, t) for t in (r.DARWIN, r.MUSL)])
            self.assertEqual(sums.read_text(), ''.join(f'{r.digest(a)}  {a.name}\n' for a in assets))
            self.assertTrue(all(a.name in notes.read_text() for a in assets))
            with self.assertRaises(ValueError):
                r.collect(root, VERSION, 'different-sha')
            assets[0].write_bytes(b'corrupted')
            with self.assertRaises(ValueError):
                r.collect(root, VERSION, SHA)

    def test_collect_requires_both_release_targets(self):
        for present in ((), (r.DARWIN,), (r.MUSL,), (r.DARWIN, r.GNU), (r.MUSL, r.GNU)):
            with tempfile.TemporaryDirectory() as tmp, self.subTest(present=present):
                root = Path(tmp)
                for target in present:
                    self.archive(root, target)
                with self.assertRaisesRegex(ValueError, 'expected one metadata file'):
                    r.collect(root, VERSION, SHA)

    def test_collect_does_not_attach_a_gnu_archive(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.fixture(root)
            gnu = self.archive(root, r.GNU)
            assets, sums, notes = r.collect(root, VERSION, SHA)
            self.assertNotIn(gnu, assets)
            self.assertEqual(len(assets), 2)
            self.assertNotIn(gnu.name, sums.read_text())
            self.assertNotIn(gnu.name, notes.read_text())

    def test_collect_rejects_a_second_metadata_file(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.fixture(root)
            (root / 'copy').mkdir()
            (root / 'copy' / f'{r.MUSL}.json').write_text((root / r.MUSL / f'{r.MUSL}.json').read_text())
            with self.assertRaisesRegex(ValueError, 'expected one metadata file'):
                r.collect(root, VERSION, SHA)

    def rewritten_archive(self, root, entries):
        """An archive that matches its metadata hash but not the contract, so only the content checks can catch it."""
        archive = root / r.MUSL / r.filename(VERSION, r.MUSL)
        zipped(archive, entries)
        record_path = root / r.MUSL / f'{r.MUSL}.json'
        record = json.loads(record_path.read_text())
        record['sha256'] = r.digest(archive)
        record_path.write_text(json.dumps(record))

    def test_collect_rechecks_file_list_modes_and_contents(self):
        good = entries_for(VERSION, r.MUSL)
        root_name = f'marathon-{VERSION}'
        cases = {
            'missing third-party file': {k: v for k, v in good.items() if not k.endswith('THIRD-PARTY-LICENSES')},
            'missing licence': {k: v for k, v in good.items() if not k.endswith('/LICENSE')},
            'binary only': {k: v for k, v in good.items() if k.endswith('bin/marathon')},
            'extra file': {**good, f'{root_name}/extra': (0o644, b'x')},
            'executable licence': {**good, f'{root_name}/LICENSE': (0o755, good[f'{root_name}/LICENSE'][1])},
            'group-writable notices': {**good, f'{root_name}/THIRD-PARTY-LICENSES': (0o664, b'x')},
            'binary without execute': {**good, f'{root_name}/bin/marathon': (0o644, good[f'{root_name}/bin/marathon'][1])},
            'different licence text': {**good, f'{root_name}/LICENSE': (0o644, b'tampered')},
        }
        for name, entries in cases.items():
            with tempfile.TemporaryDirectory() as tmp, self.subTest(name):
                root = Path(tmp)
                self.fixture(root)
                self.rewritten_archive(root, entries)
                with self.assertRaisesRegex(ValueError, 'manifest|digest'):
                    r.collect(root, VERSION, SHA)

    def test_changelog_section_is_only_the_matching_one(self):
        section = r.changelog_section('1.2.3')
        self.assertIn('Section text', section)
        self.assertIn('### Known limits', section)
        self.assertIn('A limit.', section)
        self.assertNotIn('Other text', section)
        self.assertNotIn('Candidate text', section)
        self.assertEqual(r.changelog_section('1.2.3-rc.1'), 'Candidate text.')

    def test_changelog_missing_empty_or_duplicate_section_is_refused(self):
        for version in ('9.9.9', '1.2', '1.2.3-rc'):
            with self.subTest(version=version), self.assertRaisesRegex(ValueError, 'exactly one section'):
                r.changelog_section(version)
        self.changelog.write_text('## 1.0.0\n\n## 1.1.0\n\ntext\n\n## 1.1.0\n\nagain\n')
        with self.assertRaisesRegex(ValueError, 'is empty'):
            r.changelog_section('1.0.0')
        with self.assertRaisesRegex(ValueError, 'exactly one section'):
            r.changelog_section('1.1.0')

    def test_repository_changelog_has_the_first_release(self):
        with patch.object(r, 'CHANGELOG', REAL_CHANGELOG):
            self.assertIn('docs/deviations.md', r.changelog_section('0.99.0'))

    def test_notes_content(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.fixture(root)
            assets, _, notes = r.collect(root, VERSION, 'abc', REPO)
            text = notes.read_text()
        self.assertIn('Section text', text)
        self.assertNotIn('Other text', text)
        self.assertNotIn('Candidate text', text)
        self.assertIn(f'(https://github.com/{REPO}/blob/{VERSION}/docs/guide.md#a)', text)
        self.assertIn('(https://example.org/x)', text)
        for target, asset in zip(r.TARGETS, assets):
            self.assertIn(target, text)
        self.assertIn('Source commit: `abc`', text)
        self.assertIn(f'gh attestation verify marathon-{VERSION}-$target.zip --repo {REPO}', text)
        self.assertIn('--ignore-missing SHA256SUMS', text)
        self.assertIn(f'xattr -d com.apple.quarantine marathon-{VERSION}/bin/marathon', text)
        self.assertIn('not notarised', text)
        self.assertIn('before the first run', text)

    def test_notes_table_has_each_hash(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.fixture(root)
            assets, _, notes = r.collect(root, VERSION, SHA, REPO)
            for asset in assets:
                self.assertIn(f'| `{asset.name}` | `{r.digest(asset)}` |', notes.read_text())

    def test_publish_without_changelog_section_stops_before_gh(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.fixture(root, '3.0.0')
            with patch.object(r, 'run', side_effect=fake_git) as run, self.assertRaisesRegex(ValueError, 'exactly one section'):
                r.publish(root, dict(TAG_ENV, GITHUB_REF_NAME='3.0.0'))
            self.assertEqual([call.args[0][0] for call in run.call_args_list], ['git'])

    def test_missing_target(self):
        with tempfile.TemporaryDirectory() as tmp, self.assertRaises(ValueError):
            r.collect(Path(tmp), VERSION, SHA)

    def test_no_branch_or_pull_request_publish(self):
        for event, ref in [('pull_request', 'branch'), ('push', 'branch'), ('workflow_dispatch', 'tag'), ('workflow_dispatch', 'branch')]:
            with self.subTest(event=event, ref=ref), patch.object(r, 'run') as run, self.assertRaises(ValueError):
                r.publish(Path('/unused'), dict(TAG_ENV, GITHUB_EVENT_NAME=event, GITHUB_REF_TYPE=ref))
            run.assert_not_called()

    def test_publish_refuses_a_version_outside_the_release_format(self):
        for value in ('v1.2.3', 'release_1', '1.2'):
            with self.subTest(value=value), patch.object(r, 'run') as run, self.assertRaises(ValueError):
                r.publish(Path('/unused'), dict(TAG_ENV, GITHUB_REF_NAME=value))
            run.assert_not_called()

    def publish_calls(self, version):
        """The exact command lines a tag push runs, with gh recorded and nothing sent."""
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.changelog.write_text(f'## {version}\n\nText.\n')
            self.fixture(root, version)
            env = dict(TAG_ENV, GITHUB_REF_NAME=version)
            calls = []
            def record(argv, **kwargs):
                argv = [str(x).replace(str(root), 'dist') for x in argv]
                if argv[0] == 'git':
                    self.assertEqual(argv, ['git', 'rev-parse', '--verify', f'{SHA}^{{commit}}'])
                    return SHA + '\n'
                calls.append(argv)
                self.assertEqual(kwargs['env'], env)
                if argv[1] == 'release' and argv[2] == 'create':
                    for asset in (r.filename(version, t) for t in r.TARGETS):
                        self.assertTrue(any(a.endswith(asset) for a in argv))
                    self.assertTrue((root / 'release-notes.md').is_file())
                return '[[]]' if argv[1] == 'api' else ''
            with patch.object(r, 'run', side_effect=record):
                r.publish(root, env)
            return calls

    def test_publication_of_a_plain_version_is_marked_latest(self):
        calls = self.publish_calls(VERSION)
        self.assertEqual(calls, [
            ['gh', 'api', 'repos/{owner}/{repo}/releases', '--paginate', '--slurp'],
            ['gh', 'release', 'create', VERSION,
             f'dist/{r.DARWIN}/marathon-{VERSION}-{r.DARWIN}.zip', f'dist/{r.MUSL}/marathon-{VERSION}-{r.MUSL}.zip',
             'dist/SHA256SUMS', '--verify-tag', '--draft', '--title', VERSION, '--notes-file', 'dist/release-notes.md'],
            ['gh', 'release', 'edit', VERSION, '--draft=false', '--latest'],
        ])

    def test_publication_of_a_prerelease_is_not_marked_latest(self):
        version = '1.2.3-rc.1'
        calls = self.publish_calls(version)
        self.assertEqual(calls[1], [
            'gh', 'release', 'create', version,
            f'dist/{r.DARWIN}/marathon-{version}-{r.DARWIN}.zip', f'dist/{r.MUSL}/marathon-{version}-{r.MUSL}.zip',
            'dist/SHA256SUMS', '--verify-tag', '--draft', '--title', version, '--notes-file', 'dist/release-notes.md',
            '--prerelease'])
        self.assertEqual(calls[2], ['gh', 'release', 'edit', version, '--draft=false', '--prerelease', '--latest=false'])

    def test_publication_never_replaces_assets(self):
        for call in self.publish_calls(VERSION):
            self.assertNotIn('--clobber', call)
            self.assertNotEqual(call[1:3], ['release', 'upload'])
            self.assertNotEqual(call[1:3], ['release', 'delete'])

    def test_failed_upload_stops_before_going_public(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.fixture(root)
            calls = []
            def fail_on_create(argv, **kwargs):
                calls.append([str(x) for x in argv])
                if argv[0] == 'git':
                    return SHA + '\n'
                if argv[1] == 'api':
                    return '[[]]'
                raise subprocess.CalledProcessError(1, 'gh')
            with patch.object(r, 'run', side_effect=fail_on_create), self.assertRaises(subprocess.CalledProcessError):
                r.publish(root, TAG_ENV)
            self.assertFalse(any(call[1:3] == ['release', 'edit'] for call in calls))

    def test_existing_draft_or_release_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.fixture(root)
            for draft in (True, False):
                listing = json.dumps([[dict(tag_name=VERSION, draft=draft)]])
                def answer(argv, **kwargs):
                    return SHA + '\n' if argv[0] == 'git' else listing
                with patch.object(r, 'run', side_effect=answer) as run, self.assertRaisesRegex(ValueError, 'already exists'):
                    r.publish(root, TAG_ENV)
                self.assertFalse(any(call.args[0][:2] == ['gh', 'release'] for call in run.call_args_list))
                with patch.object(r, 'run', side_effect=answer), self.assertRaisesRegex(ValueError, 'already exists'):
                    r.check_absent(VERSION, TAG_ENV)

    def test_check_absent_passes_when_only_other_tags_exist(self):
        listing = json.dumps([[dict(tag_name='1.2.2', draft=False)], [dict(tag_name='1.2.3-rc.1', draft=True)]])
        with patch.object(r, 'run', return_value=listing):
            r.check_absent(VERSION, TAG_ENV)

    def test_check_absent_is_for_a_tag_push_only(self):
        with patch.object(r, 'run') as run, self.assertRaisesRegex(ValueError, 'tag push'):
            r.check_absent(VERSION, dict(TAG_ENV, GITHUB_EVENT_NAME='workflow_dispatch'))
        run.assert_not_called()

    def test_prepare_is_a_dry_run_that_never_calls_gh(self):
        for env in (dict(GITHUB_EVENT_NAME='workflow_dispatch', GITHUB_REF_TYPE='branch', GITHUB_REPOSITORY=REPO),
                    dict(GITHUB_EVENT_NAME='push', GITHUB_REF_TYPE='branch', GITHUB_REPOSITORY=REPO), {}):
            with tempfile.TemporaryDirectory() as tmp, self.subTest(env=env):
                root = Path(tmp)
                self.fixture(root)
                with patch.object(r, 'run', side_effect=fake_git) as run:
                    assets, sums, notes = r.prepare(root, VERSION, env)
                self.assertEqual([call.args[0][0] for call in run.call_args_list], ['git'])
                self.assertEqual(len(assets), 2)
                self.assertTrue(sums.is_file() and notes.is_file())

    def test_prepare_refuses_a_version_outside_the_release_format(self):
        with patch.object(r, 'run') as run, self.assertRaises(ValueError):
            r.prepare(Path('/unused'), 'v1.2.3', {})
        run.assert_not_called()

    def preflight(self, env, version=VERSION, ancestor_rc=0):
        with patch.object(r, 'run', side_effect=fake_git) as run, \
                patch.object(r.subprocess, 'run', return_value=subprocess.CompletedProcess([], ancestor_rc)) as sub:
            try:
                r.preflight(version, env)
            finally:
                self.preflight_git = [call.args[0] for call in sub.call_args_list]
                self.preflight_run = [call.args[0] for call in run.call_args_list]

    def test_preflight_of_a_tag_on_main(self):
        self.preflight(TAG_ENV)
        self.assertEqual(self.preflight_git, [['git', 'merge-base', '--is-ancestor', SHA, 'refs/remotes/origin/main']])

    def test_preflight_refuses_a_tag_off_main_and_unknown_ancestry(self):
        with self.assertRaisesRegex(ValueError, 'not on origin/main'):
            self.preflight(TAG_ENV, ancestor_rc=1)
        with self.assertRaisesRegex(ValueError, 'cannot compare'):
            self.preflight(TAG_ENV, ancestor_rc=128)

    def test_preflight_refuses_a_version_that_is_not_the_tag(self):
        with self.assertRaisesRegex(ValueError, 'is not the version'):
            self.preflight(TAG_ENV, version='1.2.3-rc.1')

    def test_preflight_refuses_a_bad_version_or_missing_section(self):
        for version, message in (('v1.2.3', 'without a v'), ('1.2', 'without a v'), ('9.9.9', 'exactly one section')):
            for env in (TAG_ENV, dict(GITHUB_EVENT_NAME='workflow_dispatch', GITHUB_REF_TYPE='branch')):
                with self.subTest(version=version, env=env.get('GITHUB_EVENT_NAME')), self.assertRaisesRegex(ValueError, message):
                    self.preflight(env, version=version)

    def test_preflight_of_a_dry_run_needs_neither_a_tag_nor_main(self):
        for ref_type in ('branch', 'tag'):
            self.preflight(dict(GITHUB_EVENT_NAME='workflow_dispatch', GITHUB_REF_TYPE=ref_type, GITHUB_REF_NAME='anything', GITHUB_SHA=SHA))
            self.assertEqual(self.preflight_git, [])
            self.assertEqual(self.preflight_run, [])

    def downloaded_release(self, root, tampered=False, reported=VERSION, state=None, extra_asset=False):
        """A stub gh: `view` answers with state, `download` writes the assets, the binary reports a version."""
        calls = []
        names = [r.filename(VERSION, t) for t in r.TARGETS] + ['SHA256SUMS'] + (['extra'] if extra_asset else [])
        state = dict(dict(tagName=VERSION, isDraft=False, isPrerelease=False), **(state or {}), assets=[dict(name=n) for n in names])
        def stub(argv, **kwargs):
            argv = [str(a) for a in argv]
            calls.append(argv)
            if argv[:3] == ['gh', 'release', 'view']:
                return json.dumps(state)
            if argv[:3] == ['gh', 'release', 'download']:
                directory = Path(argv[argv.index('--dir') + 1])
                directory.mkdir()
                lines = []
                for target in r.TARGETS:
                    archive = directory / r.filename(VERSION, target)
                    zipped(archive, entries_for(VERSION, target))
                    lines.append(f'{r.digest(archive)}  {archive.name}\n')
                (directory / 'SHA256SUMS').write_text(''.join(lines))
                if tampered:
                    (directory / r.filename(VERSION, r.DARWIN)).write_bytes(b'tampered')
                return ''
            if argv[0] == 'unzip':
                return ''
            if argv[-1] == 'version':
                self.assertIn(f'extracted/marathon-{VERSION}/bin/marathon', argv[0])
                return f'12:00:00.000 [main] INFO Starting marathon\n{reported}\n'
            raise AssertionError(argv)
        with patch.object(r, 'run', side_effect=stub):
            r.verify(VERSION, r.DARWIN, root / 'release', {})
        return calls

    def test_verify_checks_the_published_release(self):
        with tempfile.TemporaryDirectory() as tmp:
            calls = self.downloaded_release(Path(tmp))
            self.assertEqual(calls[0][:4], ['gh', 'release', 'view', VERSION])
            self.assertEqual(calls[1][:4], ['gh', 'release', 'download', VERSION])
            self.assertEqual(calls[-1][-1], 'version')

    def test_verify_fails_loudly(self):
        cases = {'checksum mismatch': dict(tampered=True), 'published binary reports': dict(reported='0.10.1'),
                 'unexpected release state': dict(state=dict(isDraft=True)),
                 'unexpected release assets': dict(extra_asset=True)}
        for message, arguments in cases.items():
            with tempfile.TemporaryDirectory() as tmp, self.subTest(message), self.assertRaisesRegex(ValueError, message):
                self.downloaded_release(Path(tmp), **arguments)

    def test_verify_of_a_plain_version_refuses_prerelease_state(self):
        with tempfile.TemporaryDirectory() as tmp, self.assertRaisesRegex(ValueError, 'unexpected release state'):
            self.downloaded_release(Path(tmp), state=dict(isPrerelease=True))

    def test_alpine_image_is_pinned_by_digest(self):
        self.assertRegex(r.ALPINE_IMAGE, r'^alpine:[0-9.]+@sha256:[0-9a-f]{64}$')

    def test_musl_proof_runs_the_archive_in_the_pinned_container(self):
        with tempfile.TemporaryDirectory() as tmp:
            archive = Path(tmp) / r.filename(VERSION, r.MUSL)
            zipped(archive, entries_for(VERSION, r.MUSL))
            calls = []
            def fake_sub(argv, **kwargs):
                calls.append(argv)
                return subprocess.CompletedProcess(argv, 0, 'out', '')
            with patch.object(r, 'run') as run, patch.object(r.subprocess, 'run', side_effect=fake_sub), \
                    patch.object(r, 'check_cli') as check:
                r.prove_musl(archive, VERSION)
            commands = [call.args[0] for call in run.call_args_list]
            self.assertEqual(commands[0][0], 'unzip')
            self.assertEqual(commands[1], ['docker', 'pull', r.ALPINE_IMAGE])
            self.assertEqual(commands[2][-3:-1], ['sh', '-c'])
            self.assertIn('ld-musl', commands[2][-1])
            self.assertEqual([c[-2:] for c in calls], [[f'/proof/marathon-{VERSION}/bin/marathon', 'version'],
                                                       [f'/proof/marathon-{VERSION}/bin/marathon', '--help']])
            for command in calls:
                self.assertEqual(command[:2], ['docker', 'run'])
                self.assertIn(r.ALPINE_IMAGE, command)
                self.assertEqual(command[command.index('--network') + 1], 'none')
            self.assertEqual([c.args[0] for c in check.call_args_list], [['version'], ['--help']])

    def test_musl_proof_refuses_an_archive_with_the_wrong_contents(self):
        with tempfile.TemporaryDirectory() as tmp:
            archive = Path(tmp) / r.filename(VERSION, r.MUSL)
            zipped(archive, {f'marathon-{VERSION}/bin/marathon': (0o755, b'x')})
            with patch.object(r, 'run') as run, self.assertRaisesRegex(ValueError, 'unexpected archive payload'):
                r.prove_musl(archive, VERSION)
            run.assert_not_called()

    def test_failed_build_stops_packaging(self):
        with tempfile.TemporaryDirectory() as tmp:
            args = ['release.py', 'build', '--target', r.DARWIN, '--output', tmp + '/out']
            with patch.object(r.sys, 'argv', args), patch.object(r, 'run', return_value='host: ' + r.DARWIN), patch.object(r.subprocess, 'run', return_value=subprocess.CompletedProcess([], 19)), patch.object(r, 'package') as package:
                with self.assertRaisesRegex(ValueError, 'cargo build failed'):
                    r.main()
                package.assert_not_called()

    def test_package_failure_leaves_no_archive(self):
        with tempfile.TemporaryDirectory(prefix='release paths with spaces ') as tmp:
            root = Path(tmp)
            with self.assertRaises(ValueError):
                r.package(root / 'missing', root / 'out', 'v1', r.DARWIN)
            binary = root / 'marathon'
            binary.write_bytes(b'placeholder')
            binary.chmod(0o755)
            with patch.object(r, 'run', return_value='Mach-O arm64'), patch.object(r, 'smoke', side_effect=ValueError('bad CLI')):
                with self.assertRaises(ValueError):
                    r.package(binary, root / 'out', 'v1', r.DARWIN)
            self.assertEqual(list((root / 'out').iterdir()), [])
            with patch.object(r, 'run', return_value='Mach-O arm64'), patch.object(r.zipfile, 'ZipFile', side_effect=OSError('full disk')):
                with self.assertRaises(OSError):
                    r.package(binary, root / 'out', 'v1', r.DARWIN)
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
                patch.object(r, 'smoke'), patch.object(r, 'third_party_text', return_value='third party text\n'):
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
            self.assertEqual({row['path']: row['mode'] for row in record['manifest']}, {
                'marathon-v1/LICENSE': '0644', 'marathon-v1/THIRD-PARTY-LICENSES': '0644', 'marathon-v1/bin/marathon': '0755'})
            with zipfile.ZipFile(archive) as z:
                self.assertEqual(sorted(z.namelist()), ['marathon-v1/LICENSE', 'marathon-v1/THIRD-PARTY-LICENSES', 'marathon-v1/bin/marathon'])
                self.assertEqual(z.getinfo('marathon-v1/bin/marathon').external_attr >> 16, stat.S_IFREG | 0o755)
                for name in ('LICENSE', 'THIRD-PARTY-LICENSES'):
                    self.assertEqual(z.getinfo(f'marathon-v1/{name}').external_attr >> 16, stat.S_IFREG | 0o644)
                self.assertEqual(z.read('marathon-v1/LICENSE'), r.LICENSE.read_bytes())
                self.assertEqual(z.read('marathon-v1/THIRD-PARTY-LICENSES'), b'third party text\n')

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
            args = ['release.py', 'build', '--target', r.GNU, '--output', tmp + '/out']
            with patch.object(r.sys, 'argv', args), patch.object(r, 'run', return_value='host: ' + r.GNU), \
                    patch.object(r.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0)) as sub, \
                    patch.object(r, 'package', return_value=Path('x')), contextlib.redirect_stdout(io.StringIO()):
                r.main()
            self.assertEqual(sub.call_args.args[0][:2], ['cargo', 'build'])

    def smoke_archive(self, root, licence=None, third_party=b'notices\n'):
        archive = root / 'release with spaces.zip'
        names = list(r.archive_modes(VERSION))
        zipped(archive, {names[0]: (0o644, licence if licence is not None else r.LICENSE.read_bytes()),
                         names[1]: (0o644, third_party),
                         names[2]: (0o755, b'#!/bin/sh\nexit 3\n')})
        return archive

    def test_smoke_extracts_for_real_and_reaches_the_cli_check(self):
        with tempfile.TemporaryDirectory() as tmp:
            with self.assertRaisesRegex(ValueError, 'embedded version mismatch'):
                r.smoke(self.smoke_archive(Path(tmp)), VERSION)

    def test_smoke_refuses_wrong_licence_files(self):
        with tempfile.TemporaryDirectory() as tmp:
            with self.assertRaisesRegex(ValueError, 'LICENSE differs'):
                r.smoke(self.smoke_archive(Path(tmp), licence=b'tampered'), VERSION)
            with self.assertRaisesRegex(ValueError, 'THIRD-PARTY-LICENSES is empty'):
                r.smoke(self.smoke_archive(Path(tmp), third_party=b''), VERSION)

    def test_smoke_refuses_a_binary_only_archive(self):
        with tempfile.TemporaryDirectory() as tmp:
            archive = Path(tmp) / 'old.zip'
            zipped(archive, {f'marathon-{VERSION}/bin/marathon': (0o755, b'x')})
            with self.assertRaisesRegex(ValueError, 'unexpected archive payload'):
                r.smoke(archive, VERSION)

    def test_cli_entry_points(self):
        cases = (('preflight', ['--version', VERSION]), ('check-absent', ['--version', VERSION]),
                 ('prepare', ['--artifacts', 'dist', '--version', VERSION]),
                 ('prove-musl', ['--archive', 'a.zip', '--version', VERSION]),
                 ('verify', ['--version', VERSION, '--target', r.MUSL, '--dir', 'out']),
                 ('publish', ['--artifacts', 'dist']))
        functions = {'preflight': 'preflight', 'check-absent': 'check_absent', 'prepare': 'prepare',
                     'prove-musl': 'prove_musl', 'verify': 'verify', 'publish': 'publish'}
        for command, arguments in cases:
            with self.subTest(command), patch.object(r.sys, 'argv', ['release.py', command, *arguments]), \
                    patch.object(r, functions[command]) as function:
                r.main()
                function.assert_called_once()


class ThirdPartyLicenseTests(unittest.TestCase):
    def crate(self, root, name, version, files, spdx='MIT', source='registry+https://example.org/index', relative=None):
        directory = root / (relative or f'registry/{name}-{version}')
        directory.mkdir(parents=True)
        for file, text in files.items():
            path = directory / file
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(text if isinstance(text, bytes) else text.encode())
        return dict(id=f'{name} {version}', name=name, version=version, license=spdx, source=source,
                    manifest_path=str(directory / 'Cargo.toml'))

    def metadata(self, root):
        packages = [
            self.crate(root, 'marathon-cli', '0.0.0', {}, spdx='GPL-2.0-only', source=None, relative='crates/marathon-cli'),
            self.crate(root, 'alpha', '1.0.0', {'LICENSE-MIT': 'alpha mit\n', 'LICENSE-APACHE': 'alpha apache\n',
                                                'src/license.rs': 'not a licence', 'src/lib.rs': 'code'}, spdx='MIT OR Apache-2.0'),
            self.crate(root, 'bundled', '2.0.0', {'COPYING': 'bundled copying', 'NOTICE.txt': 'bundled notice',
                                                  'third_party/c/LICENSE': 'nested c licence',
                                                  'LICENSES/Zlib.txt': 'reuse layout text'}, spdx='Zlib'),
            self.crate(root, 'bare', '3.0.0', {'src/lib.rs': 'code'}, spdx='ISC'),
            self.crate(root, 'nothing', '4.0.0', {}, spdx=None),
            self.crate(root, 'russh', '0.64.1', {'LICENSE': 'vendored russh licence', 'target/x/LICENSE': 'build output'},
                       spdx='Apache-2.0', source=None, relative='vendor/russh'),
            self.crate(root, 'latin', '5.0.0', {'LICENSE': b'caf\xe9\n'}),
            self.crate(root, 'only-for-tests', '9.0.0', {'LICENSE': 'dev dependency licence'}),
        ]
        return dict(packages=packages, workspace_members=['marathon-cli 0.0.0'], workspace_root=str(root))

    CRATES = {('marathon-cli', 'v0.0.0'), ('alpha', 'v1.0.0'), ('bundled', 'v2.0.0'), ('bare', 'v3.0.0'),
              ('nothing', 'v4.0.0'), ('russh', 'v0.64.1'), ('latin', 'v5.0.0')}

    def rendered(self, root):
        return tpl.render(r.MUSL, self.metadata(root), self.CRATES)

    def test_lists_each_crate_with_version_expression_and_texts(self):
        with tempfile.TemporaryDirectory() as tmp:
            text = self.rendered(Path(tmp))
        self.assertIn(r.MUSL, text.splitlines()[0])
        self.assertIn('6 crates:', text)
        self.assertIn('alpha 1.0.0: MIT OR Apache-2.0', text)
        self.assertIn('SPDX expression: MIT OR Apache-2.0', text)
        for expected in ('--- LICENSE-MIT', 'alpha mit', '--- LICENSE-APACHE', 'alpha apache', 'bundled copying', 'bundled notice',
                         '--- third_party/c/LICENSE', 'nested c licence', '--- LICENSES/Zlib.txt', 'reuse layout text'):
            self.assertIn(expected, text)
        self.assertNotIn('not a licence', text)

    def test_the_vendored_crate_is_included_with_a_relative_path(self):
        with tempfile.TemporaryDirectory() as tmp:
            text = self.rendered(Path(tmp))
            self.assertIn('vendored russh licence', text)
            self.assertIn('Source: vendored in this repository at vendor/russh', text)
            self.assertNotIn('build output', text)
            self.assertNotIn(tmp, text)

    def test_a_crate_without_a_licence_file_is_stated_never_silent(self):
        with tempfile.TemporaryDirectory() as tmp:
            text = self.rendered(Path(tmp))
        self.assertIn('The crate source contains no licence file. Its declared licence is: ISC', text)
        self.assertIn('nothing 4.0.0: no licence declared', text)
        self.assertIn('The crate source contains no licence file. Its declared licence is: none declared', text)

    def test_only_compiled_crates_are_listed_without_the_workspace(self):
        with tempfile.TemporaryDirectory() as tmp:
            text = self.rendered(Path(tmp))
        self.assertNotIn('only-for-tests', text)
        self.assertNotIn('dev dependency licence', text)
        self.assertNotIn('marathon-cli 0.0.0', text)

    def test_undecodable_licence_text_does_not_stop_the_build(self):
        with tempfile.TemporaryDirectory() as tmp:
            self.assertIn('caf\ufffd', self.rendered(Path(tmp)))

    def test_output_is_deterministic(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            metadata = self.metadata(root)
            first = tpl.render(r.MUSL, metadata, self.CRATES)
            metadata['packages'].reverse()
            self.assertEqual(first, tpl.render(r.MUSL, metadata, set(reversed(sorted(self.CRATES)))))

    def test_a_compiled_crate_unknown_to_metadata_is_an_error(self):
        with tempfile.TemporaryDirectory() as tmp, self.assertRaisesRegex(ValueError, 'expected one metadata package'):
            tpl.render(r.MUSL, self.metadata(Path(tmp)), self.CRATES | {('ghost', 'v1.0.0')})

    def test_cargo_tree_selects_the_normal_graph_of_marathon_cli(self):
        tree = ('marathon-cli v0.0.0 (/abs/crates/marathon-cli)\nalpha v1.0.0\n'
                'alpha v1.0.0 (*)\nmacros v2.0.0 (proc-macro)\nrussh v0.64.1 (/abs/vendor/russh)\n\n')
        with patch.object(tpl, 'cargo', return_value=tree) as cargo:
            crates = tpl.compiled_crates(r.MUSL)
        self.assertEqual(crates, {('marathon-cli', 'v0.0.0'), ('alpha', 'v1.0.0'), ('macros', 'v2.0.0'), ('russh', 'v0.64.1')})
        arguments = cargo.call_args.args
        self.assertEqual(arguments[:6], ('tree', '--locked', '-p', 'marathon-cli', '-e', 'normal'))
        self.assertEqual(arguments[arguments.index('--target') + 1], r.MUSL)

    def test_metadata_is_read_with_the_lockfile_for_the_target(self):
        with tempfile.TemporaryDirectory() as tmp:
            metadata = json.dumps(self.metadata(Path(tmp)))
            def answer(*arguments):
                return metadata if arguments[0] == 'metadata' else 'marathon-cli v0.0.0\n'
            with patch.object(tpl, 'cargo', side_effect=answer) as cargo, patch.object(tpl.sys, 'argv', ['x', '--target', r.MUSL]), \
                    contextlib.redirect_stdout(io.TextIOWrapper(io.BytesIO())):
                tpl.main()
        first = cargo.call_args_list[0].args
        self.assertEqual(first[:2], ('metadata', '--locked'))
        self.assertEqual(first[first.index('--filter-platform') + 1], r.MUSL)


if __name__ == '__main__':
    unittest.main()
