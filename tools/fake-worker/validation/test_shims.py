import json
import os
import pathlib
import subprocess
import sys
import tempfile
import unittest

SHIM = pathlib.Path(__file__).resolve().parents[1]/'shims/dispatch.py'

class ShimTest(unittest.TestCase):
    def invoke(self, root, args):
        return subprocess.run([sys.executable, str(SHIM), 'ssh', *args], env={**os.environ, 'FW_REAL_SSH':'/bin/echo', 'FW_ARGV_LOG':str(root/'argv.jsonl')}, capture_output=True)

    def test_isolation_precedes_original_args(self):
        with tempfile.TemporaryDirectory() as d:
            root=pathlib.Path(d)
            args=['-F','/dev/null','fixture@127.0.0.1','probe']
            result=self.invoke(root,args)
            self.assertEqual(result.returncode,0)
            row=json.loads((root/'argv.jsonl').read_text())
            self.assertEqual(row['argv'],args)
            self.assertEqual(result.stdout.decode().strip().split(),row['harness_options']+args)

    def test_protected_caller_options_are_rejected(self):
        with tempfile.TemporaryDirectory() as d:
            root=pathlib.Path(d)
            for option in ['UserKnownHostsFile','GlobalKnownHostsFile','UpdateHostKeys','IdentityAgent','IdentitiesOnly']:
                for args in [['-o',option+'=bad'],['-o'+option+'=bad'],['-o',option.lower()+' bad']]:
                    with self.subTest(args=args):
                        result=self.invoke(root,args+['fixture@127.0.0.1'])
                        self.assertNotEqual(result.returncode,0)
                        self.assertIn(b'caller must not override',result.stderr)
            self.assertFalse((root/'argv.jsonl').exists())

if __name__=='__main__': unittest.main()
