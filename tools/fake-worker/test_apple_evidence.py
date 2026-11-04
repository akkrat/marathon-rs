#!/usr/bin/env python3
"""Offline validation and adversarial evidence checks; no JDK/Xcode needed."""
import base64
import json
from pathlib import Path
import shutil
import sys
import tempfile
import unittest
import zipfile
sys.path.insert(0, str(Path(__file__).resolve().parent))
import apple_evidence as e
import apple_scenario
from unittest.mock import patch


class Evidence(unittest.TestCase):
    def test_cargo_reported_executable_is_used(self):
        executable = Path('/custom-target/debug/fake-worker')
        artifact = dict(reason='compiler-artifact', target=dict(name='fake-worker'), executable=str(executable))
        with patch.object(apple_scenario.subprocess, 'run') as build:
            build.return_value.stdout = json.dumps(artifact) + '\n'
            self.assertEqual(apple_scenario.build_worker(), executable)
            self.assertIn('--message-format=json', build.call_args.args[0])
            self.assertTrue(build.call_args.kwargs['check'])

    def test_directory_inode_sizes_do_not_fail_replay(self):
        original=e.ROOT/'fixtures/fake-worker/apple/ios-smoke/expected/raw'
        with tempfile.TemporaryDirectory() as temporary:
            work=Path(temporary)/'evidence';shutil.copytree(original,work)
            transcript=e.rows(work/'transcript.jsonl')
            directories=[f for f in transcript[-1]['detail']['files'] if f['kind']=='directory']
            self.assertTrue(directories)
            for entry in directories:
                entry['size']+=4096
            (work/'transcript.jsonl').write_text(''.join(json.dumps(r)+'\n' for r in transcript))
            e.compare(work,original)

    def test_retained_filesystem_changes_fail_replay(self):
        original=e.ROOT/'fixtures/fake-worker/apple/ios-media/expected/raw'
        with tempfile.TemporaryDirectory() as temporary:
            work=Path(temporary)/'evidence';shutil.copytree(original,work)
            transcript=e.rows(work/'transcript.jsonl')
            entry=next(f for f in transcript[-1]['detail']['files'] if f['path'].endswith('-temp.mp4'))
            entry['mode']^=0o100
            (work/'transcript.jsonl').write_text(''.join(json.dumps(r)+'\n' for r in transcript))
            with self.assertRaisesRegex(AssertionError,'filesystem'):
                e.compare(work,original)

    def test_wrong_tcc_permission_is_rejected(self):
        def mutate(w):
            state=e.load(w/'state.json')
            state['permissions'][0][1]=state['permissions'][0][1].replace('kTCCServiceAccessibility','kTCCServiceDeveloperTool')
            e.save(w/'state.json',state)
        self.check_mutation(mutate,name='macos-smoke')

    def test_measured_log_partition_is_explicit(self):
        root=e.ROOT/'fixtures/fake-worker/apple/diagnostics/macos-log-partition'
        a=e.load(root/'with-batch-tail.json');b=e.load(root/'without-batch-tail.json')
        self.assertEqual(e.report_families(a),e.report_families(b))
        tail=next(x for x in a if x['kind']=='file' and x['path'].startswith('logs/') and not x['path'].endswith('.log'))
        with self.assertRaises(AssertionError):
            e.report_families(a+[tail])

    def test_bundle_archives(self):
        root=e.ROOT/'fixtures/bundles'
        profile=e.load(root/'build-profile.json')
        for archive in profile['archives']:
            data=(root/archive['path']).read_bytes()
            self.assertEqual(e.sha(data),archive['sha256'])
            self.assertEqual(len(data),archive['size'])
            tree=e.load(root/(Path(archive['path']).stem+'-tree.json'))
            with zipfile.ZipFile(root/archive['path']) as z:
                self.assertEqual(sorted(z.namelist()),sorted(f['path'] for f in tree))
                for entry in tree:
                    payload=z.read(entry['path'])
                    self.assertEqual(e.sha(payload),entry['sha256'])
                    self.assertEqual(len(payload),entry['size'])
                    self.assertEqual(oct((z.getinfo(entry['path']).external_attr>>16)&0o7777),entry['mode'])

    def test_source_media_hashes(self):
        root=e.ROOT/'fixtures/fake-worker/apple/media'
        for entry in e.load(root/'manifest.json'):
            payload=(root/entry['path']).read_bytes()
            self.assertEqual(e.sha(payload),entry['sha256'])
            self.assertEqual(len(payload),entry['size'])
            if entry['path'].endswith('.mp4'):
                self.assertEqual(payload[4:8],b'ftyp')
                self.assertIn(b'avcC',payload)
            else:
                self.assertEqual(payload[:8],b'\x89PNG\r\n\x1a\n')

    def test_recorded_suites(self):
        for name in ['ios-smoke', 'macos-smoke', 'ios-media', 'ios-screenshot']:
            scenario=e.ROOT / 'fixtures/fake-worker/apple' / name
            spec=e.load(scenario / 'script/world.json')
            with self.subTest(suite=name):
                e.validate(scenario / 'expected/raw',spec)
                normalized=scenario / 'expected/normalized'
                self.assertEqual(sorted(p.name for p in normalized.iterdir()),['ready.json','semantic.json'])
                with tempfile.TemporaryDirectory() as temporary:
                    actual=Path(temporary)/'normalized'
                    e.normalize(scenario / 'expected/raw',actual)
                    for name in ['ready.json','semantic.json']:
                        self.assertEqual(e.load(actual/name),e.load(normalized/name))

    def test_marked_parser_chunks_must_stay_zero_filled(self):
        original=e.ROOT/'fixtures/fake-worker/apple/macos-smoke/expected/raw'
        spec=e.load(e.ROOT/'fixtures/fake-worker/apple/macos-smoke/script/world.json')
        with tempfile.TemporaryDirectory() as temporary:
            work=Path(temporary)/'evidence';shutil.copytree(original,work)
            transcript=e.rows(work/'transcript.jsonl')
            marked=[r for r in transcript if r['detail'].get('redacted')]
            self.assertTrue(marked)
            for row in marked:
                data=b'x'*row['detail']['size']
                row['detail'].update(base64=base64.b64encode(data).decode(),sha256=e.sha(data))
            (work/'transcript.jsonl').write_text(''.join(json.dumps(r)+'\n' for r in transcript))
            manifest=e.load(work/'manifest.json')
            manifest['artifact_hashes']['transcript.jsonl']=e.sha((work/'transcript.jsonl').read_bytes())
            e.save(work/'manifest.json',manifest)
            with self.assertRaisesRegex(AssertionError,'third-party library bytes'):
                e.validate(work,spec)

    def check_mutation(self, mutate, name='ios-media'):
        scenario=e.ROOT / 'fixtures/fake-worker/apple' / name
        with tempfile.TemporaryDirectory() as temporary:
            work=Path(temporary)/'evidence'
            shutil.copytree(scenario / 'expected/raw',work)
            mutate(work)
            # A coherent self-comparison must still reject incomplete evidence.
            # Rehashing avoids relying only on a stale digest for the rejection.
            m=e.load(work/'manifest.json')
            m['artifact_hashes']={n:e.sha((work/n).read_bytes()) for n in e.REQUIRED if (work/n).exists()}
            e.save(work/'manifest.json',m)
            with self.assertRaises((AssertionError,FileNotFoundError)):
                e.validate(work,e.load(scenario/'script/world.json'))

    def test_missing_artifact(self):
        self.check_mutation(lambda w:(w/'media/0').unlink())

    def test_missing_declared_transcript(self):
        self.check_mutation(lambda w:(w/'transcript.jsonl').unlink())

    def test_changed_status(self):
        def mutate(w):
            r=e.load(w/'results.json');r[0]['status']='PASSED';e.save(w/'results.json',r)
        self.check_mutation(mutate)

    def test_wrong_command(self):
        def mutate(w):
            rows=e.rows(w/'transcript.jsonl')
            row=next(r for r in rows if r['event']=='exec')
            data=b'xcrun simctl invented-command'
            row['detail'].update(base64=base64.b64encode(data).decode(),size=len(data),sha256=e.sha(data))
            (w/'transcript.jsonl').write_text(''.join(json.dumps(r)+'\n' for r in rows))
        self.check_mutation(mutate)

    def test_missing_cleanup(self):
        def mutate(w):
            rows=e.rows(w/'transcript.jsonl')[:-1]
            (w/'transcript.jsonl').write_text(''.join(json.dumps(r)+'\n' for r in rows))
        self.check_mutation(mutate)

    def test_failed_and_interrupted_evidence(self):
        for outcome in ['running','failed','timeout','skipped']:
            def mutate(w):
                m=e.load(w/'manifest.json');m['terminal_outcome']=outcome;e.save(w/'manifest.json',m)
            self.check_mutation(mutate)

    def test_unconsumed_expectation(self):
        def mutate(w):
            s=e.load(w/'state.json');s['calls']['batch']=0;e.save(w/'state.json',s)
        self.check_mutation(mutate)

    def test_catalogue_quotes_and_deferred_coverage(self):
        forms=e.load(e.ROOT/'fixtures/fake-worker/apple/command-catalogue.json')['forms']
        self.assertEqual(len({f['id'] for f in forms}),len(forms))
        for form in forms:
            self.assertTrue(form['source'] and form['timeout'] and form['contract'])
            if form['reachability']=='deferred':
                self.assertTrue(form['deferred_reason'] and form['owner'])
        self.assertTrue(e.glob('x[ab]*','x[ab]tail'))
        self.assertFalse(e.glob('x[ab]*','xatail'))


if __name__=='__main__':
    unittest.main()
