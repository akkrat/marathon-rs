#!/usr/bin/env python3
import json
import pathlib
import subprocess
import sys
import tempfile
import unittest
import evidence
from suite_plan import inventory


def complete_source(root):
    root.mkdir()
    plan=inventory('faults','darwin')
    for name in plan['required_artifacts']:
        path=root/name;path.parent.mkdir(parents=True,exist_ok=True)
        if name.endswith('transcript.jsonl'):
            path.write_text(json.dumps({'event':'cleanup','detail':{'outcome':'completed'}})+'\n')
        elif name.endswith('readiness.json'):
            path.write_text(json.dumps({'endpoint':'127.0.0.1:1234'}))
        elif path.suffix=='.json':path.write_text('{}')
        else:path.write_bytes(b'')
    manifest={'schema':2,'suite':'faults','platform_id':'darwin','terminal_outcome':'completed',**plan,'results':[{'case':case,'outcome':'completed'} for case in plan['expected_cases']]}
    (root/'results.json').write_text(json.dumps(manifest))
    return manifest


class EvidenceTest(unittest.TestCase):
    def test_incomplete_evidence_is_never_equal(self):
        with tempfile.TemporaryDirectory() as d:
            root=pathlib.Path(d);source=root/'source';manifest=complete_source(source)
            curated=root/'curated';evidence.curate(source,curated)
            self.assertTrue(evidence.compare(curated,curated))
            path=curated/'raw/results.json'
            for outcome in ['running','failed','timeout','skipped',None]:
                path.write_text(json.dumps({**manifest,'terminal_outcome':outcome}))
                self.assertFalse(evidence.compare(curated,curated))
            for rows in [[],manifest['results'][:1],[{'outcome':'completed'}]]:
                path.write_text(json.dumps({**manifest,'results':rows}))
                self.assertFalse(evidence.compare(curated,curated))
            for outcome in ['failed','timeout','skipped']:
                rows=[{**row,'outcome':outcome} for row in manifest['results']]
                path.write_text(json.dumps({**manifest,'results':rows}))
                self.assertFalse(evidence.compare(curated,curated))
            path.write_text(json.dumps({'results':[{'outcome':'completed'}]}))
            self.assertFalse(evidence.compare(curated,curated))
            path.write_text(json.dumps({**manifest,'expected_cases':manifest['expected_cases'][:1]}))
            self.assertFalse(evidence.compare(curated,curated))

    def test_required_transcripts_and_artifacts_cannot_be_missing_or_unfinished(self):
        with tempfile.TemporaryDirectory() as d:
            root=pathlib.Path(d);source=root/'source';complete_source(source)
            curated=root/'curated';evidence.curate(source,curated)
            for variant in ['raw','normalized']:
                for name in ['reject/transcript.jsonl','exec-rejection.stdout']:
                    path=curated/variant/name;original=path.read_bytes();path.unlink()
                    self.assertFalse(evidence.compare(curated,curated))
                    path.write_bytes(original)
                path=curated/variant/'reject/transcript.jsonl';original=path.read_bytes()
                for data in [b'',b'{"event":"ready"}\n',b'{"event":"cleanup","detail":{"outcome":"failed"}}\n']:
                    path.write_bytes(data);self.assertFalse(evidence.compare(curated,curated))
                path.write_bytes(original)
            self.assertTrue(evidence.compare(curated,curated))

    def test_invalid_utf8_streams_compare_unequal_and_binary_roundtrips(self):
        with tempfile.TemporaryDirectory() as d:
            root=pathlib.Path(d)
            for label,payload in [('left',b'\x80'),('right',b'\x81')]:
                source=root/(label+'-source');complete_source(source)
                (source/'exec-rejection.stdout').write_bytes(payload)
                binary=bytes(range(256))+b'\r\n/run/1234\x00'
                (source/'exec-rejection.stderr').write_bytes(binary)
                evidence.curate(source,root/label)
                for variant in ['raw','normalized']:
                    self.assertEqual((root/label/variant/'exec-rejection.stdout').read_bytes(),payload)
                    self.assertEqual((root/label/variant/'exec-rejection.stderr').read_bytes(),binary)
                self.assertTrue(evidence.compare(root/label,root/label))
            self.assertFalse(evidence.compare(root/'left',root/'right'))

    def test_prerequisite_and_worker_startup_exceptions_are_recorded(self):
        with tempfile.TemporaryDirectory() as d:
            for name,java in [('prerequisite','/nonexistent-fixture-java'),('startup','/usr/bin/true')]:
                output=pathlib.Path(d)/name
                result=subprocess.run([sys.executable,str(pathlib.Path(__file__).with_name('run.py')),'--only','standard','--output',str(output),'--java',java,'--rsync','/usr/bin/true','--sshpass','/usr/bin/true','--worker','/nonexistent-fixture-worker'],capture_output=True,timeout=20)
                self.assertNotEqual(result.returncode,0)
                manifest=json.loads((output/'results.json').read_text())
                self.assertEqual(manifest['terminal_outcome'],'failed')
                self.assertTrue(any(row['case']=='runner-exception' and row['outcome']=='failed' for row in manifest['results']))
                self.assertFalse(evidence.completed(output/'results.json'))

    def test_normalization_preserves_opaque_bytes_and_references(self):
        original={'argv':['/run/key','-p','1234',"ssh -p 1234 "],'base64':'/run/1234','path':'/tmp/marathon/file','pid':42}
        pids={};a=evidence.normalize(original,'/run',{'1234':'<port>'},pids);b=evidence.normalize({'pid':42},'/run',{},pids)
        self.assertEqual(a['base64'],original['base64']);self.assertEqual(a['path'],original['path']);self.assertEqual(a['pid'],b['pid']);self.assertEqual(a['argv'],['<run>/key','-p','<port>',"ssh -p <port> "])

if __name__=='__main__':unittest.main()
