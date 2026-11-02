"""Synthetic captures test the release adapter; none are real JVM evidence."""
import contextlib
import copy
import json
import os
from pathlib import Path
import runpy
import struct
import subprocess
import sys
import tempfile
import unittest
import zlib
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
import common as c
import outcomes as o
import projection as wt
from projection import failure, ordered, xc_projection


def chunk(kind, body):
    return struct.pack('>I', len(body)) + kind + body + struct.pack('>I', zlib.crc32(kind + body))


def png(width=4, height=6, fill=b'\x10', colour=6, depth=8, interlace=0, raw=None):
    """A valid PNG whose IDAT inflates to exactly what its header implies."""
    samples = {0: 1, 2: 3, 3: 1, 4: 2, 6: 4}[colour]
    if raw is None:
        raw = (b'\x00' + fill * ((width * samples * depth + 7) // 8)) * height
    header = struct.pack('>IIBBBBB', width, height, depth, colour, 0, 0, interlace)
    palette = chunk(b'PLTE', b'\x00\x00\x00' * 2) if colour == 3 else b''
    return (b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', header) + palette + chunk(b'IDAT', zlib.compress(raw))
            + chunk(b'IEND', b''))


def ec(classes, sessions=1):
    """A JaCoCo exec stream: header, session-info blocks, execution-data blocks."""
    def utf(text):
        return struct.pack('>H', len(text.encode())) + text.encode()
    def varint(n):
        out = b''
        while True:
            out += bytes([n & 0x7f | (0x80 if n > 0x7f else 0)])
            n >>= 7
            if not n:
                return out
    data = b'\x01\xc0\xc0\x10\x07'
    for i in range(sessions):
        data += b'\x10' + utf('session-%d' % i) + struct.pack('>qq', 1700000000000 + i, 1700000001000 + i)
    for i, (name, probes) in enumerate(classes):
        packed = bytearray((len(probes) + 7) // 8)
        for j, hit in enumerate(probes):
            packed[j // 8] |= hit << (j % 8)
        data += b'\x11' + struct.pack('>q', i) + utf(name) + varint(len(probes)) + bytes(packed)
    return data


class Outcomes(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.a, self.b = self.root / 'pair-a', self.root / 'pair-b'
        self.policy = dict(format=o.FORMAT, review='synthetic-review', rules=[], deviations=[])
        self.draft = self.root / 'draft.json'; c.save(self.draft, self.policy)
        self.qualified = self.root / 'qualified.json'
        # Read the synthetic bundle through the same reconstruction/command seam.
        def inspector(argv):
            self.assertEqual(argv[:3], ['xcrun', 'xcresulttool', 'get'])
            bundle = Path(argv[argv.index('--path') + 1])
            data = c.e.load(bundle / 'Info.plist')
            data['actions'] = dict(_type={'_name': 'Array'}, _values=data['actions'])
            return c.e.encoded(data)
        self.register_legacy = True
        self.legacy = set()   # evidence digests of the synthetic "retained 7.2" captures (source=None captures)
        legacy = patch.object(o, 'legacy_digests', side_effect=lambda: frozenset(self.legacy))
        legacy.start(); self.addCleanup(legacy.stop)
        self.inspector = patch.object(o, 'command', side_effect=inspector)
        self.inspector.start(); self.addCleanup(self.inspector.stop)

    def build_record(self, source):
        """The retained in/build.json: a 7.2 record (no WP 7.5 field) when `source` is None."""
        if source is None:
            return dict(source=c.FROZEN, products=['p'], commands=['c'])
        record = dict(source=source, source_kind=c.source_kind(source), products=['p'], commands=['c'])
        if source != c.FROZEN:
            record['samples'] = dict(origin='kotlin', paths={'sample/ios-app': 'a' * 40}, frozen={'sample/ios-app': 'a' * 40}, matches_frozen=True)
        return record

    def input_tree(self, raw, source, record=None):
        """Writes in/build.json and returns the inventory rows a real capture hashes."""
        (raw / 'in').mkdir(exist_ok=True)
        data = c.e.encoded(record or self.build_record(source))
        (raw / 'in/build.json').write_bytes(data)
        return [dict(path='tree', kind='directory', mode=0o700),
                dict(path='tree/build.json', kind='file', mode=0o644, size=len(data), sha256=c.e.sha(data))]

    def capture(self, pair, side, n, mutate=None, origin=None, source=None, bound=True, strip=False, build=None):
        raw = pair / side; raw.mkdir(parents=True)
        # `origin` is where the CLI ran when the evidence was captured on another machine.
        ran = Path(origin) / side if origin else raw
        tests = [dict(package='pkg', **{'class': 'SampleTests'}, method=name, metaProperties=[],
                      deviceSerial='sim-' + str(n), status=status, ignored=status == 'IGNORED', success=status == 'PASSED',
                      timestamp=1700000000000 + n, duration=100 + n)
                 for name, status in [('testPassing', 'PASSED'), ('testFailing', 'FAILURE'), ('testSkipping', 'IGNORED')]]
        if n % 2:
            tests.reverse()
        trace = 'AssertionError: expected failure\nfile:///synthetic/checkout/SampleTests.swift#StartingLineNumber=23&EndingLineNumber=23'
        counts = dict(testsCount=3, testsFailedCount=1, testsSkippedCount=1)
        xc = dict(metrics=counts, issues={}, actions=[dict(actionResult=dict(metrics=counts, issues={}))])
        reports = {'test_result/raw.json': json.dumps(tests).encode(),
                   'device-files/omni/sim/xcresult/batch.xcresult/Info.plist': c.e.encoded(xc),
                   'device-files/omni/sim/pulled.txt': b'exact file sync bytes'}
        junit = []
        for i, test in enumerate(tests):
            method, status = test['method'], test['status']
            log = f'logs/omni/sim/SampleTests#{method}-{n}.log'
            reports[log] = f'log line {n}\nother line\n'.encode() if n % 2 else f'other line\nlog line {n}\n'.encode()
            detail = dict(test=dict(pkg='pkg', clazz='SampleTests', method=method), status=status,
                          startTime=n*100, endTime=n*100+1, testBatchId='batch',
                          attachments=[dict(file=str(ran / 'in/report' / log), type='LOG', name='xcodebuild-log')])
            if status == 'FAILURE':
                detail['stacktrace'] = trace
            reports[f'test_result/omni/sim/{method}.json'] = c.e.encoded(detail)
            uuid = f'{n:08x}-0000-4000-8000-{i:012x}'
            allure = dict(uuid=uuid, fullName='pkg.SampleTests.'+method, name=method,
                          labels=[dict(name='testClass', value='pkg.SampleTests'), dict(name='testMethod', value=method)],
                          status={'PASSED':'passed','FAILURE':'failed','IGNORED':'skipped'}[status],
                          attachments=[dict(source=str(ran / 'in/report' / log), type='text/plain')])
            if status == 'FAILURE':
                allure['statusDetails'] = dict(message='expected failure')
            reports[f'allure-results/{uuid}-result.json'] = c.e.encoded(allure)
            html = dict(package_name='pkg', class_name='SampleTests', name=method, status=allure['status'],
                        duration_millis=n, stacktrace=detail.get('stacktrace'))
            reports[f'html/{method}.html'] = b'window.test = ' + c.e.encoded(html).strip() + b';\n'
            child = '<failure type="AssertionError" message="expected failure">'+trace.replace('&','&amp;')+'</failure>' if status=='FAILURE' else '<skipped/>' if status=='IGNORED' else ''
            junit.append(f'<testcase classname="pkg.SampleTests" name="{method}" time="{n}">{child}</testcase>')
        reports['tests/omni/junit.xml'] = ('<testsuite tests="3" failures="1" skipped="1" timestamp="'+str(n)+'">'+''.join(junit)+'</testsuite>').encode()
        state = dict(reports=reports, stdout=b'ignored diagnostic noise ' + str(n).encode() + b'\nMarathon run finished:\nDevice pool omni:\n\t1 passed, 1 failed, 1 ignored tests\nTotal time: ' + str(n + 1).encode() + b's\n',
                     stderr=b'Marathon execution failed\n', exit=1)
        if mutate:
            mutate(state)
        report_dir = raw / 'report'; report_dir.mkdir()
        for name, data in state['reports'].items():
            path = report_dir / name; path.parent.mkdir(parents=True, exist_ok=True); path.write_bytes(data)
        process = dict(argv=['/synthetic/marathon'], cwd=str(ran / 'in'), exit=state['exit'], watchdog=False, joined=True, interrupted=False)
        capture = dict(completed=True, cleanup={'complete': True}, process=process, run_id=str(n),
                       scenario='ios-unit-nm', profile='local-27', worker_id='synthetic', input_tree=self.input_tree(raw, source, build),
                       selected_tests=['pkg.SampleTests#' + t for t in ('testPassing', 'testFailing', 'testSkipping')],
                       worker_before={'before': {'xcode': 'synthetic'}})
        if source and not strip:
            # WP 7.5 captures carry the source; 7.2 ones (source=None here) predate the fields and are frozen.
            # `bound` False drops the JVM reference, a string names another source for it.
            capture.update(source=source, source_kind=c.source_kind(source),
                           jvm_reference_source=bound if isinstance(bound, str) else source if bound else None)
        c.save(raw / 'capture.json', capture)
        outcome = dict(cli=state['exit'], fake=None, harness=0, watchdog=False, joined=True, unused=[], unmatched=[])
        contract = dict(format='marathon-differential/contract@1', scenario='artifact', roles={}, paths=['reports', 'stdout', 'stderr'], normalization={}, outcome=outcome)
        if source and source != c.FROZEN and side == 'jvm':
            contract['source'] = source
        if side == 'jvm':
            c.save(pair / 'contract.json', contract)
        else:
            contract = c.e.load(pair / 'contract.json')
        provenance = dict(terminal_observation=outcome, process=process, capture_sha256=c.e.sha((raw / 'capture.json').read_bytes()))
        if source and source != c.FROZEN:
            provenance['source_sha'] = source
        if side == 'jvm':
            provenance.update(jars={'synthetic.jar': 'a' * 64}, patches={'synthetic.patch': 'b' * 64}, jdk='synthetic JDK')
        c.e.write_record(raw / 'evidence', contract, side, {'reports': report_dir}, {k: state[k] for k in ('stdout', 'stderr')}, outcome, provenance, compressed=True)
        digest = c.e.sha((raw / 'evidence/evidence.json').read_bytes())
        if source is None and self.register_legacy:
            self.legacy.add(digest)
        c.save(raw / 'witnesses.json', dict(review='synthetic-witness-review', evidence_sha256=digest,
               witnesses={w: dict(observation='synthetic test only', paths=[o.RAW]) for w in c.SCENARIOS['ios-unit-nm']['witnesses']}))

    def qualify(self):
        self.capture(self.a, 'jvm', 1)
        self.capture(self.b, 'jvm', 2)
        o.qualify(self.a, self.b, self.draft, self.qualified, self.root / 'jvm-mismatch')

    def release_pair_and_policy(self, source):
        """Two JVM captures of `source` qualified into a policy, and a pair of the same source to release."""
        self.capture(self.a, 'jvm', 1, source=source)
        self.capture(self.b, 'jvm', 2, source=source)
        o.qualify(self.a, self.b, self.draft, self.qualified, self.root / 'jvm-mismatch')

    def release_compare(self, pair):
        o.release(pair, self.qualified, self.root / ('mismatch-' + pair.name))

    def test_a_release_source_qualifies_and_compares_end_to_end(self):
        self.release_pair_and_policy(c.release_sha())
        pair = self.root / 'pair-c'
        self.capture(pair, 'jvm', 1, source=c.release_sha())
        self.capture(pair, 'native', 3, source=c.release_sha())
        self.release_compare(pair)
        self.assertEqual(c.e.load(pair / 'jvm/evidence/evidence.json')['source'], c.release_sha())
        self.assertEqual(c.e.load(pair / 'native/evidence/evidence.json')['provenance']['source_sha'], c.release_sha())

    def test_a_policy_qualified_on_the_frozen_source_cannot_release_a_release_pair(self):
        self.release_pair_and_policy(None)  # 7.2-style captures, no source fields: frozen
        pair = self.root / 'pair-c'
        self.capture(pair, 'jvm', 1, source=c.release_sha())
        self.capture(pair, 'native', 3, source=c.release_sha())
        with self.assertRaisesRegex(c.e.Invalid, 'Kotlin source'):
            self.release_compare(pair)

    def test_a_policy_qualified_on_the_release_cannot_release_a_frozen_pair(self):
        self.release_pair_and_policy(c.release_sha())
        pair = self.root / 'pair-c'
        self.capture(pair, 'jvm', 1)
        self.capture(pair, 'native', 3)
        with self.assertRaisesRegex(c.e.Invalid, 'Kotlin source'):
            self.release_compare(pair)

    def test_new_frozen_captures_qualify_and_release_among_themselves_but_not_with_7_2_ones(self):
        self.release_pair_and_policy(c.FROZEN)
        pair = self.root / 'pair-c'
        self.capture(pair, 'jvm', 1, source=c.FROZEN)
        self.capture(pair, 'native', 3, source=c.FROZEN)
        self.release_compare(pair)
        # a 7.2 capture (no WP 7.5 field anywhere) is a different input tree than a new frozen one
        legacy = self.root / 'pair-legacy'
        self.capture(legacy, 'jvm', 1)
        self.capture(legacy, 'native', 3)
        with self.assertRaisesRegex(c.e.Invalid, 'input_tree'):
            self.release_compare(legacy)

    def test_qualification_rejects_jvm_captures_of_different_sources(self):
        self.capture(self.a, 'jvm', 1, source=c.release_sha())
        self.capture(self.b, 'jvm', 2)
        with self.assertRaisesRegex(c.e.Invalid, 'Kotlin source'):
            o.qualify(self.a, self.b, self.draft, self.qualified, self.root / 'mismatch')

    def test_a_native_capture_of_another_source_than_its_jvm_pair_is_rejected(self):
        self.release_pair_and_policy(c.release_sha())
        for name, source in (('frozen', c.FROZEN), ('legacy', None)):
            pair = self.root / ('pair-' + name)
            self.capture(pair, 'jvm', 1, source=c.release_sha())
            self.capture(pair, 'native', 3, source=source)
            # the contract frozen by the JVM side names the release; the native record and capture name the other one
            with self.assertRaisesRegex(c.e.Invalid, 'different Kotlin sources'):
                self.release_compare(pair)

    def test_a_capture_record_and_contract_must_name_the_same_source(self):
        self.release_pair_and_policy(c.release_sha())
        pair = self.root / 'pair-c'
        self.capture(pair, 'jvm', 1, source=c.release_sha())
        self.capture(pair, 'native', 3, source=c.release_sha())
        for side in ('jvm', 'native'):
            path = pair / side / 'capture.json'
            original = path.read_bytes()
            capture = c.e.load(path); capture['source'] = c.FROZEN; c.save(path, capture)
            with self.assertRaises(c.e.Invalid):  # also fails the evidence digest binding, whichever check fires first
                o.load_capture(pair, side)
            path.write_bytes(original)
            o.load_capture(pair, side)

    def test_a_frozen_capture_cannot_contradict_its_own_jvm_reference(self):
        # R1-sol-2: the binding used to be skipped for every frozen capture
        for bound in (c.release_sha(), False):
            pair = self.root / ('pair-' + str(bool(bound)))
            self.capture(pair, 'jvm', 1, source=c.FROZEN, bound=bound)
            with self.assertRaisesRegex(c.e.Invalid, 'not bound'):
                o.load_capture(pair, 'jvm')

    def test_a_new_capture_cannot_pass_as_a_7_2_capture_by_dropping_its_fields(self):
        pair = self.root / 'pair-c'
        self.capture(pair, 'jvm', 1, source=c.FROZEN, strip=True)  # new build record, capture without WP 7.5 fields
        with self.assertRaisesRegex(c.e.Invalid, 'retained 7.2 captures'):
            o.load_capture(pair, 'jvm')
        legacy = self.root / 'pair-legacy'
        self.capture(legacy, 'jvm', 1, build=dict(source=c.FROZEN, source_kind='frozen', products=['p'], commands=['c']))
        with self.assertRaisesRegex(c.e.Invalid, 'complete 7.2 capture'):
            o.load_capture(legacy, 'jvm')
        self.capture(self.root / 'pair-ok', 'jvm', 1)
        o.load_capture(self.root / 'pair-ok', 'jvm')  # a genuine 7.2-shaped capture still reads

    def test_release_labels_over_a_frozen_retained_build_record_are_rejected(self):
        # R1-sol-3: capture, contract and envelope all said release while in/build.json and the inventory said frozen
        for name, build in (('frozen', self.build_record(c.FROZEN)), ('legacy', self.build_record(None))):
            pair = self.root / ('pair-' + name)
            self.capture(pair, 'jvm', 1, source=c.release_sha(), build=build)
            with self.assertRaisesRegex(c.e.Invalid, 'retained build record is bound to another'):
                o.load_capture(pair, 'jvm')
        self.capture(self.a, 'jvm', 1, source=c.release_sha(), build=self.build_record(c.FROZEN))
        self.capture(self.b, 'jvm', 2, source=c.release_sha(), build=self.build_record(c.FROZEN))
        with self.assertRaisesRegex(c.e.Invalid, 'retained build record'):
            o.qualify(self.a, self.b, self.draft, self.qualified, self.root / 'mismatch')

    def test_the_retained_build_record_must_be_the_one_in_the_input_inventory(self):
        pair = self.root / 'pair-c'
        self.capture(pair, 'jvm', 1, source=c.release_sha())
        o.load_capture(pair, 'jvm')
        path = pair / 'jvm/in/build.json'
        original = path.read_bytes()
        record = c.e.load(path); record['source'] = c.FROZEN; record['source_kind'] = 'frozen'; c.save(path, record)
        with self.assertRaisesRegex(c.e.Invalid, 'differs from the input inventory'):
            o.load_capture(pair, 'jvm')
        path.unlink()
        with self.assertRaisesRegex(c.e.Invalid, 'retains no build record'):
            o.load_capture(pair, 'jvm')
        path.write_bytes(original)
        o.load_capture(pair, 'jvm')

    def test_a_fresh_capture_with_every_new_field_stripped_is_not_a_retained_7_2_capture(self):
        # R2-sol-2: dropping the capture fields and the build's source_kind and refreshing the digests used to qualify
        # and release; field absence alone grants nothing, only the audited evidence digests do.
        self.register_legacy = False
        self.capture(self.a, 'jvm', 1); self.capture(self.b, 'jvm', 2)
        with self.assertRaisesRegex(c.e.Invalid, 'retained 7.2 captures'):
            o.qualify(self.a, self.b, self.draft, self.qualified, self.root / 'mismatch')
        pair = self.root / 'pair-c'
        self.capture(pair, 'jvm', 1); self.capture(pair, 'native', 3)
        with self.assertRaisesRegex(c.e.Invalid, 'retained 7.2 captures'):
            o.load_capture(pair, 'native')
        # the same bytes become readable only once their digest is on the audited list
        self.legacy.add(c.e.sha((pair / 'jvm/evidence/evidence.json').read_bytes()))
        o.load_capture(pair, 'jvm')

    def test_the_committed_legacy_list_covers_the_retained_7_2_captures(self):
        listing = c.e.load(c.HERE / 'legacy-captures.json')
        self.assertEqual((listing['format'], listing['count']), ('marathon-real-e2e/legacy-captures@1', len(listing['evidence_sha256'])))
        self.assertEqual(listing['evidence_sha256'], sorted(set(listing['evidence_sha256'])))
        old = Path(os.environ.get('MRS_OLD', str(c.HERE / 'runs')))
        if not old.is_dir():
            self.skipTest('the retained 7.2 runs are not on this machine')
        found = [p for p in old.rglob('evidence/evidence.json') if 'source' not in c.e.load(p.parent.parent / 'capture.json')]
        if not found:
            # The sweep worker exports MRS_OLD for the staged selections and policies only; the captures stay on the Mac.
            self.skipTest('no retained 7.2 captures under ' + str(old))
        for path in found:
            self.assertIn(c.e.sha(path.read_bytes()), listing['evidence_sha256'], str(path))
        for rel, side in (('local-27-macos/macos-permissions-pair-1', 'jvm'), ('local-27-macos/macos-permissions-pair-1', 'native'),
                          ('linux-kvm/box/runs/lvs-pair-1', 'jvm')):
            if (old / rel / side / 'capture.json').is_file():
                with patch.object(o, 'legacy_digests', side_effect=lambda: frozenset(listing['evidence_sha256'])):
                    self.assertEqual(o.load_capture(old / rel, side)['record']['source'], c.FROZEN)

    def test_the_legacy_list_check_skips_when_the_staged_starting_points_hold_no_7_2_captures(self):
        # 7.5 sweep: the worker exports MRS_OLD for the staged selections and policies; the captures stay on the Mac
        name = 'Outcomes.test_the_committed_legacy_list_covers_the_retained_7_2_captures'
        (self.root / 'staged-old/local-27-ios26').mkdir(parents=True)
        (self.root / 'staged-old/local-27-ios26/selection.json').write_text('{}')
        for old, expected in ((self.root / 'staged-old', 'skipped=1'), (self.root / 'absent', 'skipped=1')):
            result = subprocess.run([sys.executable, '-I', str(Path(__file__).resolve()), name], capture_output=True, text=True,
                                    env=dict(os.environ, MRS_OLD=str(old)))
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn(expected, result.stderr)

    def test_a_release_capture_without_a_bound_jvm_reference_is_rejected(self):
        pair = self.root / 'pair-c'
        self.capture(pair, 'jvm', 1, source=c.release_sha(), bound=False)
        with self.assertRaisesRegex(c.e.Invalid, 'not bound'):
            o.load_capture(pair, 'jvm')

    def test_jvm_rerun_uuid_names_durations_log_order_and_keyed_order_pass(self):
        self.qualify()
        report = c.e.load(self.root / 'jvm-mismatch/comparison.json')
        self.assertEqual(report['status'], 'passed')
        self.assertEqual(report['differences'], [])
        self.assertTrue(report['raw_differences_informational'])
        self.capture(self.a, 'native', 3)
        argv = ['run', 'compare', str(self.a), '--release', '--policy', str(self.qualified),
                '--mismatch', str(self.root / 'native-mismatch')]
        with patch.object(sys, 'argv', argv):
            runpy.run_path(str(c.HERE / 'run'), run_name='__main__')
        self.assertEqual(c.e.load(self.a / 'release.json')['status'], 'passed')
        report = c.e.load(self.root / 'native-mismatch/comparison.json')
        self.assertEqual(report['status'], 'passed')
        self.assertEqual(report['gif_landmarks_informational']['differences'], [])
        self.assertIn('coverage_hits_informational', report)

    def test_a_release_rejected_after_the_projection_never_keeps_status_passed(self):
        # R1-sol-5: comparison.json used to say "passed" as soon as the projection matched, before the witness
        # and landmark review could reject the release.
        self.qualify()
        def release(name, damage=None, review=None):
            target = self.root / name
            self.capture(target, 'jvm', 1)
            self.capture(target, 'native', 3)
            path = target / 'native/witnesses.json'
            if damage:
                value = c.e.load(path); damage(value); c.save(path, value)
            mismatch = self.root / (name + '-mismatch')
            with patch.object(o, 'review_witnesses', side_effect=review) if review else contextlib.nullcontext():
                with self.assertRaises(Exception) as caught:
                    o.release(target, self.qualified, mismatch)
            report = c.e.load(mismatch / 'comparison.json')
            self.assertEqual(report['status'], 'failed', name)
            self.assertEqual(report['differences'], [])  # the projection itself matched
            self.assertIn(str(caught.exception), report['error'])
            self.assertFalse((target / 'release.json').exists())
            return report
        release('missing-witness', lambda v: v['witnesses'].popitem())
        release('stale-review', lambda v: v.update(evidence_sha256='0' * 64))
        release('empty-witness', lambda v: next(iter(v['witnesses'].values())).update(paths=[]))
        report = release('landmark-order', review=[([dict(owners=[], rule='m', landmarks=['a'])], {}), ([dict(owners=[], rule='m', landmarks=['b'])], {})])
        self.assertIn('ordered media landmarks differ', report['error'])

    def test_real_differences_fail(self):
        self.qualify()
        def change(state, path, transform):
            value = json.loads(state['reports'][path]); transform(value)
            state['reports'][path] = json.dumps(value).encode()
        def raw_change(state, transform):
            change(state, 'test_result/raw.json', transform)
        def replace(state, path, old, new):
            state['reports'][path] = state['reports'][path].replace(old, new)
        mutations = {
            'status': lambda s: raw_change(s, lambda r: r[0].update(status='PASSED')),
            'extra-attempt': lambda s: raw_change(s, lambda r: r.append(r[0])),
            'missing-test': lambda s: raw_change(s, lambda r: r.pop()),
            'failure-message': lambda s: replace(s, 'test_result/omni/sim/testFailing.json', b'expected failure', b'changed failure'),
            'failure-message-space': lambda s: replace(s, 'test_result/omni/sim/testFailing.json', b'expected failure', b'expected failure '),
            'failure-class': lambda s: replace(s, 'test_result/omni/sim/testFailing.json', b'AssertionError', b'OtherError'),
            'source-line': lambda s: replace(s, 'test_result/omni/sim/testFailing.json', b'Number=23', b'Number=24'),
            'source-path': lambda s: replace(s, 'test_result/omni/sim/testFailing.json', b'SampleTests.swift', b'OtherTests.swift'),
            'malformed-junit': lambda s: s['reports'].update({'tests/omni/junit.xml': b'<testsuite broken'}),
            'missing-summary-counts': lambda s: s.update(stdout=s['stdout'].replace(b'1 passed, 1 failed, 1 ignored tests', b'')),
            'missing-report': lambda s: s['reports'].pop('tests/omni/junit.xml'),
            'missing-artifact-kind': lambda s: [s['reports'].pop(p) for p in list(s['reports']) if '/xcresult/' in p],
            'missing-per-test-report': lambda s: s['reports'].pop('html/testPassing.html'),
            'missing-log': lambda s: [s['reports'].pop(p) for p in list(s['reports']) if p.startswith('logs/')],
            'junit-failure-text': lambda s: replace(s, 'tests/omni/junit.xml', b'AssertionError: expected failure', b'AssertionError: changed text'),
            'junit-skip': lambda s: replace(s, 'tests/omni/junit.xml', b'<skipped/>', b'<error message="unexpected"/>'),
            'junit-message': lambda s: replace(s, 'tests/omni/junit.xml', b'expected failure', b'changed failure'),
            'allure-content': lambda s: [replace(s, p, b'"status": "passed"', b'"status": "failed"') for p in s['reports'] if p.startswith('allure-results/')],
            'allure-extra-result': lambda s: s['reports'].update({'allure-results/extra-result.json': next(v for p,v in s['reports'].items() if p.startswith('allure-results/'))}),
            'html-content': lambda s: replace(s, 'html/testFailing.html', b'"status": "failed"', b'"status": "passed"'),
            'sync-content': lambda s: s['reports'].update({'device-files/omni/sim/pulled.txt': b'different file sync bytes'}),
            'summary-count': lambda s: s.update(stdout=s['stdout'].replace(b'1 passed', b'2 passed')),
            'summary-line': lambda s: s.update(stderr=b'Marathon execution finished\n'),
            'summary-stream': lambda s: s.update(stdout=s['stdout'] + s['stderr'], stderr=b''),
            'xcresult-count': lambda s: change(s, 'device-files/omni/sim/xcresult/batch.xcresult/Info.plist', lambda r: r['metrics'].update(testsCount=4)),
            'xcresult-issue': lambda s: change(s, 'device-files/omni/sim/xcresult/batch.xcresult/Info.plist', lambda r: r['issues'].update(testFailureSummaries=[dict(testCaseName='testFailing', message='new issue')])),
        }
        for i, (name, mutate) in enumerate(mutations.items()):
            with self.subTest(name=name):
                target = self.root / name
                self.capture(target, 'jvm', 1)
                self.capture(target, 'native', 3, mutate)
                with self.assertRaises(c.e.Invalid):
                    o.release(target, self.qualified, self.root / (name + '-mismatch'))
                self.assertEqual(c.e.load(self.root / (name + '-mismatch/comparison.json'))['status'], 'failed')

    def test_exit_difference_fails_projection(self):
        self.capture(self.a, 'jvm', 1)
        self.capture(self.b, 'jvm', 2, lambda s: s.update(exit=0))
        with self.assertRaisesRegex(c.e.Invalid, '/cli_exit'):
            o.qualify(self.a, self.b, self.draft, self.qualified, self.root / 'mismatch')
        report = c.e.load(self.root / 'mismatch/comparison.json')
        self.assertIn(dict(field='/cli_exit', expected=1, actual=0), report['differences'])

    def test_jvm_nonvolatile_report_difference_does_not_qualify(self):
        self.capture(self.a, 'jvm', 1)
        self.capture(self.b, 'jvm', 2, lambda s: s['reports'].update({'device-files/omni/sim/pulled.txt': b'wrong'}))
        with self.assertRaisesRegex(c.e.Invalid, 'differences'):
            o.qualify(self.a, self.b, self.draft, self.qualified, self.root / 'mismatch')
        self.assertFalse(self.qualified.exists())

    def test_within_test_attempt_order_is_retained(self):
        self.capture(self.a, 'jvm', 1)
        left = o.load_capture(self.a, 'jvm')
        right = copy.deepcopy(left)
        rows = json.loads(left['files'][o.RAW]['data'])
        attempt = dict(rows[0], status='PASSED', ignored=False, success=True)
        left['files'][o.RAW]['data'] = c.e.encoded(rows + [attempt])
        right['files'][o.RAW]['data'] = c.e.encoded([attempt] + rows)
        with self.assertRaisesRegex(c.e.Invalid, 'attempts'):
            o.compare_projected(left, right, self.policy, self.root / 'mismatch')

    def test_allure_attachment_multiplicity_is_retained(self):
        self.qualify()
        def extra(state):
            path = next(p for p in state['reports'] if p.startswith('allure-results/'))
            value = json.loads(state['reports'][path])
            value['steps'] = [dict(attachments=value['attachments'])]
            state['reports'][path] = c.e.encoded(value)
        self.capture(self.a, 'native', 3, extra)
        with self.assertRaisesRegex(c.e.Invalid, 'attachments'):
            o.release(self.a, self.qualified, self.root / 'mismatch')

    # Decision 7 (WP 7.5): the log of a test that ran under 10 ms depends on whether a device line fell in its window.
    TEST = 'pkg.SampleTests#testPassing'

    def lacking_log(self, method='testPassing', duration=0, other=None):
        """A mutation: the test has no log (test-result, Allure, file) and `duration` ms between startTime and endTime."""
        def mutate(state):
            reports = state['reports']
            path = f'test_result/omni/sim/{method}.json'
            row = json.loads(reports[path])
            row['attachments'] = [a for a in row['attachments'] if a['type'] != 'LOG'] + list(other or [])
            row['endTime'] = row['startTime'] + duration
            reports[path] = c.e.encoded(row)
            for name in [n for n in reports if n.startswith('allure-results/')]:
                value = json.loads(reports[name])
                if {r['name']: r['value'] for r in value['labels']}['testMethod'] == method:
                    value['attachments'] = [a for a in value['attachments'] if a['type'] != 'text/plain']
                    reports[name] = c.e.encoded(value)
            for name in [n for n in reports if n.startswith('logs/') and f'#{method}-' in n]:
                del reports[name]
        return mutate

    def log_pair(self, mutate):
        self.qualify()
        pair = self.root / 'pair-d7'
        self.capture(pair, 'jvm', 1)
        self.capture(pair, 'native', 3, mutate)
        return pair

    def comparison(self, pair):
        return c.e.load(self.root / ('mismatch-' + pair.name) / 'comparison.json')

    def test_a_log_missing_from_a_zero_millisecond_test_passes_and_is_listed(self):
        pair = self.log_pair(self.lacking_log(duration=0))
        self.release_compare(pair)
        report = self.comparison(pair)
        self.assertEqual(report['status'], 'passed')
        self.assertEqual(report['differences'], [])
        self.assertEqual([row['test'] for row in report['log_attachment_presence_informational']], [self.TEST])
        row = report['log_attachment_presence_informational'][0]
        self.assertEqual(row['expected']['log_kinds'], ['attachment:LOG', 'allure-attachment:text/plain'])
        self.assertEqual((row['actual']['log_kinds'], row['actual']['attempts']), ([], [[0, False]]))

    def test_a_log_missing_from_a_jvm_side_test_under_ten_milliseconds_passes_too(self):
        # The rule is about the side that lacks the log, whichever it is (the JVM captures of linux-kvm had logs, native none).
        self.qualify()
        pair = self.root / 'pair-d7'
        self.capture(pair, 'jvm', 1, self.lacking_log(duration=9))
        self.capture(pair, 'native', 3)
        self.release_compare(pair)
        self.assertEqual(self.comparison(pair)['log_attachment_presence_informational'][0]['expected']['attempts'], [[9, False]])

    def test_a_log_missing_from_a_test_that_ran_ten_milliseconds_fails(self):
        # The iOS scenarios also require the log per test; relax that, so the comparison itself is what rejects it.
        options = c.SCENARIOS['ios-unit-nm']['projection']
        with patch.dict(options, per_test_kinds=[k for k in options['per_test_kinds'] if k != 'attachment:LOG']):
            for duration in (10, 11, 500):
                with self.subTest(duration=duration):
                    if not self.qualified.exists():
                        self.qualify()
                    pair = self.root / f'pair-{duration}'
                    self.capture(pair, 'jvm', 1)
                    self.capture(pair, 'native', 3, self.lacking_log(duration=duration))
                    with self.assertRaisesRegex(c.e.Invalid, 'per_test_kinds/pkg.SampleTests#testPassing'):
                        self.release_compare(pair)
                    report = self.comparison(pair)
                    self.assertEqual(report['status'], 'failed')
                    self.assertNotIn('log_attachment_presence_informational', report)
                    fields = {d['field'] for d in report['differences']}
                    self.assertIn('/per_test_kinds/' + self.TEST, fields)
                    self.assertIn('/allure/%d/attachments/text~1plain' % next(
                        i for i, r in enumerate(report['expected']['allure']) if r['test'] == self.TEST), fields)

    def test_the_log_requirement_of_a_scenario_is_waived_only_for_a_test_under_ten_milliseconds(self):
        self.qualify()
        for duration, fails in ((0, False), (9, False), (10, True)):
            with self.subTest(duration=duration):
                pair = self.root / f'pair-{duration}'
                self.capture(pair, 'jvm', 1)
                self.capture(pair, 'native', 3, self.lacking_log(duration=duration))
                if fails:
                    with self.assertRaisesRegex(c.e.Invalid, 'missing per-test artifact kinds: pkg.SampleTests#testPassing: attachment:LOG'):
                        self.release_compare(pair)
                else:
                    self.release_compare(pair)

    def test_a_missing_attachment_of_another_kind_still_fails_for_a_near_instant_test(self):
        # PROFILING is carried by the test-result JSON only here; its presence is compared with the log's out of the way.
        def profiled(n, keep):
            def mutate(state):
                path = 'test_result/omni/sim/testPassing.json'
                row = json.loads(state['reports'][path])
                if keep:
                    state['reports']['profiling/p.trace'] = b'trace'
                    row['attachments'].append(dict(file=row['attachments'][0]['file'].replace('logs/omni/sim/SampleTests#testPassing-%d.log' % n, 'profiling/p.trace'),
                                                   type='PROFILING', name='trace'))
                state['reports'][path] = c.e.encoded(row)
            return mutate
        self.capture(self.a, 'jvm', 1, profiled(1, True))
        self.capture(self.b, 'jvm', 2, profiled(2, True))
        o.qualify(self.a, self.b, self.draft, self.qualified, self.root / 'jvm-mismatch')
        pair = self.root / 'pair-d7'
        self.capture(pair, 'jvm', 1, profiled(1, True))
        lacking, native = self.lacking_log(duration=0), profiled(3, False)
        self.capture(pair, 'native', 3, lambda state: (native(state), lacking(state)))
        with self.assertRaises(c.e.Invalid):
            self.release_compare(pair)
        report = self.comparison(pair)
        self.assertIn('/per_test_kinds/' + self.TEST, {d['field'] for d in report['differences']})
        row = next(d for d in report['differences'] if d['field'] == '/per_test_kinds/' + self.TEST)
        self.assertIn('attachment:PROFILING', row['expected'])
        self.assertNotIn('attachment:PROFILING', row['actual'])
        self.assertNotIn('attachment:LOG', row['expected'])   # only the log facts were set aside

    def test_a_present_log_still_gates_and_a_waived_neighbour_hides_nothing_else(self):
        self.qualify()
        lacking = self.lacking_log(duration=0)
        # another test loses its log while it ran 100 ms: not waived
        both = lambda state: (lacking(state), self.lacking_log('testFailing', duration=100)(state))
        pair = self.root / 'pair-other'
        self.capture(pair, 'jvm', 1)
        self.capture(pair, 'native', 3, both)
        with self.assertRaisesRegex(c.e.Invalid, 'missing per-test artifact kinds: pkg.SampleTests#testFailing'):
            self.release_compare(pair)
        # the waived test's log reference must still resolve when it is made: a dangling reference is not an absent log
        pair = self.root / 'pair-dangling'
        self.capture(pair, 'jvm', 1)
        self.capture(pair, 'native', 3, lambda state: [state['reports'].pop(p) for p in list(state['reports']) if p.startswith('logs/')])
        with self.assertRaisesRegex(c.e.Invalid, 'missing attachment'):
            self.release_compare(pair)
        # a present log of a near-instant test is kept as it was: its Allure type is still compared
        def retype(state):
            for name in [n for n in state['reports'] if n.startswith('allure-results/')]:
                value = json.loads(state['reports'][name])
                value['attachments'] = [dict(a, type='text/x-other') for a in value['attachments']]
                state['reports'][name] = c.e.encoded(value)
        pair = self.root / 'pair-retyped'
        self.capture(pair, 'jvm', 1)
        self.capture(pair, 'native', 3, lambda state: (self.lacking_log('testFailing', duration=0)(state), retype(state)))
        with self.assertRaises(c.e.Invalid):
            self.release_compare(pair)
        self.assertTrue(self.comparison(pair)['differences'])

    def with_retry(self, method='testPassing', raw=True, allure=True):
        """A mutation: an earlier failed attempt in raw.json and/or the Allure results. The detail reporter keeps only the
        final result of a retried test, so the single detail row stays as it is (the shape of a real Kotlin retry)."""
        def mutate(state):
            reports = state['reports']
            if raw:
                rows = json.loads(reports[o.RAW.removeprefix('reports/')])
                first = next(r for r in rows if r['method'] == method)
                rows.append(dict(first, status='FAILURE', ignored=False, success=False))
                reports[o.RAW.removeprefix('reports/')] = c.e.encoded(rows)
            if allure:
                name = next(n for n in reports if n.startswith('allure-results/')
                            and {r['name']: r['value'] for r in json.loads(reports[n])['labels']}['testMethod'] == method)
                value = dict(json.loads(reports[name]), uuid='ffffffff-0000-4000-8000-000000000000', status='failed')
                reports['allure-results/ffffffff-0000-4000-8000-000000000000-result.json'] = c.e.encoded(value)
        return mutate

    def test_a_test_with_a_retry_is_not_waived_although_it_has_one_detail_row(self):
        # R1-sol-1: the detail reporter keeps only final events, so raw.json and Allure show the retries
        options = c.SCENARIOS['ios-unit-nm']['projection']
        for label, relax in (('scenario requires the log', False), ('comparison alone', True)):
            for name, retry in (('raw and allure', self.with_retry()), ('raw only', self.with_retry(allure=False)),
                                ('allure only', self.with_retry(raw=False))):
                with self.subTest(label=label, retry=name):
                    root = self.root / f'{relax}-{name}'.replace(' ', '-')
                    a, b, pair, qualified = root / 'a', root / 'b', root / 'pair', root / 'q.json'
                    draft = root / 'draft.json'; root.mkdir(); c.save(draft, self.policy)
                    both = lambda state: (retry(state), self.lacking_log(duration=0)(state))
                    patched = patch.dict(options, per_test_kinds=[k for k in options['per_test_kinds'] if k != 'attachment:LOG']) if relax else contextlib.nullcontext()
                    with patched:
                        self.capture(a, 'jvm', 1, retry)
                        self.capture(b, 'jvm', 2, retry)
                        o.qualify(a, b, draft, qualified, root / 'jvm-mismatch')
                        self.capture(pair, 'jvm', 1, retry)
                        self.capture(pair, 'native', 3, both)
                        with self.assertRaises(c.e.Invalid):
                            o.release(pair, qualified, root / 'mismatch')
                    report = c.e.load(root / 'mismatch/comparison.json')
                    self.assertEqual(report['status'], 'failed')
                    self.assertNotIn('log_attachment_presence_informational', report)
                    if relax:
                        self.assertIn('/per_test_kinds/' + self.TEST, {d['field'] for d in report['differences']})

    def test_the_same_test_without_a_retry_is_still_waived_in_that_shape(self):
        # the control of the regression above: identical captures and mutations except for the retry
        pair = self.log_pair(self.lacking_log(duration=0))
        self.release_compare(pair)
        self.assertEqual(self.comparison(pair)['log_attachment_presence_informational'][0]['test'], self.TEST)

    def test_a_retried_or_untimed_test_is_never_waived(self):
        self.qualify()
        pair = self.root / 'pair-untimed'
        def untimed(state):
            self.lacking_log(duration=0)(state)
            path = 'test_result/omni/sim/testPassing.json'
            row = json.loads(state['reports'][path]); del row['startTime']; state['reports'][path] = c.e.encoded(row)
        self.capture(pair, 'jvm', 1)
        self.capture(pair, 'native', 3, untimed)
        with self.assertRaisesRegex(c.e.Invalid, 'missing per-test artifact kinds'):
            self.release_compare(pair)
        for attempts in ([[0, False], [0, False]], [[None, False]], [[-1, False]], [[10, False]], [[0, True]]):
            self.assertFalse(wt.instant_without_log(attempts), attempts)
        self.assertTrue(wt.instant_without_log([[0, False]]))
        self.assertTrue(wt.instant_without_log([[9, False]]))

    def test_native_exit_difference_fails_evidence_validation(self):
        self.capture(self.a, 'jvm', 1)
        with self.assertRaisesRegex(c.e.Invalid, 'terminal outcome'):
            self.capture(self.a, 'native', 3, lambda state: state.update(exit=0))

    def test_independent_jvm_captures_required(self):
        self.capture(self.a, 'jvm', 1)
        self.capture(self.b, 'jvm', 1)
        with self.assertRaisesRegex(c.e.Invalid, 'independent'):
            o.qualify(self.a, self.b, self.draft, self.qualified, self.root / 'mismatch')

    def test_tampered_projection_and_missing_review_fail(self):
        self.qualify(); self.capture(self.a, 'native', 3)
        policy = c.e.load(self.qualified); policy['projection_sha256'] = 'forged'
        bad = self.root / 'bad.json'; c.save(bad, policy)
        with self.assertRaisesRegex(c.e.Invalid, 'projection changed'):
            o.release(self.a, bad, self.root / 'mismatch')
        (self.a / 'native/witnesses.json').unlink()
        with self.assertRaises(OSError):
            o.release(self.a, self.qualified, self.root / 'mismatch')

    def test_deviations_must_name_existing_plan_rows_and_cannot_drop_fields(self):
        policy = copy.deepcopy(self.policy)
        policy['deviations'] = [dict(number='D3', reason='raw-only serializer field')]
        o.policy_shape(policy)
        policy['deviations'][0]['number'] = 'D17'
        with self.assertRaisesRegex(c.e.Invalid, 'PLAN'):
            o.policy_shape(policy)
        policy['deviations'][0] = dict(number='D3', reason='wrong', exclude='/tests')
        with self.assertRaisesRegex(c.e.Invalid, 'annotation'):
            o.policy_shape(policy)
        policy['rules'] = [dict(id='bad', kind='text', reason='erase outcome')]
        policy['deviations'] = []
        with self.assertRaisesRegex(c.e.Invalid, 'only reviewed P3'):
            o.policy_shape(policy)

    def test_media_rule_cannot_bypass_gif_sanity(self):
        policy = copy.deepcopy(self.policy)
        policy['rules'] = [dict(id='gif', kind='media', reason='test', path='reports/*.gif',
                                expected={}, duration_seconds=[1, 2], frame_count=[1, 2], exact=True)]
        with self.assertRaisesRegex(c.e.Invalid, 'unknown P3'):
            o.policy_shape(policy)

    def test_failure_location_retains_line_but_not_checkout_prefix(self):
        a = failure('failed\nfile:///first/src/Test.swift#StartingLineNumber=23')
        b = failure('failed\nfile:///second/src/Test.swift#StartingLineNumber=23')
        self.assertEqual(a, b)
        self.assertNotEqual(a, failure('failed\nfile:///second/src/Test.swift#StartingLineNumber=24'))

    def test_media_sanity_uses_decode_and_jvm_bounds_with_doubles(self):
        files = {'reports/video.mp4': dict(kind='file', data=b'synthetic-video')}
        rule = dict(kind='media', path='reports/video.mp4', expected=dict(codec_name='h264', width=640, height=480), duration_seconds=[1, 5], frame_count=[1, 200])
        probe = c.e.encoded(dict(streams=[dict(codec_type='video', nb_read_frames='90', **rule['expected'])], format={'duration': '3'}))
        with patch.object(o, 'command', side_effect=[probe, b'']) as command:
            o.inspect_artifact(files, rule)
            self.assertEqual(command.call_count, 2)
            self.assertIn('-xerror', command.call_args_list[1].args[0])
        rule['duration_seconds'] = [4, 5]
        with patch.object(o, 'command', side_effect=[probe, b'']):
            with self.assertRaisesRegex(c.e.Invalid, 'duration'):
                o.inspect_artifact(files, rule)

    def test_xcresult_unreadable_fails(self):
        files = {'reports/a.xcresult': dict(kind='directory', mode=0o755),
                 'reports/a.xcresult/Info.plist': dict(kind='file', data=b'synthetic-bundle')}
        with patch.object(o, 'command', side_effect=c.e.Invalid('unreadable')):
            with self.assertRaisesRegex(c.e.Invalid, 'unreadable'):
                o.inspect_artifact(files, dict(kind='xcresult', path='reports/a.xcresult'))

    def test_xcresult_warnings_are_informational_but_failures_errors_and_counts_are_exact(self):
        issue = dict(testCaseName='SampleTests.testFailing()', message='expected failure',
                     documentLocationInCreatingWorkspace={'url': 'file:///src/Test.swift#StartingLineNumber=23'})
        counts = dict(testsCount=3, testsFailedCount=1, testsSkippedCount=1)
        issues = dict(testFailureSummaries=[issue], errorSummaries=[dict(issue, message='tool error')])
        original = dict(root=dict(metrics=counts, issues=issues,
                                 actions=[dict(actionResult=dict(metrics=counts, issues=issues))]), tests=[])
        expected = xc_projection(original)
        noisy = copy.deepcopy(original)
        for node in (noisy['root'], noisy['root']['actions'][0]['actionResult']):
            node['issues']['testWarningSummaries'] = [dict(issue, message='UIScene warning')]
            node['issues']['warningSummaries'] = [dict(issue, message='environment warning')]
        self.assertEqual(expected, xc_projection(noisy))
        for scope in ('root', 'action'):
            for kind in ('testFailureSummaries', 'errorSummaries'):
                for field, value in (('message', 'changed'), ('testCaseName', 'Other.test()'),
                                     ('documentLocationInCreatingWorkspace', {'url': 'file:///src/Test.swift#StartingLineNumber=24'})):
                    with self.subTest(scope=scope, kind=kind, field=field):
                        changed = copy.deepcopy(original)
                        node = changed['root'] if scope == 'root' else changed['root']['actions'][0]['actionResult']
                        node['issues'][kind][0][field] = value
                        self.assertNotEqual(expected, xc_projection(changed))
            for count in counts:
                changed = copy.deepcopy(original)
                node = changed['root'] if scope == 'root' else changed['root']['actions'][0]['actionResult']
                node['metrics'][count] += 1
                self.assertNotEqual(expected, xc_projection(changed))

    def test_xcresult_omitted_zero_counts_equal_explicit_zero_but_test_count_is_required(self):
        # Xcode 26.6 results omit testsSkippedCount when nothing was skipped; Xcode 27 writes 0.
        def result(metrics):
            return dict(root=dict(metrics=metrics, issues={}, actions=[dict(actionResult=dict(metrics=metrics))]), tests=[])
        explicit = xc_projection(result(dict(testsCount=2, testsFailedCount=1, testsSkippedCount=0)))
        self.assertEqual(explicit, xc_projection(result(dict(testsCount=2, testsFailedCount=1))))
        self.assertNotEqual(explicit, xc_projection(result(dict(testsCount=2, testsFailedCount=1, testsSkippedCount=1))))
        with self.assertRaisesRegex(c.e.Invalid, 'test count'):
            xc_projection(result(dict(testsFailedCount=1)))

    def test_row_order_follows_test_identity_not_row_content(self):
        full = dict(test='pkg.A#testA', attachments={'text/plain': 1, 'video/mp4': 1})
        bare = dict(test='pkg.B#testB', attachments={'text/plain': 1})
        # Content-only ordering would put the row with fewer attachments first on one side and last on the other.
        for rows in ([full, bare], [bare, full], [dict(full, attachments={}), bare]):
            self.assertEqual(['pkg.A#testA', 'pkg.B#testB'], [row['test'] for row in ordered(rows)])
        duplicate = dict(bare, attachments={})
        self.assertEqual(3, len(ordered([bare, duplicate, bare])))
        self.assertEqual([['pkg.A#testA'], ['pkg.B#testB']],
                         [row['tests'] for row in ordered([dict(tests=['pkg.B#testB']), dict(tests=['pkg.A#testA'])])])

    def test_attachments_resolve_against_the_capture_machines_report_root(self):
        self.capture(self.a, 'jvm', 1, origin='/worker/runs/pair-a')
        capture = o.load_capture(self.a, 'jvm')
        from projection import semantic_projection
        projected, _ = semantic_projection(capture, self.policy, o.inspect_artifact, o.summary)
        self.assertEqual(len(projected['allure']), 3)

    def test_empty_recordings_are_not_media_but_a_real_file_still_needs_a_rule(self):
        def mutate(state):
            state['reports']['video/omni/sim/pulled-before-written.mp4'] = b''
            state['reports']['screenshot/omni/sim/no-frames.gif'] = b';'
        self.capture(self.a, 'jvm', 1, mutate)
        capture = o.load_capture(self.a, 'jvm')
        from projection import semantic_projection
        projected, _ = semantic_projection(capture, self.policy, o.inspect_artifact, o.summary)
        self.assertEqual(projected['media'], [])
        self.assertNotIn('media', projected['kinds'])
        capture['files']['reports/video/omni/sim/pulled-before-written.mp4']['data'] = b'xy'
        with self.assertRaisesRegex(c.e.Invalid, 'media needs exactly one reviewed P3 rule'):
            semantic_projection(capture, self.policy, o.inspect_artifact, o.summary)

    def test_android_screenshots_project_their_size_not_their_live_pixels(self):
        from projection import screenshot_fact
        self.assertEqual(screenshot_fact('Android', png(32, 64, b'\x10')), screenshot_fact('Android', png(32, 64, b'\x11')))
        self.assertEqual(screenshot_fact('Android', png(32, 64)), dict(width=32, height=64))
        self.assertNotEqual(screenshot_fact('Android', png(32, 65)), screenshot_fact('Android', png(32, 64)))
        self.assertNotEqual(screenshot_fact('iOS', png(1, 1, b'\x01')), screenshot_fact('iOS', png(1, 1, b'\x02')))
        with self.assertRaises(c.e.Invalid):
            screenshot_fact('Android', b'not a png at all, long enough to index')

    def test_screenshot_png_must_decode_completely(self):
        # R1-sol-1: a retained 8,845-byte PNG cut to its first 24 bytes (signature, IHDR header) used to pass.
        from projection import decode_png, screenshot_fact
        good = png(4, 6)
        self.assertEqual(decode_png(good), dict(width=4, height=6))
        self.assertEqual(decode_png(png(3, 3, colour=0, interlace=1, raw=b'\x00' * 15)), dict(width=3, height=3))
        self.assertEqual(decode_png(png(5, 2, colour=3, depth=2)), dict(width=5, height=2))
        self.assertEqual(decode_png(png(2, 5, raw=b''.join(bytes([f]) + b'\x10' * 8 for f in range(5)))), dict(width=2, height=5))
        header_end = 8 + 12 + 13
        flipped = bytearray(good); flipped[header_end + 10] ^= 1
        stream = lambda raw: png(4, 6, raw=raw)
        row = b'\x00' + b'\x10' * 16
        broken = {
            'truncated to 24 bytes': (good[:24], 'truncated'),
            'cut before IEND': (good[:-12], 'truncated'),
            'cut inside IDAT': (good[:header_end + 20], 'truncated'),
            'chunk CRC': (bytes(flipped), 'CRC'),
            'trailing bytes': (good + b'\x00', 'trailing'),
            'one row too few': (stream(row * 5), 'inflates to'),
            'one row too many': (stream(row * 7), 'inflates to'),
            'not zlib': (good[:header_end] + chunk(b'IDAT', b'not zlib') + chunk(b'IEND', b''), 'inflate'),
            'zero width': (png(0, 6, raw=b''), 'dimensions'),
            'bad colour depth': (png(4, 6, colour=2, depth=4, raw=b''), 'colour type'),
            'missing IDAT': (good[:header_end] + chunk(b'IEND', b''), 'IDAT'),
            # R2-sol-2: sizes and CRCs are right, but libpng rejects scanline filter types above 4.
            'first filter byte 5': (stream(b'\x05' + b'\x10' * 16 + row * 5), 'filter type'),
            'last filter byte 255': (stream(row * 5 + b'\xff' + b'\x10' * 16), 'filter type'),
            'interlaced pass filter': (png(3, 3, colour=0, interlace=1, raw=b'\x00' * 9 + b'\x05' + b'\x00' * 5), 'filter type'),
            # R3-sol-1: a CRC-valid but unknown critical chunk is fatal to libpng.
            'unknown critical chunk': (good[:-12] + chunk(b'ABCD', b'x') + good[-12:], 'unknown critical'),
            'palette in greyscale': (png(4, 6, colour=0)[:header_end] + chunk(b'PLTE', b'\x00' * 3) + png(4, 6, colour=0)[header_end:], 'palette'),
        }
        # An unknown ancillary chunk (lower-case first letter) is skipped by decoders and stays acceptable.
        self.assertEqual(decode_png(good[:-12] + chunk(b'abCd', b'x') + good[-12:]), dict(width=4, height=6))
        for name, (data, message) in broken.items():
            for vendor in ('Android', 'iOS'):
                with self.subTest(name, vendor=vendor), self.assertRaisesRegex(c.e.Invalid, message):
                    screenshot_fact(vendor, data)

    def test_truncated_android_screenshot_fails_the_projection_and_a_moved_one_changes_it(self):
        from projection import semantic_projection
        path = 'reports/screenshot/omni/sim/screen.png'
        def attach(owner):
            def mutate(state):
                state['reports'][path[len('reports/'):]] = png(4, 6)
                name = 'test_result/omni/sim/%s.json' % owner
                row = c.e.loads(state['reports'][name])
                row['attachments'].append(dict(file=path, type='SCREENSHOT_PNG', name='screen'))
                state['reports'][name] = c.e.encoded(row)
            return mutate
        self.capture(self.a, 'jvm', 1, attach('testPassing'))
        self.capture(self.b, 'jvm', 2, attach('testFailing'))
        with patch.dict(c.SCENARIOS['ios-unit-nm'], vendor='Android'):
            first = semantic_projection(o.load_capture(self.a, 'jvm'), self.policy, o.inspect_artifact, o.summary)[0]
            moved = semantic_projection(o.load_capture(self.b, 'jvm'), self.policy, o.inspect_artifact, o.summary)[0]
            self.assertEqual(first['screenshots'], [dict(owners=[dict(test='pkg.SampleTests#testPassing', attempt=0, role='SCREENSHOT_PNG', source='test-result')],
                                                         width=4, height=6)])
            self.assertEqual(moved['screenshots'][0]['owners'][0]['test'], 'pkg.SampleTests#testFailing')
            self.assertIn('/screenshots/0/owners/0/test', [d['field'] for d in o.differences(first, moved)])
            capture = o.load_capture(self.a, 'jvm')
            capture['files'][path]['data'] = capture['files'][path]['data'][:24]
            with self.assertRaisesRegex(c.e.Invalid, 'PNG chunk is truncated'):
                semantic_projection(capture, self.policy, o.inspect_artifact, o.summary)

    def test_android_file_sync_ignores_run_uuids_but_not_payloads(self):
        from projection import file_sync_fact
        def allure(uuid, status='passed'):
            return c.e.encoded(dict(uuid=uuid, name='t', fullName='p.T.t', status=status, start=uuid))
        a = file_sync_fact('Android', 'reports/device-files/allure-results/aaaaaaaa-0000-4000-8000-000000000001-result.json', allure('one'))
        b = file_sync_fact('Android', 'reports/device-files/allure-results/bbbbbbbb-0000-4000-8000-000000000002-result.json', allure('two'))
        self.assertEqual(a, b)
        self.assertNotEqual(a, file_sync_fact('Android', 'reports/device-files/allure-results/bbbbbbbb-0000-4000-8000-000000000002-result.json', allure('two', 'failed')))
        cover = 'reports/device-files/omni/coverage/coverage-%s.ec'
        one = file_sync_fact('Android', cover % 'aaaaaaaa-0000-4000-8000-000000000001', ec([('a/A', [1, 0, 1]), ('a/B', [0] * 9)]))
        two = file_sync_fact('Android', cover % 'bbbbbbbb-0000-4000-8000-000000000002', ec([('a/B', [1] * 9), ('a/A', [0, 1, 0])], sessions=2))
        self.assertEqual(one, two)  # which probes fired, and the session metadata, are timing
        self.assertEqual(one['classes'], [['a/A', 3], ['a/B', 9]])
        exact = 'reports/device-files/external-folder/external-folder/external-file-2'
        self.assertEqual(file_sync_fact('Android', exact, b'payload')['role'], 'external-folder/external-folder/external-file-2')
        self.assertNotEqual(file_sync_fact('Android', exact, b'payload'), file_sync_fact('Android', exact, b'payloaD'))
        self.assertEqual(file_sync_fact('iOS', 'reports/device-files/omni/sim/dir/out.txt', b'x')['role'], 'dir/out.txt')

    def test_coverage_exec_must_parse_completely_and_gate_on_classes_and_probe_lengths(self):
        # R1-sol-2: an .ec cut to its 5-byte header used to pass because only the header was projected.
        from projection import coverage_fact, parse_jacoco
        path = 'omni/coverage/coverage-<uuid>.ec'
        full = ec([('a/A', [1, 0, 1]), ('a/B', [0] * 9)])
        self.assertEqual(parse_jacoco(full), (1, [('a/A', 3, 2), ('a/B', 9, 0)]))
        fact = coverage_fact(path, full)
        self.assertNotEqual(fact, coverage_fact(path, full[:5]))
        self.assertNotEqual(fact, coverage_fact(path, ec([('a/A', [1, 0, 1])])))  # a class is missing
        self.assertNotEqual(fact, coverage_fact(path, ec([('a/A', [1, 0, 1, 1]), ('a/B', [0] * 9)])))  # probe length
        self.assertEqual(fact, coverage_fact(path, ec([('a/A', [0, 0, 0]), ('a/B', [1] * 9)])))
        broken = {
            'cut inside a class': (full[:-1], 'truncated'),
            'cut inside the header': (full[:3], 'truncated'),
            'trailing partial block': (full + b'\x11\x00', 'truncated'),
            'trailing garbage': (full + b'garbage', 'unknown block'),
            'not jacoco': (b'not jacoco', 'JaCoCo'),
            'padding bits set': (full[:-1] + b'\xff', 'padding'),
        }
        for name, (data, message) in broken.items():
            with self.subTest(name), self.assertRaisesRegex(c.e.Invalid, message):
                coverage_fact(path, data)

    def test_androidx_class_presence_is_informational_but_other_classes_and_probe_lengths_gate(self):
        # Decision 17: two Kotlin runs differ in the androidx font/emoji classes they load lazily.
        name = 'reports/device-files/omni/coverage/coverage-%s.ec'
        app, lazy = ('app/Main', [1, 0, 1]), ('androidx/emoji2/text/EmojiProcessor', [1] * 5)
        def compared(classes_a, classes_b):
            def mutate(classes):
                return lambda state: state['reports'].update({name[len('reports/'):] % ('a' * 8 + '-0000-4000-8000-' + '0' * 12): ec(classes)})
            pair = self.root / ('cov-%d' % len(list(self.root.glob('cov-*'))))
            self.capture(pair, 'jvm', 1, mutate(classes_a)); self.capture(pair, 'native', 2, mutate(classes_b))
            mismatch = pair / 'mismatch'
            with patch.dict(c.SCENARIOS['ios-unit-nm'], vendor='Android'):
                try:
                    o.compare_projected(o.load_capture(pair, 'jvm'), o.load_capture(pair, 'native'), self.policy, mismatch)
                except c.e.Invalid as error:
                    return str(error), c.e.load(mismatch / 'comparison.json')
            return None, c.e.load(mismatch / 'comparison.json')
        error, report = compared([app, lazy], [app])
        self.assertIsNone(error)
        self.assertEqual(report['status'], 'passed')
        self.assertEqual(report['coverage_androidx_presence_informational'],
                         [dict(role='omni/coverage/coverage-<uuid>.ec', only_expected=[lazy[0]], only_actual=[])])
        error, report = compared([app], [app, lazy])
        self.assertIsNone(error)
        self.assertEqual(report['coverage_androidx_presence_informational'][0]['only_actual'], [lazy[0]])
        error, report = compared([app, lazy], [app, lazy])
        self.assertIsNone(error)
        self.assertNotIn('coverage_androidx_presence_informational', report)
        error, _ = compared([app, lazy], [lazy])  # a non-androidx class missing
        self.assertIn('/file_sync', error)
        # R2-sol-4: a second execution-data block with another probe length for a shared class is not collapsed away.
        error, report = compared([app, lazy], [app, lazy, (lazy[0], [1] * 4)])
        self.assertIn('/androidx/', error + ''.join(d['field'] for d in report['differences']))
        error, report = compared([app, lazy], [app, lazy, (app[0], [1] * 4)])
        self.assertIn('/file_sync/', error)
        error, report = compared([app, lazy, (lazy[0], [1] * 4)], [app, lazy, (lazy[0], [1] * 4)])
        self.assertIsNone(error)
        error, report = compared([app, lazy], [app, (lazy[0], [1] * 6)])  # probe length of a shared androidx class
        self.assertIn('/file_sync/', error)
        self.assertEqual([(d['expected'], d['actual']) for d in report['differences']], [([5], [6])])
        self.assertTrue(report['differences'][0]['field'].endswith('/androidx/' + lazy[0]))

    def test_coverage_keeps_every_probe_length_of_a_class(self):
        from projection import coverage_fact
        path = 'omni/coverage/coverage-<uuid>.ec'
        one = coverage_fact(path, ec([('androidx/lifecycle/Lifecycling', [0] * 83)]))
        both = coverage_fact(path, ec([('androidx/lifecycle/Lifecycling', [0] * 83), ('androidx/lifecycle/Lifecycling', [0] * 82)]))
        self.assertNotEqual(one, both)
        self.assertEqual(both['androidx'], [['androidx/lifecycle/Lifecycling', 82], ['androidx/lifecycle/Lifecycling', 83]])

    def test_coverage_hits_are_reported_but_not_projected(self):
        capture = lambda hits: dict(files={'reports/device-files/omni/coverage/coverage-%s.ec' % ('a' * 8 + '-0000-4000-8000-' + '0' * 12):
                                           dict(kind='file', data=ec([('a/A', hits)]))})
        report = o.coverage_hit_report(o.coverage_hits(capture([1, 0, 1])), o.coverage_hits(capture([1, 1, 1])))
        self.assertEqual(report, {'omni/coverage/coverage-<uuid>.ec': dict(expected_total=2, actual_total=3, differing_classes={'a/A': [2, 3]})})

    def test_screenshot_gif_keeps_per_test_role_and_missing_attachment_fails(self):
        self.capture(self.a, 'jvm', 1)
        capture = o.load_capture(self.a, 'jvm')
        files = capture['files']
        path = 'reports/screenshot/omni/sim/screen.gif'
        files[path] = dict(kind='file', data=b'synthetic-gif')
        detail = 'reports/test_result/omni/sim/testPassing.json'
        row = c.e.loads(files[detail]['data'])
        row['attachments'].append(dict(file=path, type='SCREENSHOT_GIF', name='screen'))
        files[detail]['data'] = c.e.encoded(row)
        policy = dict(self.policy, rules=[dict(id='gif', kind='media', path=path)])
        def inspector(files, rule):
            if rule['kind'] == 'media':
                return c.e.encoded(dict(codec_name='gif', width=640, height=480)), ['hash', 4.5, 9]
            return o.inspect_artifact(files, rule)
        with patch.dict(c.SCENARIOS['ios-unit-nm']['projection'], required_kinds=['screenshot']):
            from projection import semantic_projection
            projected, _ = semantic_projection(capture, policy, inspector, o.summary)
            self.assertIn('screenshot', projected['per_test_kinds']['pkg.SampleTests#testPassing'])
            self.assertNotIn('screenshot', projected['per_test_kinds']['pkg.SampleTests#testFailing'])
            self.assertEqual(projected['screenshots'], [])  # GIF pixels use P3, not a static-image hash.
            del files[path]
            with self.assertRaisesRegex(c.e.Invalid, 'missing attachment'):
                semantic_projection(capture, policy, inspector, o.summary)


    def test_allure_ownership_gates_alongside_test_result_ownership(self):
        # R2-sol-3: rewiring only an Allure attachment to another test used to pass, because Allure ownership was
        # dropped whenever a test-result row also owned the file.
        from projection import attachment_owners, semantic_projection
        path = 'reports/screenshot/omni/sim/screen.png'
        def attach(allure_owner):
            def mutate(state):
                state['reports'][path[len('reports/'):]] = png(4, 6)
                row = c.e.loads(state['reports']['test_result/omni/sim/testPassing.json'])
                row['attachments'].append(dict(file=path, type='SCREENSHOT_PNG', name='screen'))
                state['reports']['test_result/omni/sim/testPassing.json'] = c.e.encoded(row)
                for name, data in state['reports'].items():
                    if name.startswith('allure-results/') and ('"testMethod"' in data.decode()) and ('"value": "%s"' % allure_owner) in data.decode():
                        value = c.e.loads(data)
                        value['attachments'].append(dict(source=path, type='image/png'))
                        state['reports'][name] = c.e.encoded(value)
            return mutate
        self.capture(self.a, 'jvm', 1, attach('testPassing'))
        self.capture(self.b, 'jvm', 2, attach('testFailing'))
        owners = attachment_owners(o.load_capture(self.a, 'jvm'))[path]
        self.assertEqual([(x['test'], x['source']) for x in owners],
                         [('pkg.SampleTests#testPassing', 'allure'), ('pkg.SampleTests#testPassing', 'test-result')])
        with patch.dict(c.SCENARIOS['ios-unit-nm'], vendor='Android'):
            first = semantic_projection(o.load_capture(self.a, 'jvm'), self.policy, o.inspect_artifact, o.summary)[0]
            rewired = semantic_projection(o.load_capture(self.b, 'jvm'), self.policy, o.inspect_artifact, o.summary)[0]
        self.assertNotEqual(first['screenshots'], rewired['screenshots'])
        self.assertTrue(o.differences(first, rewired))


class GifLandmarks(unittest.TestCase):
    """Decision 14: GIF landmarks compare from the first app state; videos stay strict. Decision 16: per owning test."""
    TEST = 'com.example.MainActivity showing "Test"'

    def review(self, landmarks, owners=None, attempts=None, allure=None):
        """`landmarks` maps a recording path to its states; `owners` maps it to the test that owns it (default: named
        after the recording), and `attempts` to how many test-result rows (attempts) that test has."""
        scenario = c.SCENARIOS['ios-unit-nm']
        owners = owners or {p: Path(p).stem for p in landmarks}
        with tempfile.TemporaryDirectory() as temp:
            pair = Path(temp)
            (pair / 'jvm').mkdir()
            c.save(pair / 'jvm/witnesses.json', dict(review='r', evidence_sha256='d', media_landmarks=landmarks,
                   witnesses={w: dict(observation='x', paths=['a/b']) for w in scenario['witnesses']}))
            files = {'a/b': dict(kind='file', data=b'x')}
            files.update({p: dict(kind='file', data=b'xx') for p in landmarks})
            for test in sorted(set(owners.values())):
                for attempt in range((attempts or {}).get(test, 1)):
                    attached = [p for p, owner in owners.items() if owner == test and attempt == (attempts or {}).get(test, 1) - 1]
                    files['reports/test_result/%s-%d.json' % (test, attempt)] = dict(kind='file', data=c.e.encoded(dict(
                        test=dict(pkg='pkg', clazz='C', method=test), endTime=attempt, status='PASSED',
                        attachments=[dict(file=p, type='VIDEO') for p in attached])))
            for p, test in (allure or {}).items():  # Allure's own view of who owns each recording
                files['reports/allure-results/%s-result.json' % Path(p).stem] = dict(kind='file', data=c.e.encoded(dict(
                    labels=[dict(name='testClass', value='C'), dict(name='testMethod', value=test)],
                    attachments=[dict(source=p, type='video/mp4')])))
            capture = dict(pair=pair, side='jvm', digest='d', capture=dict(scenario='ios-unit-nm', process=dict(cwd='/synthetic')), files=files)
            policy = dict(rules=[dict(id='m', kind='media', path='**')])
            return o.review_witnesses(capture, policy)

    def test_leading_launcher_is_dropped_for_gif(self):
        a = self.review({'reports/t.gif': [self.TEST]})
        b = self.review({'reports/t.gif': ['launcher home screen', self.TEST]})
        self.assertEqual(a, b)

    def test_gif_landmark_mismatch_is_informational_only(self):
        a = self.review({'reports/t.gif': [self.TEST, 'blank screen']})
        b = self.review({'reports/t.gif': [self.TEST]})
        self.assertEqual(a[0], b[0])  # nothing gates
        self.assertNotEqual(a[1], b[1])  # but the mismatch is reported

    def test_launcher_only_gif_equals_no_landmarks(self):
        a = self.review({'reports/t.gif': ['launcher home screen']})
        b = self.review({'reports/t.gif': []})
        self.assertEqual(a, b)
        self.assertEqual(a[1]['pkg.C#t@0:VIDEO(test-result)|t.gif#0'], [])

    def test_videos_keep_leading_launcher_and_need_landmarks(self):
        a = self.review({'reports/t.mp4': [self.TEST]})
        b = self.review({'reports/t.mp4': ['launcher home screen', self.TEST]})
        self.assertNotEqual(a[0], b[0])
        with self.assertRaisesRegex(c.e.Invalid, 'invalid ordered landmarks'):
            self.review({'reports/t.mp4': []})

    def test_launcher_between_app_states_is_kept(self):
        self.assertEqual(o.gif_landmarks('t.gif', [self.TEST, 'launcher home screen']), [self.TEST, 'launcher home screen'])

    def test_swapping_two_recordings_with_their_annotations_between_tests_fails(self):
        # R1-sol-3: both sides hold the same two annotations, so a rule-id-keyed comparison could not tell them apart.
        kotlin = self.review({'reports/video/one.mp4': ['screen of test one'], 'reports/video/two.mp4': ['screen of test two']})
        native = self.review({'reports/video/one.mp4': ['screen of test two'], 'reports/video/two.mp4': ['screen of test one']})
        self.assertNotEqual(kotlin[0], native[0])
        # The same annotations on the same tests still agree, whatever the recordings are called.
        renamed = self.review({'reports/video/uno-1.mp4': ['screen of test one'], 'reports/video/dos-2.mp4': ['screen of test two']},
                              owners={'reports/video/uno-1.mp4': 'one', 'reports/video/dos-2.mp4': 'two'})
        self.assertEqual(kotlin[0], renamed[0])

    def test_landmarks_follow_the_attempt_and_role_that_own_the_recording(self):
        first = self.review({'reports/video/a.mp4': ['state']}, owners={'reports/video/a.mp4': 'one'})
        retry = self.review({'reports/video/a.mp4': ['state']}, owners={'reports/video/a.mp4': 'one'}, attempts={'one': 2})
        self.assertNotEqual(first[0], retry[0])  # same states, but now the second attempt's recording
        self.assertEqual(retry[0][0]['owners'], [dict(test='pkg.C#one', attempt=1, role='VIDEO', source='test-result')])

    def test_swapping_only_the_allure_ownership_of_two_recordings_fails(self):
        # R2-sol-3: the test-result rows, recordings and annotations are untouched; only Allure's sources trade places.
        landmarks = {'reports/video/one.mp4': ['screen of test one'], 'reports/video/two.mp4': ['screen of test two']}
        kotlin = self.review(landmarks, allure={'reports/video/one.mp4': 'one', 'reports/video/two.mp4': 'two'})
        native = self.review(landmarks, allure={'reports/video/one.mp4': 'two', 'reports/video/two.mp4': 'one'})
        self.assertNotEqual(kotlin[0], native[0])
        self.assertEqual(kotlin[0], self.review(landmarks, allure={'reports/video/one.mp4': 'one', 'reports/video/two.mp4': 'two'})[0])

    def test_a_reviewed_recording_that_no_test_owns_is_rejected(self):
        with self.assertRaisesRegex(c.e.Invalid, 'no owning test attempt'):
            self.review({'reports/video/orphan.mp4': ['state']}, owners={'reports/video/other.mp4': 'one'})


if __name__ == '__main__':
    unittest.main()
