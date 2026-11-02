#!/usr/bin/env python3
"""Adversarial checks go through the production comparator, with recomputed digests."""
import base64
import json
import os
from pathlib import Path
import shutil
import sys
import tempfile
import unittest
import runpy
from unittest import mock
sys.path.insert(0, str(Path(__file__).resolve().parent))
import evidence as e

ROOT = Path(__file__).resolve().parents[2]
DRIVER = runpy.run_path(str(ROOT / 'tools/differential/run'))


def synthetic_provenance():
    return dict(test='synthetic adversarial witness, not Kotlin evidence',
                jars={'synthetic.jar':e.sha(b'jar')}, patches={'synthetic.patch':e.sha(b'patch')},
                jdk='synthetic test JDK', process=dict(argv=['synthetic'], exit=0, watchdog=False))


class EvidenceTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.tree = self.root / 'tree'
        self.tree.mkdir()
        (self.tree / 'empty').mkdir()
        (self.tree / 'log').write_bytes(b'literal 12345678-1234-1234-1234-123456789abc\n')
        (self.tree / 'result.json').write_bytes(b'{"status":"PASSED","failure":"","attachments":["log"],"tests":["a","b"]}')
        (self.tree / 'screen.gif').write_bytes(b'GIF89a\x01\x00\x01\x00\x80\x00\x00\x00\x00\x00\xff\xff\xff!\xf9\x04\x00\x0a\x00\x00\x00,\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02D\x01\x00;')
        os.symlink('log', self.tree / 'link')
        self.outcome = dict(cli=1, fake=0, harness=0, watchdog=False, joined=True, unused=[], unmatched=[])
        self.contract = dict(format='marathon-differential/contract@1', scenario='artifact', roles={}, paths=[
            'reports', 'reports/empty', 'reports/log', 'reports/result.json', 'reports/screen.gif', 'reports/link',
            'stdout', 'stderr', 'transcript.json'], normalization={}, outcome=self.outcome)
        self.artifacts = dict(stdout=b'', stderr=b'Marathon execution failed\n',
                              **{'transcript.json':b'{"commands":[["xcodebuild","-destination-timeout","30"]],"cleanup":["close","join"],"terminal":"complete"}'})
        self.expected = self.record('expected', 'jvm')

    def record(self, name, implementation='native', identities=None):
        return e.write_record(self.root / name, self.contract, implementation, {'reports':self.tree},
                              self.artifacts, self.outcome, dict(synthetic_provenance(), terminal_observation=dict(self.outcome)), identities)

    def reject_changed_tree(self, mutation):
        mutation()
        try:
            actual = self.record('actual')
            e.compare(self.expected, actual, self.contract)
        except e.Invalid:
            return
        self.fail('comparator accepted a changed observable')

    def test_exact_positive_and_symlink_without_dereference(self):
        actual = self.record('actual')
        e.compare(self.expected, actual, self.contract)
        self.assertEqual(next(x for x in e.load(actual / 'evidence.json')['entries'] if x['kind'] == 'link')['target'], 'log')

    def test_missing_file(self):
        self.reject_changed_tree(lambda: (self.tree / 'log').unlink())

    def test_missing_empty_directory(self):
        self.reject_changed_tree(lambda: (self.tree / 'empty').rmdir())

    def test_extra_file(self):
        self.reject_changed_tree(lambda: (self.tree / 'extra').write_bytes(b'extra'))

    def test_missing_and_extra_file_diagnostics_retain_mismatch(self):
        self.contract['paths'].remove('reports/log')
        expected = self.record('diagnostic-expected', 'jvm')
        (self.tree / 'log').unlink()
        actual = self.record('diagnostic-actual')
        for label, left, right in [('missing', expected, actual), ('extra', actual, expected)]:
            with self.subTest(label=label):
                mismatch = self.root / label
                with self.assertRaisesRegex(e.Invalid, 'observable differences: reports/log'):
                    e.compare(left, right, self.contract, mismatch)
                error = (mismatch / 'error.txt').read_text()
                self.assertIn('content differs at', error)
                self.assertNotIn('NoneType', error)
                self.assertEqual(e.load(mismatch / 'differences.json'), ['reports/log'])
                self.assertTrue((mismatch / 'expected/evidence.json').is_file())
                self.assertTrue((mismatch / 'actual/evidence.json').is_file())

    def test_byte_content_difference_names_the_first_differing_line(self):
        (self.tree / 'log').write_bytes(b'same\nsecond line\n')
        expected = self.record('line-expected', 'jvm')
        (self.tree / 'log').write_bytes(b'same\nother line\n')
        actual = self.record('line-actual')
        with self.assertRaisesRegex(e.Invalid, "line 2: 'second line' -> 'other line'"):
            e.compare(expected, actual, self.contract)

    def test_sshpass_warning_must_match_the_capture_host(self):
        import vendor_console as vc
        warned = (vc.SSHPASS_WARNING + 'Marathon execution failed\n').encode()
        quiet = b'Marathon execution failed\n'
        self.assertEqual(vc.normalize(warned, {}, 'native', 'stderr', {vc.SSHPASS_FACT: False}), quiet)
        self.assertEqual(vc.normalize(quiet, {}, 'native', 'stderr', {vc.SSHPASS_FACT: True}), quiet)
        for data, present in [(quiet, False), (warned, True)]:
            with self.subTest(present=present), self.assertRaisesRegex(e.Invalid, 'sshpass warning'):
                vc.normalize(data, {}, 'native', 'stderr', {vc.SSHPASS_FACT: present})

    def test_artifact_queue_wait_moves_only_within_its_admission_window(self):
        import vendor_console as vc
        values = dict(sandbox='/sandbox', safe_serial='SAFE', serial='SERIAL', port='1')
        p = 'D 00:00:00.000 [t] <c.m.marathon.apple.ArtifactProcessor> '
        admission = p + 'Artifact admission for SERIAL, batch B took 0 ms; outstanding=1, on worker 127.0.0.1:1=1\n'
        wait = p + 'Artifact queue wait for SERIAL, batch B: 0 ms\n'
        final = p + 'Artifact finalization took 0 ms\n'
        barrier = p + 'Artifact barrier for SERIAL took 0 ms\n'
        other_barrier = p + 'Artifact barrier for OTHER took 0 ms\n'
        progress = '00% | [omni]-[SERIAL] a.B#c passed\n'
        canonical = vc.normalize((admission + wait + progress + final).encode(), values, 'native', 'stdout')
        for lines in [[wait, admission, progress, final], [admission, progress, wait, final]]:
            with self.subTest(lines=lines):
                self.assertEqual(vc.normalize(''.join(lines).encode(), values, 'native', 'stdout'), canonical)
        # Another owner's barrier does not close the window; this owner's barrier does, because
        # Kotlin's await logs only after the work, which logs its wait first, has completed.
        canonical = vc.normalize((admission + wait + other_barrier + final).encode(), values, 'native', 'stdout')
        self.assertEqual(vc.normalize((admission + other_barrier + wait + final).encode(), values, 'native', 'stdout'), canonical)
        for lines in [[admission, progress, final, wait], [wait, progress, admission, final],
                      [admission, barrier, wait, final], [admission, progress, barrier, wait, final]]:
            with self.subTest(lines=lines), self.assertRaisesRegex(e.Invalid, 'admission window'):
                vc.normalize(''.join(lines).encode(), values, 'native', 'stdout')

    def test_mode(self):
        self.reject_changed_tree(lambda: (self.tree / 'log').chmod(0o700))

    def test_changed_status_failure_attachment_and_array_order(self):
        original = (self.tree / 'result.json').read_bytes()
        for old, new in [(b'PASSED',b'FAILURE'), (b'"failure":""', b'"failure":"broken"'),
                         (b'["log"]',b'["wrong"]'),(b'["a","b"]',b'["b","a"]')]:
            with self.subTest(old=old):
                (self.tree / 'result.json').write_bytes(original.replace(old,new))
                actual = self.record('mutation-' + str(len(list(self.root.iterdir()))))
                with self.assertRaises(e.Invalid):
                    e.compare(self.expected, actual, self.contract)

    def test_gif_delay(self):
        def mutate():
            p = self.tree / 'screen.gif'
            p.write_bytes(p.read_bytes().replace(b'!\xf9\x04\x00\x0a',b'!\xf9\x04\x00\x0b'))
        self.reject_changed_tree(mutate)

    def test_literal_uuid_message_is_not_normalized(self):
        self.reject_changed_tree(lambda: (self.tree / 'log').write_bytes(b'literal 87654321-1234-1234-1234-123456789abc\n'))

    def test_stdout_and_stderr_stay_separate(self):
        self.artifacts['stdout'], self.artifacts['stderr'] = self.artifacts['stderr'], self.artifacts['stdout']
        with self.assertRaises(e.Invalid):
            e.compare(self.expected, self.record('actual'), self.contract)

    def test_exit_watchdog_cleanup_unused_tape_and_unmatched_command(self):
        for key, value in [('cli',0),('fake',1),('harness',1),('watchdog',True),('joined',False),
                           ('unused',['id:1']),('unmatched',['unknown'])]:
            with self.subTest(key=key):
                actual = self.root / ('bad-' + key)
                shutil.copytree(self.expected, actual)
                record = e.load(actual / 'evidence.json')
                record['outcome'][key] = value
                (actual / 'evidence.json').write_bytes(e.encoded(record))
                with self.assertRaises(e.Invalid):
                    e.compare(actual, actual, self.contract)

    def test_removed_cleanup_and_truncated_transcript(self):
        for data in [b'{"commands":[],"cleanup":[],"terminal":"complete"}', self.artifacts['transcript.json'][:-1]]:
            self.artifacts['transcript.json'] = data
            with self.assertRaises((e.Invalid, ValueError)):
                actual = self.record('mutated-' + str(len(list(self.root.iterdir()))))
                e.compare(self.expected, actual, self.contract)

    def test_missing_payload_and_truncated_manifest_even_against_self(self):
        for change in ['payload','manifest']:
            actual = self.root / change
            shutil.copytree(self.expected, actual)
            if change == 'payload':
                next((actual / 'blobs').iterdir()).unlink()
            else:
                record = e.load(actual / 'evidence.json')
                record['entries'] = record['entries'][:-1]
                (actual / 'evidence.json').write_bytes(e.encoded(record))
            with self.assertRaises(e.Invalid):
                e.compare(actual, actual, self.contract)

    def test_extra_payload_and_duplicate_path(self):
        (self.expected / 'blobs' / 'extra').write_bytes(b'extra')
        with self.assertRaises(e.Invalid):
            e.validate(self.expected, self.contract)
        (self.expected / 'blobs' / 'extra').unlink()
        record = e.load(self.expected / 'evidence.json')
        record['entries'].append(record['entries'][0])
        (self.expected / 'evidence.json').write_bytes(e.encoded(record))
        with self.assertRaises(e.Invalid):
            e.validate(self.expected, self.contract)

    def test_typed_references_preserve_surrounding_bytes_and_identity(self):
        a = '00000000-0000-0000-0000-000000000001'
        b = '00000000-0000-0000-0000-000000000002'
        self.contract['roles'] = {'batch':'batch'}
        self.contract['paths'].append('reports/{batch}.txt')
        self.contract['normalization'] = {'reports/result.json': {'mode':'bytes', 'rules':[
            {'kind':'json-string','pointer':'/id','value':'{batch}'},
            {'kind':'json-string','pointer':'/target','value':'{batch}.txt'}]}}
        (self.tree / (a+'.txt')).write_bytes(b'attachment')
        literal = 'literal ' + a
        data = {'id':a, 'target':a+'.txt', 'message':literal}
        (self.tree / 'result.json').write_bytes(e.encoded(data))
        expected = self.record('typed-jvm', 'jvm', {'batch':a})
        (self.tree / (a+'.txt')).rename(self.tree / (b+'.txt'))
        data.update(id=b, target=b+'.txt')
        (self.tree / 'result.json').write_bytes(e.encoded(data))
        actual = self.record('typed-native', identities={'batch':b})
        e.compare(expected, actual, self.contract)
        data['message'] = 'literal ' + b
        (self.tree / 'result.json').write_bytes(e.encoded(data))
        with self.assertRaises(e.Invalid):
            e.compare(expected, self.record('literal-mutant', identities={'batch':b}), self.contract)
        data['target'] = a + '.txt'
        (self.tree / 'result.json').write_bytes(e.encoded(data))
        with self.assertRaisesRegex(e.Invalid, 'inconsistent reference'):
            e.compare(expected, self.record('bad-reference', identities={'batch':b}), self.contract)

    def test_uuid_collision(self):
        self.contract['roles'] = {'one':'uuid','two':'uuid'}
        with self.assertRaisesRegex(e.Invalid, 'identity collision'):
            self.record('collision', identities={'one':'00000000-0000-0000-0000-000000000001',
                                                'two':'00000000-0000-0000-0000-000000000001'})

    def test_path_collision(self):
        self.contract['roles'] = {'name':'identity'}
        self.contract['paths'] += ['reports/{name}']
        (self.tree / '{name}').write_bytes(b'literal path')
        with self.assertRaisesRegex(e.Invalid, 'path .*collision'):
            self.record('collision', identities={'name':'log'})

    def test_no_failed_native_or_partial_golden_promotion(self):
        before = (self.expected / 'evidence.json').read_bytes()
        native = self.record('native')
        with self.assertRaisesRegex(e.Invalid, 'JVM'):
            e.publish(native, self.root / 'new-golden', self.contract)
        with self.assertRaisesRegex(e.Invalid, 'already exists'):
            e.publish(self.expected, self.expected, self.contract)
        self.assertEqual(before, (self.expected / 'evidence.json').read_bytes())
        record = e.load(self.expected / 'evidence.json')
        record['outcome']['watchdog'] = True
        (self.expected / 'evidence.json').write_bytes(e.encoded(record))
        with self.assertRaises(e.Invalid):
            e.publish(self.expected, self.root / 'new-golden', self.contract)
        self.assertFalse((self.root / 'new-golden').exists())

    def test_lossless_binary_storage_and_corruption(self):
        compressed = e.write_record(self.root/'compressed',self.contract,'jvm',{'reports':self.tree},self.artifacts,
            self.outcome,dict(synthetic_provenance(),terminal_observation=self.outcome),compressed=True)
        e.compare(self.expected,compressed,self.contract)
        row = next(r for r in e.load(compressed/'evidence.json')['entries'] if r['path'] == 'reports/log')
        blob = compressed/'blobs'/row['sha256']
        self.assertIn(b'\0',blob.read_bytes()[:10])
        blob.write_bytes(blob.read_bytes()[:-2])
        with self.assertRaises(e.Invalid):e.validate(compressed,self.contract)

    def test_mismatch_artifacts_keep_both_raw_records(self):
        (self.tree / 'log').write_bytes(b'changed')
        actual = self.record('actual')
        with self.assertRaises(e.Invalid):
            e.compare(self.expected, actual, self.contract, self.root / 'mismatch')
        self.assertEqual(e.load(self.root / 'mismatch/differences.json'), ['reports/log'])
        e.validate(self.root / 'mismatch/expected', self.contract)
        e.validate(self.root / 'mismatch/actual', self.contract)

    def test_duplicate_keys_and_opaque_bytes(self):
        with self.assertRaises(e.Invalid):
            e.json_spans(b'{"id":"a","id":"b"}')
        data=b'\x00\xff\xc0'
        self.assertEqual(e.normalize(data, [], {}), data)

    def test_u30_real_jvm_wire_and_host_argv_witnesses(self):
        source = ROOT / 'fixtures/fake-worker/apple/ios-smoke/expected/raw'
        artifacts = {name:(source / name).read_bytes() for name in ['host-argv.jsonl','transcript.jsonl']}
        c = dict(format='marathon-differential/contract@1', scenario='artifact', roles={}, paths=list(artifacts),
                 normalization={}, outcome=self.outcome)
        expected = e.write_record(self.root / 'wire-jvm', c, 'jvm', {}, artifacts, self.outcome,
                                  dict(synthetic_provenance(), source=str(source.relative_to(ROOT)), scope='retained raw wire witness only', terminal_observation=self.outcome))
        for n, (path, old, new) in enumerate([
            ('host-argv.jsonl', b'--links', b'--no-links'),
            ('host-argv.jsonl', b'--rsh', b'--rshX'),
            ('transcript.jsonl', b'-destination-timeout', b'-destination-timeoutX')]):
            with self.subTest(option=old):
                changed = dict(artifacts)
                if path == 'transcript.jsonl':
                    rows = [json.loads(line) for line in changed[path].splitlines()]
                    row = next(row for row in rows if row['event'] == 'exec' and
                               old in base64.b64decode(row['detail']['base64']))
                    payload = base64.b64decode(row['detail']['base64']).replace(old, new, 1)
                    row['detail'].update(base64=base64.b64encode(payload).decode(), size=len(payload), sha256=e.sha(payload))
                    changed[path] = b''.join(json.dumps(row, separators=(',', ':')).encode() + b'\n' for row in rows)
                else:
                    self.assertTrue(old in changed[path], 'missing command option witness')
                    changed[path] = changed[path].replace(old, new, 1)
                actual = e.write_record(self.root / f'wire-mutant-{n}', c, 'native', {}, changed,
                                        self.outcome, {'scope':'mutation, all hashes recomputed', 'terminal_observation':self.outcome})
                with self.assertRaises(e.Invalid):
                    e.compare(expected, actual, c)

    def test_causal_streams_aggregate_chunks_without_sorting_commands(self):
        rows = [dict(lane='a',seq=0,event='command',argv=['xcodebuild','-destination-timeout','30'],
                     stdin='',timeout_ms=100,idle_ms=10,barriers=['ready']),
                dict(lane='a',seq=1,event='chunk',stream='stdout',base64='YWI='),
                dict(lane='a',seq=2,event='end',exit=0,outcome='completed')]
        split = [rows[0], dict(lane='a',seq=1,event='chunk',stream='stdout',base64='YQ=='),
                 dict(lane='a',seq=2,event='chunk',stream='stdout',base64='Yg=='), dict(rows[2],seq=3)]
        self.contract['normalization'] = {'transcript.json': {'mode':'causal-streams','rules':[]}}
        self.artifacts['transcript.json'] = e.encoded(rows)
        expected = self.record('causal-jvm','jvm')
        self.artifacts['transcript.json'] = e.encoded(split)
        actual = self.record('causal-native')
        e.compare(expected, actual, self.contract)
        for mutation in [split[:-1], [split[0],split[2],split[1],split[3]],
                         [split[0],dict(split[1],stream='stderr'),split[2],split[3]]]:
            self.artifacts['transcript.json'] = e.encoded(mutation)
            mutated = self.record('causal-mutant-'+str(len(list(self.root.iterdir()))))
            with self.assertRaises(e.Invalid):
                e.compare(expected, mutated, self.contract)

    def test_allure_structure_keeps_array_order_nulls_and_references(self):
        allure = self.tree / 'allure-results'
        allure.mkdir()
        report = allure / 'result.json'
        self.contract['normalization'] = {'reports/allure-results/result.json': {'mode':'allure-json','rules':[]}}
        report.write_bytes(b'{"status":"passed","attachments":["a","b"],"optional":null}')
        expected = self.record('allure-jvm','jvm')
        report.write_bytes(b'{"optional":null,"attachments":["a","b"],"status":"passed"}')
        e.compare(expected, self.record('allure-native'), self.contract)
        for n, data in enumerate([b'{"status":"passed","attachments":["b","a"],"optional":null}',
                                 b'{"status":"passed","attachments":["a","b"]}',
                                 b'{"status":"passed","attachments":["a","wrong"],"optional":null}']):
            report.write_bytes(data)
            with self.assertRaises(e.Invalid):
                e.compare(expected, self.record('allure-mutant-'+str(n)), self.contract)

    def test_finite_variants_are_complete_and_never_optional_logs(self):
        (self.tree / 'log').write_bytes(b'alternate complete JVM log')
        alternate = self.record('variant','jvm')
        e.compare_variants([self.expected,alternate], self.record('actual'), self.contract)
        (self.tree / 'log').unlink()
        with self.assertRaises(e.Invalid):
            e.compare_variants([self.expected,alternate], self.record('missing'), self.contract)
        record=e.load(alternate / 'evidence.json')
        record['outcome']['unused']=['id']
        (alternate / 'evidence.json').write_bytes(e.encoded(record))
        with self.assertRaises(e.Invalid):
            e.compare_variants([self.expected,alternate], self.expected, self.contract)

    def test_relabelled_native_cannot_validate_or_publish(self):
        native = self.record('relabelled')
        record = e.load(native / 'evidence.json')
        record.update(implementation='jvm', provenance={'probe':1})
        for field in ('all', 'jars', 'jdk', 'patches', 'process'):
            with self.subTest(field=field):
                record['provenance'] = {'probe':1} if field == 'all' else synthetic_provenance()
                if field != 'all':
                    del record['provenance'][field]
                (native / 'evidence.json').write_bytes(e.encoded(record))
                with self.assertRaisesRegex(e.Invalid, 'JVM'):
                    e.validate(native, self.contract)
                with self.assertRaisesRegex(e.Invalid, 'JVM'):
                    e.publish(native, self.root / 'forged-golden', self.contract)
        self.assertFalse((self.root / 'forged-golden').exists())

    def test_process_provenance_requires_typed_result(self):
        for process in ({}, {'argv':[], 'exit':0, 'watchdog':False},
                        {'argv':['java'], 'exit':False, 'watchdog':False},
                        {'argv':['java'], 'exit':0, 'watchdog':'false'}):
            with self.subTest(process=process), self.assertRaises(e.Invalid):
                e.validate_jvm_provenance(dict(synthetic_provenance(), process=process))

    def test_pack_observes_process_and_requires_tape_accounting(self):
        # Reuse the reviewer's independent regen transcript: unhooked parse has
        # hooksReport=None. It is valid only for a scenario without tapes.
        source = ROOT / 'fixtures/differential/portable/parse/jvm-v1'
        facts = e.load(source / 'evidence.json')['provenance']
        transcript = facts['driver_transcript']
        self.assertIsNone(transcript['hooksReport'])
        raw = self.root / 'raw'
        raw.mkdir()
        (raw / 'reports').mkdir()
        (raw / 'reports/parse_result.yml').write_bytes(b'parse')
        observation = dict(outcome=transcript['outcome'], log=[])
        (raw / 'observation.json').write_bytes(e.encoded(observation))
        pack = DRIVER['pack']
        with mock.patch.dict(pack.__globals__, provenance=lambda _: {}):
            for n, process in enumerate((dict(argv=['probe'], exit=9, watchdog=False),
                                         dict(argv=['probe'], exit=0, watchdog=True))):
                with self.subTest(process=process), self.assertRaises(e.Invalid):
                    pack('parse', 'native', raw, self.root / f'failed-{n}', {'process':process})
                recorded = e.load(self.root / f'failed-{n}/evidence.json')['outcome']
                self.assertEqual(recorded['harness'], process['exit'])
                self.assertEqual(recorded['watchdog'], process['watchdog'])
            incomplete = dict(observation, outcome=dict(observation['outcome'], completed=False))
            (raw / 'observation.json').write_bytes(e.encoded(incomplete))
            with self.assertRaises(e.Invalid):
                pack('parse', 'native', raw, self.root / 'unfinished', {'process':facts['process']})
            self.assertFalse(e.load(self.root / 'unfinished/evidence.json')['outcome']['joined'])
            (raw / 'observation.json').write_bytes(e.encoded(observation))
            c = DRIVER['contract']('parse')
            c['uses_tapes'] = True
            with mock.patch.dict(pack.__globals__, contract=lambda _: c):
                with self.assertRaisesRegex(e.Invalid, 'missing tape accounting'):
                    pack('parse', 'native', raw, self.root / 'no-tape',
                         dict(process=facts['process'], driver_transcript=transcript))
        # Validation also detects a success outcome stamped over a failed process.
        forged = self.root / 'stamped-success'
        shutil.copytree(source, forged)
        record = e.load(forged / 'evidence.json')
        record['provenance']['process']['exit'] = 9
        (forged / 'evidence.json').write_bytes(e.encoded(record))
        with self.assertRaisesRegex(e.Invalid, 'outcome differs from observations'):
            e.validate(forged, DRIVER['contract']('parse'))
        taped = dict(DRIVER['contract']('parse'), uses_tapes=True)
        for report in (None, dict(active=False, unused=[], failures=[])):
            with self.subTest(report=report), self.assertRaisesRegex(e.Invalid, 'missing tape accounting'):
                e.observed_outcome(taped, observation, facts['process'], dict(transcript, hooksReport=report))
        c = DRIVER['contract']('parse')
        outcome = e.observed_outcome(c, observation, facts['process'], transcript)
        self.assertEqual(outcome, c['outcome'])
        for field in ('unused', 'failures'):
            tr = dict(transcript, hooksReport=dict(unused=[], failures=[]))
            tr['hooksReport'][field] = ['unconsumed-or-unmatched']
            observed = e.observed_outcome(c, observation, facts['process'], tr)
            self.assertNotEqual(observed, c['outcome'])

    def test_release_source_requires_matching_explicit_provenance(self):
        record = e.load(self.expected / 'evidence.json')
        record['source'] = 'c95b212882c7ba7b1ad91b7981916ed175f1530e'
        (self.expected / 'evidence.json').write_bytes(e.encoded(record))
        with self.assertRaisesRegex(e.Invalid, 'release evidence lacks explicit source provenance'):
            e.validate(self.expected, self.contract)

    def test_registry_pins_golden_and_verification_manifest(self):
        validate = DRIVER['validate_registry']
        ledger = e.load(ROOT / 'tools/differential/registry.json')
        inventory = ledger['evidence_inventory']
        self.assertEqual(len(inventory), 21)
        for path in inventory:
            with self.subTest(path=path):
                altered = json.loads(json.dumps(ledger))
                altered['evidence_inventory'][path] = e.sha(b'tampered')
                with mock.patch.object(e, 'load', return_value=altered):
                    with self.assertRaisesRegex(e.Invalid, 'committed evidence changed'):
                        validate()

    def test_opaque_globs_and_undeclared_json_are_byte_exact(self):
        (self.tree / 'dynamic.json').write_bytes(b'{"a":1,"a":2}')
        (self.tree / 'empty.jsonl').write_bytes(b'')
        (self.tree / 'odd.json').write_bytes(b'not JSON')
        self.contract['normalization']['reports/odd.json'] = {'mode':'bytes', 'rules':[
            {'kind':'text', 'prefix':'not ', 'suffix':'ON', 'value':'JS'}]}
        self.contract['opaque_json'] = ['reports/*.json']
        expected = self.record('opaque-jvm', 'jvm')
        e.compare(expected, self.record('opaque-native'), self.contract)
        (self.tree / 'dynamic.json').write_bytes(b'{"a":1,"a":3}')
        with self.assertRaises(e.Invalid):
            e.compare(expected, self.record('opaque-changed'), self.contract)
        self.contract['normalization']['reports/odd.json'] = {'mode':'causal-streams', 'rules':[]}
        opaque = self.record('glob-opaque')
        e.validate(opaque, self.contract)
        self.contract['opaque_json'] = []
        with self.assertRaises(ValueError):
            self.record('glob-not-opaque')
        # A selected JSON structural mode still rejects malformed JSON.
        self.contract['normalization']['reports/dynamic.json'] = {'mode':'causal-streams', 'rules':[]}
        self.contract['opaque_json'] = []
        (self.tree / 'dynamic.json').write_bytes(b'not JSON')
        with self.assertRaises(ValueError):
            self.record('structured-invalid')

    def test_missing_comparison_path_retains_invalid_error(self):
        with self.assertRaises(e.Invalid):
            e.compare(self.expected, self.root / 'absent', self.contract, self.root / 'missing-mismatch')
        self.assertTrue((self.root / 'missing-mismatch/error.txt').exists())
        self.assertTrue((self.root / 'missing-mismatch/expected/evidence.json').exists())

    def test_round_two_validation_residuals(self):
        source = ROOT / 'fixtures/differential/portable/parse/jvm-v1'
        c = DRIVER['contract']('parse')
        copy = self.root / 'parse-copy'
        shutil.copytree(source, copy)
        record = e.load(copy / 'evidence.json')
        del record['provenance']['driver_transcript']
        (copy / 'evidence.json').write_bytes(e.encoded(record))
        with self.assertRaisesRegex(e.Invalid, 'driver transcript'):
            e.validate(copy, c)
        facts = e.load(source / 'evidence.json')['provenance']
        with self.assertRaisesRegex(e.Invalid, 'malformed JVM transcript'):
            e.observed_outcome(c, {'outcome':{}}, facts['process'], {})
        no_scenario = dict(self.contract)
        del no_scenario['scenario']
        with self.assertRaisesRegex(e.Invalid, 'scenario adapter'):
            e.validate(self.expected, no_scenario)
        self.assertTrue(e.path_glob('reports/a.json', 'reports/*.json'))
        self.assertFalse(e.path_glob('reports/nested/a.json', 'reports/*.json'))
        self.assertTrue(e.path_glob('reports/nested/a.json', 'reports/**/*.json'))
        self.assertTrue(e.path_glob('reports/a.json', 'reports/**/*.json'))

    def test_factory_tripwires_ignore_unrelated_functions(self):
        from registry_support import factory_fingerprints
        for name in ['main.rs','runner.rs']:
            path = self.root/'crates/marathon-cli/src'/name
            path.parent.mkdir(parents=True,exist_ok=True)
            path.write_bytes((ROOT/'crates/marathon-cli/src'/name).read_bytes())
        before = factory_fingerprints(self.root)
        runner = self.root/'crates/marathon-cli/src/runner.rs'
        text = runner.read_text()
        runner.write_text(text+'\nfn unrelated_tripwire_witness() {}\n')
        self.assertEqual(before,factory_fingerprints(self.root))
        runner.write_text(text.replace('ios: Box::new','ios: changed_factory'))
        self.assertNotEqual(before,factory_fingerprints(self.root))

    def test_command_expected_exit_and_closed_stdin(self):
        command = DRIVER['command']
        work = self.root / 'command'
        command([sys.executable, '-c', 'import sys; assert sys.stdin.read() == ""; sys.exit(7)'],
                work, expected_exit=7)
        self.assertEqual(e.load(work / 'process.json')['exit'], 7)
        with self.assertRaises(e.Invalid):
            command([sys.executable, '-c', 'import sys; sys.exit(7)'], self.root / 'unexpected-exit')


class VendorEvidenceTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.source = ROOT/'fixtures/differential/vendors/ios-media/jvm-v2'
        self.contract = e.load(ROOT/'tools/differential/contracts/vendor-ios-media.json')
        self.actual = self.root/'actual'
        shutil.copytree(self.source,self.actual)

    def mutate(self, path, update):
        record = e.load(self.actual/'evidence.json')
        row = next(r for r in record['entries'] if r['path'] == path)
        data = update(e.payload(self.actual,row,record))
        row.update(size=len(data),sha256=e.sha(data))
        e.save_payload(self.actual,row['sha256'],data,record)
        needed = {r['sha256'] for r in record['entries'] if r['kind'] == 'file'}
        for blob in (self.actual/'blobs').iterdir():
            if blob.name not in needed: blob.unlink()
        (self.actual/'evidence.json').write_bytes(e.encoded(record))

    def test_whole_vendor_commands_and_host_flags_reject_rehashed_mutations(self):
        for n,(path,old,new) in enumerate([('host-argv.jsonl',b'--links',b'--no-links'),
                ('host-argv.jsonl',b'--rsh',b'--rshX'),('transcript.jsonl',b'-destination-timeout',b'-destination-timeoutX')]):
            if n:
                shutil.rmtree(self.actual);shutil.copytree(self.source,self.actual)
            def change(data):
                if path == 'host-argv.jsonl':
                    self.assertIn(old,data)
                    return data.replace(old,new,1)
                rows = [e.loads(line) for line in data.splitlines()]
                row = next(r for r in rows if r['event'] == 'exec' and old in base64.b64decode(r['detail']['base64']))
                payload = base64.b64decode(row['detail']['base64']).replace(old,new,1)
                row['detail'].update(base64=base64.b64encode(payload).decode(),size=len(payload),sha256=e.sha(payload))
                return b''.join(json.dumps(r,separators=(',',':')).encode()+b'\n' for r in rows)
            self.mutate(path,change)
            with self.subTest(option=old),self.assertRaises(e.Invalid):
                e.compare(self.source,self.actual,self.contract)

    def test_source_parallel_preparation_keeps_branch_order_and_join(self):
        import vendor_transport as transport
        def swap(data, first, second, move_after=False):
            rows = transport.records(data)
            keys = []
            for needle in (first,second):
                row = next(r for r in rows if r['event'] == 'exec' and needle in transport.payload(r['detail']).decode())
                keys.append((row['connection'],row['channel']))
            groups = [[r for r in rows if (r['connection'],r['channel']) == key] for key in keys]
            replaced, seen = [], set()
            for row in rows:
                key = (row['connection'],row['channel'])
                if move_after:
                    if key != keys[0]:replaced.append(row)
                    if row is groups[1][-1]:replaced.extend(groups[0])
                elif key not in keys:
                    replaced.append(row)
                elif key not in seen:
                    replaced.extend(groups[1-keys.index(key)]);seen.add(key)
            rows = replaced
            for seq,row in enumerate(rows):row['seq'] = seq
            return b''.join(e.encoded(row).replace(b'\n',b'')+b'\n' for row in rows)
        sdk = 'xcrun --sdk iphonesimulator --show-sdk-platform-path'
        health = 'launchctl dumpstate user/foreground/com.apple.SpringBoard'
        for independent in (sdk, "sh -c 'umask 077 && echo "):
            shutil.rmtree(self.actual);shutil.copytree(self.source,self.actual)
            self.mutate('transcript.jsonl',lambda data:swap(data,health,independent,move_after=independent != sdk))
            e.compare(self.source,self.actual,self.contract)
        for first,second in [(health,'xcrun simctl boot '),(sdk,"chmod +x '"),(sdk,'xcrun xcodebuild test-without-building')]:
            shutil.rmtree(self.actual);shutil.copytree(self.source,self.actual)
            self.mutate('transcript.jsonl',lambda data:swap(data,first,second))
            with self.subTest(first=first,second=second),self.assertRaises(e.Invalid):
                e.compare(self.source,self.actual,self.contract)

    def test_vendor_symlink_modes_are_exact_for_the_recorded_host(self):
        record = e.load(self.actual/'evidence.json')
        row = next(r for r in record['entries'] if r['kind'] == 'link')
        row['mode'] = 0o777
        record['provenance']['platform'] = 'Linux-test-x86_64'
        def save():
            (self.actual/'evidence.json').write_bytes(e.encoded(record))
        save()
        e.compare(self.source,self.actual,self.contract)
        for mode in (0o755, 0o776):
            row['mode'] = mode; save()
            with self.subTest(mode=mode),self.assertRaisesRegex(e.Invalid,'host symlink mode'):
                e.compare(self.source,self.actual,self.contract)
        row['mode'] = 0o777
        row['target'] += '-changed'; save()
        with self.assertRaises(e.Invalid):e.compare(self.source,self.actual,self.contract)

    def test_vendor_outcome_is_recomputed_even_with_stamped_success(self):
        record = e.load(self.actual/'evidence.json')
        del record['provenance']['fake_process']
        (self.actual/'evidence.json').write_bytes(e.encoded(record))
        with self.assertRaisesRegex(e.Invalid,'fake process'):
            e.validate(self.actual,self.contract)

    def test_full_stdout_and_real_literal_message_stay_exact(self):
        self.mutate('stdout',lambda data:data.replace(b'Marathon run finished:',b'Changed run finished:',1))
        with self.assertRaises(e.Invalid):e.compare(self.source,self.actual,self.contract)
        import vendor_reports as reports
        v = dict(serial='device:42',safe_serial='device-42',sandbox='/sandbox',port='42')
        raw = b'{"device":{"serialNumber":"device:42","safeSerialNumber$delegate":{}},"test":{"metaProperties":{"serialNumber":"device:42","safeSerialNumber$delegate":"literal"}},"message":"12345678-1234-1234-1234-123456789abc"}'
        data = reports.omit_delegate(raw,'/device','jvm')
        data = e.loads(reports.json_tokens(data,v,'report'))
        self.assertEqual(data['device']['serialNumber'],'{serial}')
        self.assertEqual(data['test']['metaProperties']['serialNumber'],'device:42')
        self.assertEqual(data['test']['metaProperties']['safeSerialNumber$delegate'],'literal')
        self.assertEqual(data['message'],'12345678-1234-1234-1234-123456789abc')

    def test_real_gif_encoders_compare_exact_decoded_frames_and_timing(self):
        import gif_frames
        c = e.load(ROOT/'tools/differential/contracts/vendor-ios-screenshot.json')
        values = []
        for implementation in ['jvm-a','native']:
            directory = ROOT/'fixtures/differential/vendor-verification/ios-screenshot'/implementation
            record = e.validate(directory,c)
            row = next(r for r in record['entries'] if r['path'].endswith('.gif'))
            data = e.payload(directory,row,record)
            values.append(gif_frames.decode(data))
        self.assertEqual(values[0],values[1])
        changed = bytearray(data);start=data.index(b'!\xf9\x04');changed[start+4] += 1
        self.assertNotEqual(values[0],gif_frames.decode(bytes(changed)))
        with self.assertRaises(e.Invalid):gif_frames.decode(data[:-1])

    def test_console_only_reorders_declared_concurrent_blocks(self):
        import vendor_console as console
        v = dict(sandbox='/sandbox',serial='serial',safe_serial='safe',port='42')
        prefix = 'D 00:00:00.000 [worker] <c.m.marathon.apple.ArtifactProcessor> '
        admission = prefix+'Artifact admission for serial, batch x took 0 ms\n'
        waiting = prefix+'Artifact queue wait for serial, batch x: 0 ms\n'
        self.assertEqual(console.normalize((admission+waiting).encode(),v,'native','stdout'),
                         console.normalize((waiting+admission).encode(),v,'native','stdout'))
        timing = 'I 00:00:00.000 [worker] <Timings> Device provider initialized in 0 ms (+0 ms since start)\n'
        update = 'D 00:00:00.000 [worker] <AppleSimulatorProvider> Processing updates from Ssh(addr=127.0.0.1, port=42):\n- Connected(target=Simulator(udid=fixture))\n'
        self.assertEqual(console.normalize((timing+update).encode(),v,'native','stdout'),
                         console.normalize((update+timing).encode(),v,'native','stdout'))
        self.assertNotEqual(console.normalize(b'first\nsecond\n',v,'native','stdout'),
                            console.normalize(b'second\nfirst\n',v,'native','stdout'))


if __name__ == '__main__':
    unittest.main()
