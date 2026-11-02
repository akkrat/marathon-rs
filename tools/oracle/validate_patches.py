#!/usr/bin/env python3
"""WP 0.2b validation. Evidence is disposable, under tools/oracle/build/validation; never replay goldens."""
import argparse
import json
import os
import shutil
import socket
import subprocess
import time
import xml.etree.ElementTree as ET
from pathlib import Path

from compare_replay import compare_console, compare_reference, exact, files, compare_adb

ROOT = Path(__file__).resolve().parents[2]
ORACLE = ROOT/'tools/oracle'
SOURCE_SHA = os.environ.get('ORACLE_SOURCE_SHA', 'e47352f25cbc50333900d5d040a5544ff1224501')
if SOURCE_SHA not in ('e47352f25cbc50333900d5d040a5544ff1224501', 'c95b212882c7ba7b1ad91b7981916ed175f1530e'):
    raise ValueError('unsupported ORACLE_SOURCE_SHA')
BUILD = ORACLE/('build' if SOURCE_SHA.startswith('e47352f2') else 'build-'+SOURCE_SHA)
EXCLUDED = 'com.malinskiy.marathon.apple.cmd.remote.ssh.sshj.SshjCommandExecutorTest'


def run(args, log, cwd=ROOT, env=None, timeout=900, expected=0):
    started = time.monotonic()
    log.parent.mkdir(parents=True, exist_ok=True)
    with log.open('wb') as output:
        result = subprocess.run([str(a) for a in args], cwd=cwd, env=env, stdout=output, stderr=subprocess.STDOUT, timeout=timeout)
    if result.returncode != expected:
        raise AssertionError(f'{args[0]} exited {result.returncode}, expected {expected}: {log}')
    return round(time.monotonic()-started, 3)


