#!/usr/bin/env python3
"""Adversarial checks go through the production comparator, with recomputed digests."""
import base64
import gzip
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

    def reject_changed_yaml(self, before, after):
        target = self.tree / 'result.yml'
        target.write_bytes(before)
        baseline = self.record('yaml-baseline')
        expected = e.normalized(baseline, self.contract, native=True)
        target.write_bytes(after)
        actual = self.record('yaml-mutant')
        with self.assertRaises(e.Invalid):
            e.compare_native_views(expected, e.normalized(actual, self.contract, native=True))

    def test_native_yaml_rejects_scalar_tag_on_mapping(self):
        self.reject_changed_yaml(b'---\na: b\n', b'--- !!str\na: b\n')

    def test_native_yaml_distinguishes_boolean_and_integer(self):
        self.reject_changed_yaml(b'a: true\n', b'a: 1\n')

    def test_native_yaml_preserves_full_decimal_precision(self):
        self.reject_changed_yaml(b'a: 0.123456789012345678901\n', b'a: 0.123456789012345678902\n')

    def test_native_yaml_distinguishes_finite_floats_above_binary_range(self):
        self.reject_changed_yaml(b'a: 1.0e+309\n', b'a: 2.0e+309\n')

    def test_native_yaml_accepts_equivalent_numeric_spellings(self):
        def value(text):
            return e.structured_yaml(('a: ' + text + '\n').encode())
        for equivalent in ['1.', '+1.0', '1.00']:
            self.assertEqual(value('1.0'), value(equivalent), equivalent)
        self.assertEqual(value('.5'), value('0.5'))
        self.assertEqual(value('1e3'), value('1_000.0'))
        self.assertEqual(value('1:20.5'), value('80.5'))
        self.assertEqual(value('.Inf'), value('+.INF'))
        self.assertNotEqual(value('1.0e+309'), value('.inf'))
        self.assertNotEqual(value('-.inf'), value('.inf'))
        self.assertEqual(value('.NaN'), value('.nan'))

    def test_missing_yaml_helper_reports_build_command(self):
        with mock.patch.dict(os.environ, {'CARGO_TARGET_DIR': str(self.root / 'missing-target')}):
            with self.assertRaisesRegex(e.Invalid, 'cargo build -q -p snakeyaml --example yaml_tree'):
                e.structured_yaml(b'a: 1\n')

    def test_native_yaml_matches_shared_implicit_scalar_contract(self):
        for line in (ROOT / 'fixtures/contract/yaml-scalars.tsv').read_text().splitlines():
            scalar, expected = line.split('\t')
            with self.subTest(scalar=scalar):
                self.assertEqual(e.yaml_implicit_type(scalar), expected)
                if scalar not in ('._', '-'):  # malformed number or YAML sequence indicator
                    document = e.structured_yaml(('a: ' + scalar + '\n').encode())
                    self.assertEqual(document[0][2][0][1][1], expected)
        self.assertEqual(e.structured_yaml(b'a: 1._0\n'), e.structured_yaml(b'a: 1.0\n'))
        self.assertNotEqual(e.structured_yaml(b'a: .inf\n'), e.structured_yaml(b'a: .iNF\n'))

    def test_native_yaml_rejects_changed_infinity_to_string(self):
        self.reject_changed_yaml(b'a: .inf\n', b'a: .iNF\n')

    def test_native_xml_indentation_and_significant_text(self):
        target = self.tree / 'result.xml'
        target.write_bytes(b'<root><item id="1">hello</item><item>world</item></root>')
        expected = e.normalized(self.record('xml-baseline'), self.contract, native=True)
        target.write_bytes(b'<root>\n  <item id="1">hello</item>\n  <item>world</item>\n</root>')
        e.compare_native_views(expected, e.normalized(self.record('xml-indented'), self.contract, native=True))
        target.write_bytes(b'<root>\n  <item id="1">hello!</item>\n  <item>world</item>\n</root>')
        with self.assertRaises(e.Invalid):
            e.compare_native_views(expected, e.normalized(self.record('xml-changed'), self.contract, native=True))
        self.assertNotEqual(e.xml_value(b'<a> </a>'), e.xml_value(b'<a></a>'))

    def test_allure_environment_accepts_indentation_and_real_values(self):
        import vendor_reports
        identities = {'sandbox':'/sandbox', 'port':'22'}
        compact = b'<environment><parameter><key>analyticsConfiguration</key><value>com.malinskiy.marathon.config.Foo@abc</value></parameter></environment>'
        pretty = b'<environment>\n <parameter>\n  <key>analyticsConfiguration</key>\n  <value>com.malinskiy.marathon.config.Foo@abc</value>\n </parameter>\n</environment>'
        first = e.xml_value(vendor_reports.environment(compact, identities, 'native', []))
        self.assertEqual(first, e.xml_value(vendor_reports.environment(pretty, identities, 'native', [])))
        real = pretty.replace(b'com.malinskiy.marathon.config.Foo@abc', b'analytics enabled')
        self.assertNotEqual(first, e.xml_value(vendor_reports.environment(real, identities, 'native', [])))

    def test_native_environment_keeps_identity_suffix_and_class_exact(self):
        import vendor_reports
        identities = {'sandbox': '/sandbox', 'port': '22'}
        for key, value in [
            ('analyticsConfiguration', 'com.malinskiy.marathon.config.Foo@abc'),
            ('vendorConfiguration', 'testParserConfiguration=com.malinskiy.marathon.config.vendor.android.TestParserConfiguration$LocalTestParserConfiguration@abc, '),
        ]:
            data = f'<environment><parameter><key>{key}</key><value>{value}</value></parameter></environment>'.encode()
            native = vendor_reports.environment(data, identities, 'native', [], native=True)
            self.assertIn(value.encode(), native)
            self.assertNotIn(b'{class-identity}', native)
            self.assertNotEqual(e.xml_value(native), e.xml_value(vendor_reports.environment(
                data.replace(b'@abc', b'@def'), identities, 'native', [], native=True)))
            historical = vendor_reports.environment(data, identities, 'jvm', ['com.malinskiy.marathon.config.Foo@abc', value.split('=', 1)[-1].removesuffix(', ')])
            self.assertIn(b'{class-identity}', historical)

    def test_native_state_json_compares_parsed_data(self):
        self.artifacts['state.json'] = b'{"devices":[1.0e10],"ready":true}'
        expected = e.normalized(self.record('state-baseline'), self.contract, native=True)
        self.artifacts['state.json'] = b'{ "devices": [10000000000.0], "ready": true }'
        e.compare_native_views(expected, e.normalized(self.record('state-equivalent'), self.contract, native=True))
        self.artifacts['state.json'] = b'{"devices":[10000000000.0],"ready":false}'
        with self.assertRaises(e.Invalid):
            e.compare_native_views(expected, e.normalized(self.record('state-changed'), self.contract, native=True))

    def html_view(self, name, data):
        return {name: {'kind': 'file', 'mode': 0o644,
                       'content': base64.b64encode(data).decode()}}

    def test_native_html_embedded_json_accepts_numeric_and_escape_spellings(self):
        for name, assignment in [('reports/html/pools/p/test.html', b'window.test'),
                                 ('reports/html/timeline/index.html', b'var dataset')]:
            before = b'<script>\n  ' + assignment + br' = {"n":1.0E10,"s":"\u003c"};' + b'\n</script>'
            after = before.replace(b'1.0E10', b'10000000000.0').replace(br'\u003c', b'<')
            e.compare_native_views(self.html_view(name, before), self.html_view(name, after))

    def test_native_html_rejects_changed_data_key_order_and_surrounding_bytes(self):
        name = 'reports/html/index.html'
        before = b'<script>\nwindow.mainData = {"n":1,"s":"text","a":[1,2],"z":null};\n</script>'
        mutations = [(b'"n":1', b'"n":2'), (b'"n":1,"s":"text"', b'"s":"text","n":1'),
                     (b'<script>', b'<script >'), (b' = ', b'  = '), (b';', b''),
                     (b'[1,2]', b'[2,1]'), (b',"z":null', b''), (b'"n":1', b'"n":true')]
        for old, new in mutations:
            with self.subTest(new=new), self.assertRaises(e.Invalid):
                e.compare_native_views(self.html_view(name, before), self.html_view(name, before.replace(old, new)))

    def test_native_html_scope_and_historical_comparison_stay_byte_exact(self):
        before = b'<script>\nwindow.test = {"n":1.0E10}\n</script>'
        after = before.replace(b'1.0E10', b'10000000000.0')
        for name in ['reports/other/index.html', 'tree/html/index.html', 'reports/html/app.js']:
            with self.subTest(name=name), self.assertRaises(e.Invalid):
                e.compare_native_views(self.html_view(name, before), self.html_view(name, after))
        (self.tree / 'html').mkdir()
        target = self.tree / 'html/index.html'
        target.write_bytes(before)
        expected = self.record('html-before')
        target.write_bytes(after)
        actual = self.record('html-after')
        with self.assertRaises(e.Invalid):
            e.compare(expected, actual, self.contract)
        e.compare_native_views(e.normalized(expected, self.contract, native=True),
                               e.normalized(actual, self.contract, native=True))

    def test_native_html_json_lookalikes_outside_scripts_remain_exact(self):
        name = 'reports/html/index.html'
        for before in [b'window.test = {"n":1.0E10}\n',
                       b'<script>\nvar dataset = {"n":1.0E10}\n</script>',
                       b'<script>if (ready) window.test = {"n":1.0E10}</script>',
                       b'<!-- <script>\nwindow.test = {"n":1.0E10}\n</script> -->',
                       b"<div title='<script>\nwindow.test = {\"n\":1.0E10}\n</script>'>text</div>",
                       b"<script title='>\nwindow.fake = {\"n\":1.0E10}\n'>\nwindow.test = {\"n\":0}\n</script>"]:
            with self.subTest(before=before), self.assertRaises(e.Invalid):
                e.compare_native_views(self.html_view(name, before),
                                       self.html_view(name, before.replace(b'1.0E10', b'10000000000.0')))

    def reject_js_number_spelling_change(self, body):
        for name, fake in [('reports/html/index.html', b'window.fake'),
                           ('reports/html/timeline/index.html', b'var dataset')]:
            before = (b'<script>\n' + body.replace(b'window.fake', fake)
                      + b'window.test = {"n":2.0};\n</script>')
            with self.subTest(name=name, body=body), self.assertRaises(e.Invalid):
                e.compare_native_views(self.html_view(name, before),
                                       self.html_view(name, before.replace(b'1.0', b'1e0')))

    def test_native_html_js_block_comments_remain_byte_exact(self):
        self.reject_js_number_spelling_change(b'/*\nwindow.fake = {"n":1.0}\n*/\n')

    def test_native_html_js_line_comments_remain_byte_exact(self):
        self.reject_js_number_spelling_change(b'// window.fake = {"n":1.0}; quotes: \'"` /*\n')

    def test_native_html_js_string_literals_remain_byte_exact(self):
        for quote in [b"'", b'"']:
            payload = b'window.fake = {"n":1.0}'
            if quote == b'"':
                payload = payload.replace(b'"', br'\"')
            self.reject_js_number_spelling_change(b'const literal = ' + quote + b'\\\n'
                                                 + payload + b'\\\n' + quote + b';\n')

    def test_native_html_js_template_literals_remain_byte_exact(self):
        for body in [b'const literal = `\nwindow.fake = {"n":1.0}\n`;\n',
                     b'const literal = `${ "`" + `nested\nwindow.fake = {"n":1.0}\n` }\n`;\n',
                     b'const literal = `${ (() => {\nwindow.fake = {"n":1.0}\nreturn `nested`; })() }`;\n']:
            self.reject_js_number_spelling_change(body)

    def test_native_html_nested_js_assignments_remain_byte_exact(self):
        for opening, closing in [(b'function render() {', b'}'), (b'const values = [', b'];'), (b'(', b');')]:
            self.reject_js_number_spelling_change(opening + b'\nwindow.fake = {"n":1.0}\n' + closing + b'\n')

    def test_native_html_js_contexts_resume_at_top_level(self):
        prefixes = [b'// quotes: \'"` /*\n',
                    b'/* quotes: \'"` //\n*/\n',
                    b"const literal = 'escaped \\' /* `';\n",
                    b'const literal = "escaped \\" // `";\n',
                    b'const literal = `${ "`" + `nested` }`;\n',
                    b'function render() { return [("quoted")]; }\n']
        name = 'reports/html/index.html'
        for prefix in prefixes:
            before = b'<script>\n' + prefix + b'window.test = {"n":1.0E10};\nwindow.pool = {"n":3};\n</script>'
            after = before.replace(b'1.0E10', b'10000000000.0')
            with self.subTest(prefix=prefix):
                e.compare_native_views(self.html_view(name, before), self.html_view(name, after))
                with self.assertRaises(e.Invalid):
                    e.compare_native_views(self.html_view(name, before),
                                           self.html_view(name, after.replace(b'{"n":3}', b'{"n":4}')))

    def test_native_html_multiple_assignments_and_invalid_json(self):
        name = 'reports/html/index.html'
        before = b'<script>\nwindow.test = {"text":"} ; window.fake = 0"};\nwindow.logs = [1,null]\n</script>'
        with self.assertRaises(e.Invalid):
            e.compare_native_views(self.html_view(name, before), self.html_view(name, before.replace(b'[1,null]', b'[2,null]')))
        for invalid in [b'{"n":NaN}', b'{"n":1,"n":1}', b'{broken']:
            with self.subTest(invalid=invalid), self.assertRaises((e.Invalid, ValueError)):
                e.compare_native_views(self.html_view(name, before), self.html_view(name, b'<script>\nwindow.test = ' + invalid + b'\n</script>'))

    def test_stale_yaml_helper_reports_build_command(self):
        target = self.root / 'target'
        executable = target / 'debug/examples/yaml_tree'
        executable.parent.mkdir(parents=True)
        executable.write_bytes(b'old')
        source = self.root / 'changed.rs'; source.write_text('new')
        stamp = executable.stat().st_mtime_ns + 2_000_000_000
        os.utime(source, ns=(stamp, stamp))
        with mock.patch.dict(os.environ, {'CARGO_TARGET_DIR': str(target)}), mock.patch.object(e, 'yaml_helper_sources', return_value=[source]):
            with self.assertRaisesRegex(e.Invalid, 'stale YAML helper; run cargo build'):
                e.structured_yaml(b'a: 1\n')

    def test_golden_diff_is_readable_and_detects_additions(self):
        before = gzip.compress(b'{"value": 1}'); after = gzip.compress(b'{"value": 2}')
        text = DRIVER['diff_view'](before, after, 'case.json.gz', 'baseline')
        self.assertIn('-  "value": 1', text)
        self.assertIn('+  "value": 2', text)
        self.assertIn('+  "value": 2', DRIVER['diff_view'](None, after, 'case.json.gz', 'baseline'))

    def test_capture_requires_explicit_historical_flag(self):
        args = ['run', 'capture', 'local', '--output', str(self.root/'never-created')]
        with mock.patch.object(sys, 'argv', args), self.assertRaisesRegex(e.Invalid, '--historical'):
            DRIVER['main']()
        self.assertFalse((self.root/'never-created').exists())

    def test_native_only_scenario_rejects_a_historical_jvm_capture(self):
        args = ['run', 'capture', 'android-instrumentation-eof', '--historical',
                '--output', str(self.root/'never-created')]
        with mock.patch.object(sys, 'argv', args), self.assertRaisesRegex(e.Invalid, 'native-only'):
            DRIVER['main']()
        self.assertFalse((self.root/'never-created').exists())

    def test_all_uses_native_captures_only(self):
        namespace = DRIVER['main'].__globals__
        selected = {'local': {'kind':'portable'}, 'parse': {'kind':'portable'}}
        capture = mock.Mock(return_value=self.root/'recording')
        command = mock.Mock()
        args = ['run', 'all', '--profile', 'harness-first', '--output', str(self.root/'all')]
        with mock.patch.dict(namespace, {'validate_registry': lambda: {'cases': []},
                             'adapters': lambda: selected, 'command': command,
                             'capture': capture, 'compare_golden': mock.Mock()}), mock.patch.object(sys, 'argv', args):
            DRIVER['main']()
        self.assertEqual([call.args[1] for call in capture.call_args_list], ['native', 'native'])
        self.assertTrue(all('oracle/build.sh' not in str(call.args[0]) for call in command.call_args_list))

    def test_native_console_ignores_other_diagnostics(self):
        values = {'serial': 'device-1'}
        first = b'diagnostic one\n00% | [omni]-[device-1] C#t passed\n'
        second = b'different diagnostic\n00% | [omni]-[device-1] C#t passed\n'
        self.assertEqual(e.contract_console(first, values), e.contract_console(second, values))
        self.assertNotEqual(e.contract_console(first, values), e.contract_console(second.replace(b'passed', b'failed'), values))

    def test_native_structured_and_command_adversaries(self):
        self.contract['paths'].remove('reports/log')
        baseline = self.record('native-baseline')
        expected = e.normalized(baseline, self.contract, native=True)
        target = self.tree / 'result.json'
        original = target.read_bytes()
        for index, changed in enumerate((
                b'{"status":"FAILED","failure":"","attachments":["log"],"tests":["a","b"]}',
                b'{"failure":"","status":"PASSED","attachments":["log"],"tests":["a","b"]}')):
            target.write_bytes(changed)
            actual = self.record('changed-' + str(index))
            with self.assertRaises(e.Invalid):
                e.compare_native_views(expected, e.normalized(actual, self.contract, native=True))
        target.write_bytes(original)
        (self.tree / 'log').unlink()
        actual = self.record('missing-native')
        with self.assertRaises(e.Invalid):
            e.compare_native_views(expected, e.normalized(actual, self.contract, native=True))
        (self.tree / 'log').write_bytes(b'literal 12345678-1234-1234-1234-123456789abc\n')
        self.artifacts['transcript.json'] = b'{"commands":[["xcodebuild","-destination-timeout","31"]],"cleanup":["close","join"],"terminal":"complete"}'
        actual = self.record('command-native')
        with self.assertRaises(e.Invalid):
            e.compare_native_views(expected, e.normalized(actual, self.contract, native=True))
        self.assertEqual(e.structured_json(b'{"a":"<","n":1.0E10}'),
                         e.structured_json(b'{"a":"\\u003c","n":10000000000.0}'))
        self.assertEqual(e.xml_value(b'<a>&lt;</a>'), e.xml_value(b'<a><![CDATA[<]]></a>'))
        self.assertEqual(e.structured_yaml(b'a: 1.0\n'), e.structured_yaml(b'a: 1.00\n'))
        self.assertEqual(e.structured_yaml(b'a: !!str true\n'), e.structured_yaml(b'a: "true"\n'))
        self.assertEqual(e.structured_yaml(b'a: &x [1, 2]\nb: *x\n'), e.structured_yaml(b'a: [1, 2]\nb: [1, 2]\n'))

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
        # A barrier line names no batch, and Kotlin's await logs outside its lock, so an await that
        # began before this admission may log before the wait: barriers do not close the window.
        for b in [barrier, other_barrier]:
            with self.subTest(barrier=b):
                canonical = vc.normalize((admission + wait + b + final).encode(), values, 'native', 'stdout')
                self.assertEqual(vc.normalize((admission + b + wait + final).encode(), values, 'native', 'stdout'), canonical)
        for lines in [[admission, progress, final, wait], [wait, progress, admission, final]]:
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
            with self.subTest(native_option=old), self.assertRaises(e.Invalid):
                e.compare_native_views(e.normalized(self.source, self.contract, native=True),
                                       e.normalized(self.actual, self.contract, native=True))

    @staticmethod
    def as_live_native(directory, record):
        """Changing a pinned native record makes it a run of today's code, which holds what today's code sends."""
        import vendor_transport as t
        if record['implementation'] != 'native' or not e.may_withhold(directory, record):
            return
        row = next(r for r in record['entries'] if r['path'] == 'host-argv.jsonl')
        rows = [json.loads(line) for line in e.payload(directory, row, record).splitlines()]
        for argv in rows:
            argv['argv'] = t.with_d43_flag(argv['program'], t.with_d31_flag(argv['program'], argv['argv']))
        data = b''.join(json.dumps(r, separators=(',',':'), sort_keys=True).encode() + b'\n' for r in rows)
        (directory / 'blobs' / row['sha256']).unlink()
        row.update(sha256=e.sha(data), size=len(data))
        e.save_payload(directory, row['sha256'], data, record)
        row = next(r for r in record['entries'] if r['path'] == 'transcript.jsonl')
        rows = [json.loads(line) for line in e.payload(directory, row, record).splitlines()]
        rows = t.with_d46_probe(rows)
        for event in rows:
            if event['event'] == 'exec':
                wire = t.with_d32_prefix(base64.b64decode(event['detail']['base64']).decode()).encode()
                event['detail'].update(base64=base64.b64encode(wire).decode(), sha256=e.sha(wire), size=len(wire))
        data = b''.join(json.dumps(r, separators=(',',':'), sort_keys=True).encode() + b'\n' for r in rows)
        (directory / 'blobs' / row['sha256']).unlink()
        row.update(sha256=e.sha(data), size=len(data))
        e.save_payload(directory, row['sha256'], data, record)

    def parser_copy(self, source, name, content=None, mode=None, **fields):
        """A copy of a committed macOS record whose parser entry holds `content` or is rewritten by `fields`."""
        destination = self.tmp_root / name
        shutil.copytree(source, destination)
        record = json.loads((destination / 'evidence.json').read_bytes())
        self.as_live_native(destination, record)
        row = next(r for r in record['entries'] if r['path'] == e.PARSER_PATH)
        self.assertIs(row['withheld'], True)
        if content is not None:
            del row['withheld']
            row.update(size=len(content), sha256=e.sha(content))
            e.save_payload(destination, row['sha256'], content, record)
        if mode is not None:
            row['mode'] = mode
        row.update(fields)
        (destination / 'evidence.json').write_bytes(e.encoded(record))
        return destination

    def test_d27_only_the_root_wipe_of_a_jvm_record_is_left_out(self):
        import vendor_transport as t
        def transcript(*commands):
            rows = []
            for conn,command in enumerate(commands,1):
                rows.append(dict(type='shell',conn=conn,serial='s',command=command))
                rows.append(dict(type='output',conn=conn,stream='stdout',bytes=[120]))
            rows.append(dict(type='event',event='server_stopped'))
            return b''.join(json.dumps(r).encode()+b'\n' for r in rows)
        def commands(data,ran_the_wipe):
            return [row['command'] for row in t.adb(data,ran_the_wipe)['device:s']]
        wipe = transcript('rm -r /sdcard','mkdir /sdcard','pm list packages')
        self.assertEqual(commands(wipe,True),['pm list packages'])
        # A native run of today's code that wipes again keeps the commands, so it differs from the recording.
        self.assertEqual(commands(wipe,False),['rm -r /sdcard','mkdir /sdcard','pm list packages'])
        others = ['rm -r /sdcard/fixture','mkdir /sdcard/fixture','rm -r /storage/emulated/0','rm /sdcard','rm -r /','rm -r /sdcard ']
        self.assertEqual(commands(transcript(*others),True),others)

    def test_d27_a_new_native_record_never_counts_as_having_run_the_wipe(self):
        # The digest pin that scopes D20 scopes this too: only committed records from before D27 qualify.
        record = dict(implementation='native')
        (self.root / 'evidence.json').write_bytes(b'{}')
        self.assertFalse(e.may_withhold(self.root,record))
        self.assertTrue(e.may_withhold(self.root,dict(implementation='jvm')))

    def d31_argv(self, *rows):
        import vendor_transport as t
        data = b''.join(json.dumps(dict(event='host_argv',program=program,argv=argv)).encode()+b'\n' for program,argv in rows)
        values = dict(sandbox='/sandbox',safe_serial='serial',port='1')
        return lambda before: t.host_argv(data,values,before)

    def test_d31_d43_a_transfer_of_an_older_record_gets_exactly_the_flag(self):
        send = lambda *flag: ['--recursive','--times',*flag,'--rsh','ssh -p 1 ','--delete','/src','127.0.0.1:/tmp/marathon/x']
        receive = lambda *flag: ['--recursive','--times',*flag,'--rsh','ssh -p 1 ','--delete','127.0.0.1:/tmp/marathon/x','/dst']
        version = ['--version','/tmp','/tmp']
        flag = '--timeout=300'
        old = self.d31_argv(('rsync',send()),('rsync',receive()),('rsync',version))(True)
        new = self.d31_argv(('rsync',send(flag)),('rsync',receive(flag)),('rsync',version))(False)
        # Today's native run compares equal to the JVM's.
        self.assertEqual(old,new)
        # On either kind the flag is missing, has another value or sits elsewhere: all differ.
        for build in [send,receive]:
            for argv in [build(),build('--timeout=30'),[flag,*build()]]:
                rows = [argv if build is send else send(flag), argv if build is receive else receive(flag)]
                with self.subTest(argv):
                    self.assertNotEqual(old,self.d31_argv(('rsync',rows[0]),('rsync',rows[1]),('rsync',version))(False))
        # An older record is not given it on the version form, and neither program but rsync gets it.
        self.assertEqual(self.d31_argv(('rsync',version))(True)['rsync:/tmp'][0]['argv'],version)

    def test_d31_d43_an_older_record_that_already_has_the_flag_is_refused(self):
        with self.assertRaisesRegex(e.Invalid,'D31 flag'):
            self.d31_argv(('rsync',['--times','--timeout=300','/src','127.0.0.1:/tmp/marathon/x']))(True)
        with self.assertRaisesRegex(e.Invalid,'D43 flag'):
            self.d31_argv(('rsync',['--times','--timeout=300','127.0.0.1:/tmp/marathon/x','/dst']))(True)

    def test_d31_leaves_the_ssh_form_of_an_older_record_alone(self):
        ssh = ['-l','u','-p','1','127.0.0.1','/usr/bin/rsync','--server','--times','.','/tmp/marathon/x']
        self.assertEqual(self.d31_argv(('ssh',ssh))(True),self.d31_argv(('ssh',ssh))(False))

    @staticmethod
    def d32_control_lane(*wires, before=False):
        import vendor_transport as t
        rows = []
        for channel,wire in enumerate(wires):
            data = wire.encode()
            rows.append(dict(event='exec',connection=1,channel=channel,detail=dict(base64=base64.b64encode(data).decode(),sha256=e.sha(data),size=len(data))))
            rows.append(dict(event='exit_status',connection=1,channel=channel,detail=dict(status=0)))
            rows.append(dict(event='channel_end',connection=1,channel=channel,detail=dict(outcome='completed')))
        rows.append(dict(event='connection_end',connection=1,channel=None,detail={}))
        rows.append(dict(event='cleanup',connection=None,channel=None,detail=dict(outcome='completed',active_processes=[])))
        for seq,row in enumerate(rows):
            row['seq'] = seq
        data = b''.join(json.dumps(row).encode()+b'\n' for row in rows)
        return [session['wire'] for session in t.ssh(data,None,before)['control']]

    def test_d32_only_the_xcodebuild_test_command_of_an_older_record_gets_the_prefix(self):
        test = 'xcrun xcodebuild test-without-building -xctestrun /tmp/marathon/m.xctestrun'
        others = ['xcrun xcodebuild -version','xcrun simctl boot U','sh -c "xcrun xcodebuild test-without-building"',' ' + test]
        prefix = 'env NSUnbufferedIO=YES '
        old = self.d32_control_lane(test,*others,before=True)
        self.assertEqual(old,[prefix + test,*others])
        # Today's native run compares equal to the JVM's.
        self.assertEqual(old,self.d32_control_lane(prefix + test,*others))
        # No prefix, another form of it, or the prefix on another command: all differ.
        for wire,rest in [(test,others),('NSUnbufferedIO=YES ' + test,others),('env NSUnbufferedIO=NO ' + test,others),
                          (prefix + test,[prefix + others[0],*others[1:]])]:
            with self.subTest(wire):
                self.assertNotEqual(old,self.d32_control_lane(wire,*rest))

    def test_d32_an_older_record_that_already_has_the_prefix_is_refused(self):
        with self.assertRaisesRegex(e.Invalid,'D32 prefix'):
            self.d32_control_lane('env NSUnbufferedIO=YES xcrun xcodebuild test-without-building -x',before=True)

    def test_d32_the_catalogue_form_loses_the_prefix_on_this_command_only(self):
        import vendor_transport as t
        test = 'xcrun xcodebuild test-without-building -x'
        self.assertEqual(t.without_d32_prefix('env NSUnbufferedIO=YES ' + test),test)
        for other in [test,'env NSUnbufferedIO=YES xcrun xcodebuild -version','env NSUnbufferedIO=NO ' + test]:
            self.assertEqual(t.without_d32_prefix(other),other)

    @staticmethod
    def d46_control_lane(*sessions, before=False):
        """Control-lane wires of a record whose sessions are (wire, stdout) pairs, with the older record's allowance or not."""
        import vendor_transport as t
        rows = []
        for channel,(wire,stdout) in enumerate(sessions):
            for event,data in [('exec',wire.encode()),('output',stdout.encode())]:
                if event == 'output' and not data:
                    continue
                rows.append(dict(event=event,connection=1,channel=channel,detail=dict(base64=base64.b64encode(data).decode(),sha256=e.sha(data),size=len(data))))
            rows.append(dict(event='exit_status',connection=1,channel=channel,detail=dict(status=0)))
            rows.append(dict(event='channel_end',connection=1,channel=channel,detail=dict(outcome='completed')))
        rows.append(dict(event='connection_end',connection=1,channel=None,detail={}))
        rows.append(dict(event='cleanup',connection=None,channel=None,detail=dict(outcome='completed',active_processes=[])))
        for seq,row in enumerate(rows):
            row['seq'] = seq
        data = b''.join(json.dumps(row).encode()+b'\n' for row in rows)
        return [session['wire'] for session in t.ssh(data,None,before)['control']]

    D46_OLD = 'xcrun simctl spawn U launchctl dumpstate user/foreground/com.apple.SpringBoard'
    D46_NEW = 'xcrun simctl spawn U launchctl print user/foreground'
    D46_GETENV = 'xcrun simctl getenv U SIMULATOR_RUNTIME_VERSION'

    def test_d46_only_the_probe_of_a_runtime_18_or_later_in_an_older_record_changes(self):
        old, new = self.D46_OLD, self.D46_NEW
        for reply in ['18.0\n','18.6\n','26.4\n','27.0\n','099.9\n','18','+18.1\n','2147483647.0\n','0002147483647.0\n']:
            with self.subTest(reply):
                self.assertEqual(self.d46_control_lane((self.D46_GETENV,reply),(old,''),before=True),[self.D46_GETENV,new])
                # Today's native run compares equal to the JVM's; the older program's probe does not.
                self.assertEqual(self.d46_control_lane((self.D46_GETENV,reply),(old,''),before=True),
                                 self.d46_control_lane((self.D46_GETENV,reply),(new,'')))
                self.assertNotEqual(self.d46_control_lane((self.D46_GETENV,reply),(old,''),before=True),
                                    self.d46_control_lane((self.D46_GETENV,reply),(old,'')))
        # Runtimes 16 and 17, a version that does not parse as a number, and no recorded version keep the recorded wire.
        # A major past i32::MAX fails the program's parse too.
        for reply in ['16.0\n','17.5\n','17\n','','x.1\n','１８.0\n','-18.0\n','1x8.0\n','2147483648.0\n','99999999999999999999.1\n']:
            with self.subTest(reply):
                self.assertEqual(self.d46_control_lane((self.D46_GETENV,reply),(old,''),before=True),[self.D46_GETENV,old])
                self.assertNotEqual(self.d46_control_lane((self.D46_GETENV,reply),(old,''),before=True),
                                    self.d46_control_lane((self.D46_GETENV,reply),(new,'')))
        self.assertEqual(self.d46_control_lane((old,''),before=True),[old])

    def test_d46_the_allowance_reads_the_version_of_the_probed_simulator_and_changes_no_other_command(self):
        old, new = self.D46_OLD, self.D46_NEW
        other = 'xcrun simctl spawn V launchctl dumpstate user/foreground/com.apple.SpringBoard'
        # Runtime 18 on U says nothing about V, and a version read of another key is not the runtime.
        self.assertEqual(self.d46_control_lane((self.D46_GETENV,'27.1\n'),(other,''),before=True),[self.D46_GETENV,other])
        self.assertEqual(self.d46_control_lane(('xcrun simctl getenv U SIMULATOR_RUNTIME_BUILD_VERSION','27.1\n'),(old,''),before=True),
                         ['xcrun simctl getenv U SIMULATOR_RUNTIME_BUILD_VERSION',old])
        untouched = [old + ' -x','xcrun simctl spawn U launchctl dumpstate user/foreground','xcrun simctl spawn U launchctl print system',
                     'xcrun simctl spawn U launchctl dumpstate system',"sh -c '" + old + "'",'env ' + old,'xcrun simctl spawn U launchctl print user/foreground/com.apple.SpringBoard']
        self.assertEqual(self.d46_control_lane((self.D46_GETENV,'27.1\n'),*[(w,'') for w in untouched],before=True),[self.D46_GETENV,*untouched])
        # Without the allowance (a new native record) nothing changes, and both forms are listed in the catalogue's one form.
        self.assertEqual(self.d46_control_lane((self.D46_GETENV,'27.1\n'),(old,''),(new,'')),[self.D46_GETENV,old,new])
        import vendor_transport as t
        self.assertEqual(t.without_d46_probe(new),old)
        for wire in [old,new + ' -x',new.replace('print','dump'),'env ' + new,new.replace('user/foreground','system')]:
            self.assertEqual(t.without_d46_probe(wire),wire)

    def test_d46_an_older_record_that_already_has_the_new_probe_is_refused(self):
        with self.assertRaisesRegex(e.Invalid,'D46 probe'):
            self.d46_control_lane((self.D46_GETENV,'27.1\n'),(self.D46_NEW,''),before=True)

    def test_d46_a_live_native_probe_equals_the_recorded_dumpstate_in_the_ios_records(self):
        import vendor_transport as t
        golden, contract = DRIVER['golden']('ios-smoke'), DRIVER['contract']('ios-smoke')
        recorded = ROOT / 'fixtures/differential/vendor-verification-c95b2128/ios-smoke/native'
        def probes(directory):
            record = e.load(directory / 'evidence.json')
            row = next(r for r in record['entries'] if r['path'] == 'transcript.jsonl')
            return [wire for r in t.records(e.payload(directory,row,record)) if r['event'] == 'exec'
                    for wire in [t.payload(r['detail']).decode()] if ' launchctl ' in wire]
        probe = 'xcrun simctl spawn AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE launchctl '
        # The JVM's witness and the older native pin hold dumpstate; a live run of today's program holds print.
        self.assertEqual(probes(golden),[probe + 'dumpstate user/foreground/com.apple.SpringBoard'] * 2)
        self.assertEqual(probes(recorded),probes(golden))
        live = self.rewrite_transcript(recorded,'live-ios',lambda rows: None)
        self.assertEqual(probes(live),[probe + 'print user/foreground'] * 2)
        e.compare(golden,live,contract)
        def keep_dumpstate(rows):
            for row in rows:
                if row['event'] == 'exec' and t.payload(row['detail']).decode().endswith(' launchctl print user/foreground'):
                    data = t.payload(row['detail']).decode().replace('print user/foreground','dumpstate user/foreground/com.apple.SpringBoard').encode()
                    row['detail'].update(base64=base64.b64encode(data).decode(),sha256=e.sha(data),size=len(data))
        # A program that kept dumpstate on runtime 27.1 differs from the JVM's record under the allowance, as it should.
        with self.assertRaises(e.Invalid):
            e.compare(golden,self.rewrite_transcript(recorded,'kept-dumpstate',keep_dumpstate),contract)

    def test_d20_stand_in_matches_the_withheld_library_and_nothing_else_does(self):
        self.tmp_root = Path(tempfile.mkdtemp(dir=self.root))
        contract = DRIVER['contract']('macos-smoke')
        golden = DRIVER['golden']('macos-smoke')
        recorded = ROOT / 'fixtures/differential/vendor-verification-c95b2128/macos-smoke/native'
        def small_chunks(rows):
            for row in self.parser_chunks(rows):
                del row['detail']['redacted']
                self.set_payload(row, b's')
        # A changed record is a new native, so these need a transcript a new native may hold.
        native = self.rewrite_transcript(recorded, 'native-small-chunks', small_chunks)
        # Positive: what a live native run records compares equal to the JVM's withheld copy.
        e.compare(golden, self.parser_copy(native, 'stand-in', e.PARSER_STAND_IN), contract)
        e.compare(golden, recorded, contract)
        for label, source, kwargs, message in [
            ('other bytes', native, dict(content=b'another library\n'), 'worker/shared/libxctest-parser.dylib'),
            ('empty file', native, dict(content=b''), 'worker/shared/libxctest-parser.dylib'),
            ('mode', native, dict(content=e.PARSER_STAND_IN, mode=0o644), 'mode'),
            ('stored digest', native, dict(content=e.PARSER_STAND_IN, sha256=e.PARSER_DIGEST), 'third-party library bytes stored'),
            ('withheld wrong digest', native, dict(sha256=e.sha(b'x')), 'invalid withheld payload'),
            ('withheld wrong size', native, dict(size=1), 'invalid withheld payload'),
            ('withheld not true', native, dict(withheld='yes'), 'third-party library bytes stored'),
        ]:
            with self.subTest(label):
                with self.assertRaisesRegex(e.Invalid, message):
                    e.compare(golden, self.parser_copy(source, label, **kwargs), contract)
        # A JVM record cannot use the stand-in: only a native run does.
        with self.assertRaisesRegex(e.Invalid, 'worker/shared/libxctest-parser.dylib'):
            e.compare(self.parser_copy(golden, 'jvm stand-in', e.PARSER_STAND_IN),
                      self.parser_copy(native, 'stand-in 2', e.PARSER_STAND_IN), contract)

    def test_d20_withholding_is_limited_to_the_parser_path_of_macos_cases(self):
        self.tmp_root = Path(tempfile.mkdtemp(dir=self.root))
        golden = DRIVER['golden']('macos-smoke')
        contract = DRIVER['contract']('macos-smoke')
        record = json.loads((golden / 'evidence.json').read_bytes())
        other = next(r for r in record['entries'] if r['path'] == 'state.json')
        other['withheld'] = True
        copy = self.tmp_root / 'other-path'
        shutil.copytree(golden, copy)
        (copy / 'evidence.json').write_bytes(e.encoded(record))
        with self.assertRaisesRegex(e.Invalid, 'invalid withheld payload'):
            e.validate(copy, contract)
        self.assertFalse(e.parser_scope(DRIVER['contract']('ios-smoke'), e.PARSER_PATH))
        self.assertFalse(e.parser_scope(dict(scenario='parse'), e.PARSER_PATH))
        self.assertFalse(e.parser_scope(contract, 'worker/shared/other.dylib'))

    def test_d20_a_capture_keeps_the_digest_and_drops_the_bytes(self):
        entries = [dict(path=e.PARSER_PATH, sha256=e.PARSER_DIGEST), dict(path='other', sha256=e.sha(b'o'))]
        payloads = {e.PARSER_DIGEST: b'real', e.sha(b'o'): b'o'}
        e.withhold_parser(entries, payloads)
        self.assertIs(entries[0]['withheld'], True)
        self.assertNotIn('withheld', entries[1])
        self.assertEqual(list(payloads), [e.sha(b'o')])
        stand_in = [dict(path=e.PARSER_PATH, sha256=e.sha(e.PARSER_STAND_IN))]
        kept = {e.sha(e.PARSER_STAND_IN): e.PARSER_STAND_IN}
        e.withhold_parser(stand_in, kept)
        self.assertNotIn('withheld', stand_in[0])
        self.assertIn(e.sha(e.PARSER_STAND_IN), kept)

    def test_d20_transcript_redaction_touches_only_the_parser_transfer(self):
        import vendor_transport as t
        def row(seq, event, connection, data):
            return dict(channel=2, connection=connection, detail=dict(base64=base64.b64encode(data).decode(),
                        sha256=e.sha(data), size=len(data)), event=event, run='macos-smoke', schema=1, seq=seq)
        parser = b"/usr/bin/rsync --server -vlogDtpre.iLsfxCIvu --partial . /tmp/marathon/shared/libxctest-parser.dylib"
        other = b"/usr/bin/rsync --server -vlogDtpre.iLsfxCIvu --partial . /tmp/marathon/shared/other"
        rows = [row(0, 'exec', 1, parser), row(1, 'stdin', 1, b'library bytes'), row(2, 'output', 1, b'server reply'),
                row(3, 'exec', 2, other), row(4, 'stdin', 2, b'other bytes')]
        data = b''.join(json.dumps(r, separators=(',',':'), sort_keys=True).encode() + b'\n' for r in rows)
        redacted = t.redact_parser_transfer(data)
        before, after = data.splitlines(), redacted.splitlines()
        self.assertEqual([a == b for a, b in zip(before, after)], [True, False, True, True, True])
        self.assertNotIn(base64.b64encode(b'library bytes'), redacted)
        changed = json.loads(after[1])
        self.assertEqual(t.payload(changed['detail']), bytes(13))
        self.assertEqual(changed['detail']['redacted'], 'D20')
        self.assertEqual(t.redact_parser_transfer(redacted), redacted)

    def test_d20_a_jvm_record_with_library_bytes_in_its_transcript_is_rejected(self):
        golden = DRIVER['golden']('macos-smoke')
        contract = DRIVER['contract']('macos-smoke')
        copy = self.root / 'unredacted'
        shutil.copytree(golden, copy)
        record = json.loads((copy / 'evidence.json').read_bytes())
        row = next(r for r in record['entries'] if r['path'] == 'transcript.jsonl')
        rows = [json.loads(line) for line in e.payload(copy, row, record).splitlines()]
        chunk = next(r for r in rows if r['detail'].get('redacted'))
        del chunk['detail']['redacted']
        fake = b'x' * chunk['detail']['size']
        chunk['detail'].update(base64=base64.b64encode(fake).decode(), sha256=e.sha(fake))
        data = b''.join(json.dumps(r, separators=(',',':'), sort_keys=True).encode() + b'\n' for r in rows)
        (copy / 'blobs' / row['sha256']).unlink()
        row.update(sha256=e.sha(data), size=len(data))
        e.save_payload(copy, row['sha256'], data, record)
        (copy / 'evidence.json').write_bytes(e.encoded(record))
        with self.assertRaisesRegex(e.Invalid, 'third-party library bytes in the transcript of a recorded run'):
            e.validate(copy, contract)

    def test_d20_committed_records_hold_no_copy_of_the_library(self):
        digests = {'680185323898795897641a932fb4b5b4481206429b6472db1a4ba317598cb782',
                   'a82ca9e3d6259ac95f526b797371a6bd59771f7213fb67a4a011f30cbfd9c327',
                   e.PARSER_DIGEST,
                   '9e0020168274d4adc2c3f5e1cbd0c114932dfd62e57f5f88a37299df0a8aa149',
                   '25eda5f7e59ebcb57f8787241836e339b0cc02a9f5df57b71d374f50421bacc3'}
        count = 0
        for path in (ROOT / 'fixtures/differential').rglob('evidence.json'):
            record = json.loads(path.read_bytes())
            blobs = {p.name for p in (path.parent / 'blobs').iterdir()}
            self.assertFalse(blobs & digests, path)
            for row in record['entries']:
                if row['path'] == e.PARSER_PATH:
                    count += 1
                    self.assertIs(row.get('withheld'), True, path)
        self.assertEqual(count, 32)

    def rewrite_transcript(self, source, name, change, **provenance):
        """A copy of a committed record with `change` applied to the rows of its transcript, digests recomputed."""
        destination = self.root / name
        shutil.copytree(source, destination)
        record = json.loads((destination / 'evidence.json').read_bytes())
        self.as_live_native(destination, record)
        row = next(r for r in record['entries'] if r['path'] == 'transcript.jsonl')
        rows = [json.loads(line) for line in e.payload(destination, row, record).splitlines()]
        change(rows)
        data = b''.join(json.dumps(r, separators=(',',':'), sort_keys=True).encode() + b'\n' for r in rows)
        (destination / 'blobs' / row['sha256']).unlink()
        row.update(sha256=e.sha(data), size=len(data))
        e.save_payload(destination, row['sha256'], data, record)
        record['provenance'].update(provenance)
        (destination / 'evidence.json').write_bytes(e.encoded(record))
        return destination

    @staticmethod
    def set_payload(row, data):
        row['detail'].update(base64=base64.b64encode(data).decode(), sha256=e.sha(data), size=len(data))

    @classmethod
    def fill_marked(cls, rows):
        """Sol's reproduction: every marked chunk holds nonzero bytes again, keeping its marker and a valid digest."""
        marked = [r for r in rows if r['detail'].get('redacted')]
        assert marked
        for row in marked:
            cls.set_payload(row, b'x' * row['detail']['size'])

    NATIVE = ROOT / 'fixtures/differential/vendor-verification-c95b2128/macos-smoke/native'

    def test_d20_a_live_native_capture_of_the_real_library_is_refused(self):
        library = b'bytes of the library in this test only\n'
        tree = self.root / 'worker-tree'
        (tree / 'shared').mkdir(parents=True)
        (tree / 'shared/libxctest-parser.dylib').write_bytes(library)
        contract = DRIVER['contract']('macos-smoke')
        with mock.patch.object(e, 'PARSER_DIGEST', e.sha(library)):
            with self.assertRaisesRegex(e.Invalid, 'native run pushed the real'):
                e.write_record(self.root / 'native-real', contract, 'native', {'worker': tree}, {}, contract['outcome'],
                               synthetic_provenance(), {})
        self.assertEqual(list((self.root / 'native-real/blobs').iterdir()), [])

    def live_native(self, name, change):
        """A record shaped like a new native capture: the stand-in on the worker, and `change` on its transcript."""
        self.tmp_root = Path(tempfile.mkdtemp(dir=self.root))
        source = self.parser_copy(self.NATIVE, name, e.PARSER_STAND_IN)
        return self.rewrite_transcript(source, name + '-live', change)

    @staticmethod
    def parser_chunks(rows):
        import vendor_transport as t
        keys = t.parser_transfer_keys(rows)
        return [r for r in rows if r['event'] == 'stdin' and (r['connection'], r['channel']) in keys]

    def test_d20_a_live_native_transcript_may_hold_the_stand_in_transfer_only(self):
        contract = DRIVER['contract']('macos-smoke')
        def small(rows):
            for row in self.parser_chunks(rows):
                del row['detail']['redacted']
                self.set_payload(row, b's')
        e.compare(DRIVER['golden']('macos-smoke'), self.live_native('small', small), contract)
        def library_sized(rows):
            for row in self.parser_chunks(rows):
                del row['detail']['redacted']
                self.set_payload(row, b'x' * row['detail']['size'])
        with self.assertRaisesRegex(e.Invalid, 'library bytes in the parser transfer'):
            e.validate(self.live_native('large', library_sized), contract)
        def marked(rows):
            for row in self.parser_chunks(rows):
                self.set_payload(row, bytes(1))
        with self.assertRaisesRegex(e.Invalid, 'redacted row in a new native transcript'):
            e.validate(self.live_native('marked', marked), contract)

    def test_d20_marked_chunks_with_nonzero_bytes_are_rejected(self):
        golden, contract = DRIVER['golden']('macos-smoke'), DRIVER['contract']('macos-smoke')
        filled = self.rewrite_transcript(golden, 'filled-jvm', self.fill_marked)
        with self.assertRaisesRegex(e.Invalid, 'not zero-filled'):
            e.validate(filled, contract)
        with self.assertRaisesRegex(e.Invalid, 'not zero-filled'):
            e.compare(golden, filled, contract)
        # A native copy is no longer the pinned record once changed, so it fails even earlier.
        filled = self.rewrite_transcript(self.NATIVE, 'filled-native', self.fill_marked)
        with self.assertRaisesRegex(e.Invalid, 'withheld payload in a new native record'):
            e.compare(golden, filled, contract)

    def test_d20_the_redaction_marker_is_exact_and_belongs_to_the_parser_transfer(self):
        import vendor_transport as t
        def row(seq, event, connection, data, **detail):
            return dict(channel=2, connection=connection, detail=dict(base64=base64.b64encode(data).decode(),
                        sha256=e.sha(data), size=len(data), **detail), event=event, run='macos-smoke', schema=1, seq=seq)
        parser = b"/usr/bin/rsync --server -vlogDtpre.iLsfxCIvu --partial . /tmp/marathon/shared/libxctest-parser.dylib"
        other = b"/usr/bin/rsync --server -vlogDtpre.iLsfxCIvu --partial . /tmp/marathon/shared/other"
        def lines(*rows):
            return b''.join(json.dumps(r, separators=(',',':'), sort_keys=True).encode() + b'\n' for r in rows)
        good = lines(row(0, 'exec', 1, parser), row(1, 'stdin', 1, bytes(4), redacted='D20'))
        self.assertEqual(t.redact_parser_transfer(good), good)
        for label, bad, message in [
            ('other marker', lines(row(0, 'exec', 1, parser), row(1, 'stdin', 1, bytes(4), redacted='yes')), 'redaction marker'),
            ('empty marker', lines(row(0, 'exec', 1, parser), row(1, 'stdin', 1, bytes(4), redacted='')), 'redaction marker'),
            ('nonzero', lines(row(0, 'exec', 1, parser), row(1, 'stdin', 1, b'real', redacted='D20')), 'not zero-filled'),
            ('other session', lines(row(0, 'exec', 2, other), row(1, 'stdin', 2, bytes(4), redacted='D20')), 'outside the parser transfer'),
            ('other event', lines(row(0, 'exec', 1, parser), row(1, 'output', 1, bytes(4), redacted='D20')), 'outside the parser transfer'),
        ]:
            with self.subTest(label), self.assertRaisesRegex(e.Invalid, message):
                t.redact_parser_transfer(bad)

    @classmethod
    def add_parser_stream(cls, rows, data, extended=None):
        """Sol's reproduction: `data` in 31-byte rows of the parser transfer's output, sequence numbers kept contiguous."""
        last = max(i for i, row in enumerate(rows) if row in cls.parser_chunks(rows))
        added = []
        for start in range(0, len(data), 31):
            row = json.loads(json.dumps(rows[last]))
            row['event'] = 'output'
            row['detail'].pop('redacted', None)
            cls.set_payload(row, data[start:start + 31])
            if extended is not None:
                row['detail']['extended'] = extended
            added.append(row)
        rows[last + 1:last + 1] = added
        for seq, row in enumerate(rows):
            row['seq'] = seq

    def test_d20_library_sized_output_of_the_parser_transfer_is_refused(self):
        contract = DRIVER['contract']('macos-smoke')
        golden = DRIVER['golden']('macos-smoke')
        library_sized = b'L' * e.PARSER_SIZE
        def small_chunks(rows):
            for row in self.parser_chunks(rows):
                del row['detail']['redacted']
                self.set_payload(row, b's')
        for extended in (None, 1):
            def new_native(rows):
                small_chunks(rows)
                self.add_parser_stream(rows, library_sized, extended)
            def recorded(rows):
                self.add_parser_stream(rows, library_sized, extended)
            for label, record in [('new native', self.live_native(f'out-{extended}', new_native)),
                                  ('jvm', self.rewrite_transcript(golden, f'jvm-out-{extended}', recorded))]:
                with self.subTest(label, extended=extended):
                    with self.assertRaisesRegex(e.Invalid, 'library bytes in the parser transfer'):
                        e.validate(record, contract)
                    with self.assertRaisesRegex(e.Invalid, 'library bytes in the parser transfer'):
                        e.compare(golden, record, contract)
        # Under the bound it is ordinary rsync protocol traffic.
        def small_output(rows):
            small_chunks(rows)
            self.add_parser_stream(rows, b'p' * 1024)
        e.compare(golden, self.live_native('small-out', small_output), contract)

    def test_d20_a_capture_is_refused_before_anything_is_stored(self):
        contract = DRIVER['contract']('macos-smoke')
        golden = DRIVER['golden']('macos-smoke')
        record = e.load(golden / 'evidence.json')
        row = next(r for r in record['entries'] if r['path'] == 'transcript.jsonl')
        rows = [json.loads(line) for line in e.payload(golden, row, record).splitlines()]
        self.add_parser_stream(rows, b'L' * e.PARSER_SIZE)
        data = b''.join(json.dumps(r, separators=(',',':'), sort_keys=True).encode() + b'\n' for r in rows)
        tree = self.root / 'capture-tree'
        tree.mkdir()
        for implementation in ('native', 'jvm'):
            destination = self.root / ('refused-' + implementation)
            with self.subTest(implementation), self.assertRaisesRegex(e.Invalid, 'parser transfer|redacted row'):
                e.write_record(destination, contract, implementation, {'worker': tree}, {'transcript.jsonl': data},
                               contract['outcome'], synthetic_provenance(), {})
            self.assertEqual(list((destination / 'blobs').iterdir()), [])

    def test_d20_a_modified_native_record_cannot_claim_a_recorded_commit(self):
        self.tmp_root = Path(tempfile.mkdtemp(dir=self.root))
        contract = DRIVER['contract']('macos-smoke')
        golden = DRIVER['golden']('macos-smoke')
        # The provenance still names the allowlisted commit in both.
        withheld = self.rewrite_transcript(self.NATIVE, 'claims-withheld', lambda rows: None, note='changed')
        self.assertIn(e.load(withheld / 'evidence.json')['provenance']['native_sha'],
                      [e.load(ROOT / 'fixtures/differential' / name / 'manifest.json')['native_sha']
                       for name in ('vendor-verification', 'vendor-verification-c95b2128')])
        with self.assertRaisesRegex(e.Invalid, 'withheld payload in a new native record'):
            e.compare(golden, withheld, contract)
        redacted = self.parser_copy(self.NATIVE, 'claims-redacted', e.PARSER_STAND_IN)
        with self.assertRaisesRegex(e.Invalid, 'redacted row in a new native transcript'):
            e.compare(golden, redacted, contract)
        # Every unmodified recorded native still validates, wherever it is copied.
        for name in ('vendor-verification', 'vendor-verification-c95b2128'):
            for case in ('ignore', 'parse', 'pass', 'smoke'):
                copy = self.root / f'{name}-{case}'
                shutil.copytree(ROOT / 'fixtures/differential' / name / f'macos-{case}/native', copy)
                self.assertTrue(e.may_withhold(copy, e.load(copy / 'evidence.json')), copy)

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
        connected = ''.join('D 00:00:00.000 [worker] <AdamDeviceProvider> Device emulator-%d changed state to CONNECTED\n' % n
                            for n in (5554, 5556))
        parsed = 'I 00:00:00.000 [worker] <Timings> Parsed 36 tests in 0 ms (+0 ms since start)\n'
        self.assertEqual(console.normalize((timing+connected+parsed).encode(),v,'native','stdout'),
                         console.normalize((connected+timing+parsed).encode(),v,'native','stdout'))
        self.assertNotEqual(console.normalize((timing+connected+parsed).encode(),v,'native','stdout'),
                            console.normalize((timing+parsed+connected).encode(),v,'native','stdout'))
        self.assertNotEqual(console.normalize(b'first\nsecond\n',v,'native','stdout'),
                            console.normalize(b'second\nfirst\n',v,'native','stdout'))


if __name__ == '__main__':
    unittest.main()