class Validation:
    def __init__(self, args):
        self.args = args
        self.root = BUILD/'validation'
        self.root.mkdir(parents=True, exist_ok=True)
        self.work = self.root/'work'
        self.result = {'format': 'marathon-replay/validation@1', 'runs': args.runs, 'excluded_test_classes': [EXCLUDED], 'checks': {}}
        self.env = {k: v for k, v in os.environ.items() if k not in ('JAVA_OPTS','JAVA_TOOL_OPTIONS','JDK_JAVA_OPTIONS','ORACLE_OPTS','MARATHON_OPTS')}
        self.env.update(TZ='UTC', LANG='en_US.UTF-8', LC_ALL='en_US.UTF-8')
        with socket.socket() as s:
            s.bind(('127.0.0.1', 0))
            self.port = s.getsockname()[1]

    def binary(self, variant, cli=False):
        clone = BUILD/('marathon-patched' if variant == 'patched' else 'marathon-src')
        return clone/('cli/build/install/marathon/bin/marathon' if cli else 'oracle-build/install/oracle/bin/oracle')

    def fresh(self):
        shutil.rmtree(self.work, ignore_errors=True)
        (self.work/'tmp').mkdir(parents=True)
        (self.work/'home').mkdir()
        (self.work/'captured').mkdir()

    def java_env(self, tape=None):
        opts = [f'-Djava.io.tmpdir={self.work}/tmp', '-Duser.timezone=UTC', '-Duser.language=en', '-Duser.country=US',
                '-XX:+EnableDynamicAgentLoading', '-XX:-UseCompressedOops', '-Xmx512m',
                '-Dlogback.statusListenerClass=ch.qos.logback.core.status.NopStatusListener']
        if tape:
            opts += [f'-Doracle.hooks.tape={tape}', f'-Doracle.hooks.report={self.work}/hooks-report.json']
        return self.env | {'JAVA_OPTS': ' '.join(opts), 'HOME': str(self.work/'home'), 'TMPDIR': str(self.work/'tmp')}

    def execute(self, args, env, expected):
        with (self.work/'captured/stdout').open('wb') as out, (self.work/'captured/stderr').open('wb') as err:
            result = subprocess.run([str(a) for a in args], cwd=self.work, env=env, stdout=out, stderr=err, timeout=120)
        (self.work/'captured/exit-code').write_text(str(result.returncode)+'\n')
        if result.returncode != expected:
            raise AssertionError(f'expected exit {expected}, got {result.returncode}: {self.work}/captured')
        report = self.work/'hooks-report.json'
        if report.exists():
            shutil.copyfile(report, self.work/'captured/hooks-report.json')
            value = json.loads(report.read_text())
            if value['failures'] or value['unused']:
                raise AssertionError(f'unconsumed or failed tape: {value}')

    def scenario(self, spec, variant):
        self.fresh()
        tape = ORACLE/'scenario-tape.json' if variant == 'patched' else None
        if variant == 'patched' and spec.stem == 'random-order': tape = ORACLE/'random-order-tape.json'
        source = spec
        if variant == 'reference' and json.loads(spec.read_text()).get('entry') == 'run':
            value = json.loads(spec.read_text()); value['entry'] = 'runAsync'
            source = self.work/'scenario.json'; source.write_text(json.dumps(value))
        self.execute([self.binary(variant), 'scenario', source, self.work/'captured/run'], self.java_env(tape), 0)

    def cli(self, variant, taped=True):
        self.fresh()
        source = ROOT/'fixtures/fake-adb/android-two-devices'
        shutil.copytree(source/'in', self.work/'in')
        for apk in ['app-debug.apk','app-debug-androidTest.apk']:
            shutil.copyfile(ROOT/'fixtures/vendor-android/apk'/apk, self.work/'in'/apk)
        marathonfile = self.work/'in/Marathonfile'
        # A documented input variant; originals are copied unchanged. Control log-only executor identities through configuration.
        marathonfile.write_text(marathonfile.read_text().replace('  type: "Android"',
            '  type: "Android"\n  threadingConfiguration:\n    bootWaitingThreads: 1')+'\ndebug: false\n')
        tape = json.loads((ORACLE/'scenario-tape.json').read_text())
        tape['clocks'] = {'wall': {'mode':'frozen','start':'2020-01-01T00:00:00Z'},
                          'monotonic': {'mode':'frozen','startNanos':0}, 'scheduler': {'mode':'real'}}
        tape['ids'].pop('test.ConfigurationFactory.outputDir')
        tape['barriers'] = ['android.AdamDeviceProvider.connected:emulator-5554','android.AdamDeviceProvider.connected:emulator-5556',
            f'core.DeviceActor.prepare:127.0.0.1:{self.port}:emulator-5554', f'core.DeviceActor.prepare:127.0.0.1:{self.port}:emulator-5556',
            f'core.DeviceActor.execute:127.0.0.1:{self.port}:emulator-5554']
        tape_path = self.work/'tape.json'; tape_path.write_text(json.dumps(tape,indent=2)+'\n')
        with (self.work/'fake.stdout').open('wb') as out, (self.work/'fake.stderr').open('wb') as err:
            fake = subprocess.Popen([str(ROOT/'target/debug/fake-adb'),'--script',str(source/'script/devices.yaml'),
                '--port',str(self.port),'--port-file',str(self.work/'port'),'--transcript',str(self.work/'adb.jsonl'),'--strict'],stdout=out,stderr=err)
        try:
            for _ in range(500):
                if (self.work/'port').exists(): break
                if fake.poll() is not None: raise AssertionError((self.work/'fake.stderr').read_text())
                time.sleep(.02)
            else: raise TimeoutError('fake-adb did not start')
            env = self.java_env(tape_path if variant == 'patched' and taped else None) | {'FAKE_ADB_PORT': str(self.port)}
            self.execute([self.binary(variant, cli=True), '-m', marathonfile], env, 1)
        finally:
            fake.terminate()
            if fake.wait(timeout=10) != 0:
                raise AssertionError('fake-adb failed strict mode: '+(self.work/'fake.stderr').read_text())
        shutil.copytree(self.work/'in/output', self.work/'captured/output')
        raw = json.loads((self.work/'captured/output/test_result/raw.json').read_text())
        assert sorted(r['status'] for r in raw) == ['FAILURE','IGNORED','PASSED'], raw
        # Retain every run's raw transport evidence and compare logical requests/payload digests.
        shutil.copyfile(self.work/'adb.jsonl', self.work/'captured/adb.jsonl')
        # Raw evidence lives with this run; logical comparison controls only documented transport interleavings.
        from compare_replay import adb_snapshot
        (self.work/'captured/adb-logical.json').write_text(json.dumps(adb_snapshot(self.work/'captured/adb.jsonl'), sort_keys=True)+'\n')

    def archive(self, name):
        target = self.root/name
        shutil.rmtree(target,ignore_errors=True)
        shutil.copytree(self.work/'captured',target)
        return target

    def check_a(self):
        env = self.env.copy()
        # Remove old XML and disable task/build caches: counts must describe this invocation.
        for path in (BUILD/'marathon-patched').rglob('TEST-*.xml'):
            if '/test-results/test/' in str(path): path.unlink()
        elapsed = run(['./gradlew','--console=plain','--rerun-tasks','--no-build-cache','test',f'-PexcludeTests={EXCLUDED}'],self.root/'a-unit-tests.log',
                      cwd=BUILD/'marathon-patched',env=env)
        counts = {'tests':0,'failures':0,'errors':0,'skipped':0}
        for p in (BUILD/'marathon-patched').rglob('TEST-*.xml'):
            if '/test-results/test/' not in str(p): continue
            root=ET.parse(p).getroot()
            for key in counts: counts[key]+=int(root.get(key,'0'))
        assert counts['tests'] > 0 and counts['failures'] == counts['errors'] == 0, counts
        self.result['checks']['a'] = counts | {'seconds':elapsed}

    def check_b(self):
        started = time.monotonic(); rows=[]
        for spec in sorted((ORACLE/'scenarios').glob('*.json')):
            for i in range(self.args.runs):
                self.scenario(spec,'patched')
                if i == 0: first=self.archive('b-'+spec.stem)
                else: exact(first,self.work/'captured')
            rows.append({'scenario':spec.stem,'files':len(files(first)), 'runs':self.args.runs})
            print(f'b: {spec.stem}: {self.args.runs} exact runs',flush=True)
        for i in range(self.args.runs):
            self.cli('patched')
            if i == 0: first=self.archive('b-cli')
            else:
                compare_adb(first, self.work/'captured')
                exact_without_adb(first, self.work/'captured')
            self.archive(f'b-cli-run-{i+1:02d}')
        rows.append({'scenario':'CLI android-two-devices','files':len(files(first)),'runs':self.args.runs})
        coverage = {}
        for path in self.root.glob('b-*/run/transcript.json'):
            for label, count in json.loads(path.read_text())['hooksReport']['calls'].items():
                coverage[label] = coverage.get(label, 0) + count
        cli_report = json.loads((self.root/'b-cli/hooks-report.json').read_text())
        for label, count in cli_report['calls'].items(): coverage[label] = coverage.get(label, 0) + count
        self.result['tape_coverage'] = dict(sorted(coverage.items()))
        (self.root/'tape-coverage.json').write_text(json.dumps(self.result['tape_coverage'], indent=2)+'\n')
        self.result['checks']['b'] = {'seconds':round(time.monotonic()-started,3),'scenarios':rows}

    def check_c(self):
        started=time.monotonic(); rows=[]
        for spec in sorted((ORACLE/'scenarios').glob('*.json')):
            # Always regenerate the pair, so --parts c cannot compare stale output from another revision.
            self.scenario(spec,'patched'); patched=self.archive('c-'+spec.stem+'-patched')
            self.scenario(spec,'reference'); reference=self.archive('c-'+spec.stem+'-reference')
            result=compare_reference(patched/'run',reference/'run',self.root/'diffs'/spec.stem)
            compare_console(patched,reference,self.root/'diffs'/spec.stem)
            rows.append({'scenario':spec.stem}|result)
            print(f'c: {spec.stem}: report tree and event transcript match',flush=True)
        self.cli('patched'); patched=self.archive('c-cli-patched')
        self.cli('reference'); reference=self.archive('c-cli-reference')
        result=compare_reference(patched,reference,self.root/'diffs/cli',cli=True)
        compare_console(patched,reference,self.root/'diffs/cli',cli=True)
        compare_adb(patched, reference, role_mapping=True)
        rows.append({'scenario':'CLI android-two-devices'}|result)
        self.cli('patched', taped=False); patched=self.archive('c-cli-no-tape-patched')
        self.cli('reference', taped=False); reference=self.archive('c-cli-no-tape-reference')
        result=compare_reference(patched,reference,self.root/'diffs/cli-no-tape',cli=True)
        compare_console(patched,reference,self.root/'diffs/cli-no-tape',cli=True)
        compare_adb(patched,reference,role_mapping=True)
        rows.append({'scenario':'CLI no tape'}|result)
        self.result['checks']['c']={'seconds':round(time.monotonic()-started,3),'scenarios':rows}

    def main(self):
        try:
            if not self.args.skip_build:
                self.result['build_seconds']=run([ORACLE/'build.sh'],self.root/'build.log',env=self.env)
            run(['cargo','build','-q','-p','fake-adb'],self.root/'fake-adb-build.log',env=self.env)
            run(['python3','-m','unittest','discover','-s',ORACLE,'-p','test_compare_replay.py'],self.root/'comparator-tests.log')
            run(['python3',ORACLE/'hook_inventory.py','check',BUILD/'marathon-src',BUILD/'marathon-patched'],self.root/'inventory.log')
            for part in self.args.parts.split(','):
                getattr(self,'check_'+part)()
            self.result['passed']=True
        except Exception as error:
            self.result['passed']=False; self.result['error']=str(error)
            raise
        finally:
            (self.root/'results.json').write_text(json.dumps(self.result,indent=2)+'\n')
        print(json.dumps(self.result,indent=2))


def exact_without_adb(a, b):
    aa, bb = files(a), files(b)
    for data in (aa, bb): data.pop('adb.jsonl')
    assert aa == bb, 'CLI byte mismatch (excluding raw connection interleaving)'


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--runs',type=int,default=10)
    parser.add_argument('--parts',default='a,b,c')
    parser.add_argument('--skip-build',action='store_true')
    args=parser.parse_args()
    if args.runs < 1 or any(p not in ('a','b','c') for p in args.parts.split(',')): parser.error('invalid runs or parts')
    Validation(args).main()


if __name__=='__main__': main()
