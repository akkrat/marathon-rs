#!/usr/bin/env python3
"""Run the full pinned Kotlin CLI against a private loopback Apple worker."""
import argparse
import apple_evidence as evidence
import datetime
import difflib
import json
from pathlib import Path
import select
import shutil
import signal
import subprocess
import zipfile

ROOT = Path(__file__).resolve().parents[2]
JDK = '/Applications/Android Studio.app/Contents/jbr/Contents/Home'


def write(path, value):
    path.write_text(json.dumps(value, indent=2) + '\n')


def build_worker():
    build = subprocess.run(['cargo', 'build', '-q', '-p', 'fake-worker', '--manifest-path', str(ROOT / 'Cargo.toml'),
                            '--message-format=json'], cwd=ROOT, check=True, stdout=subprocess.PIPE, text=True)
    artifacts = [json.loads(line) for line in build.stdout.splitlines()]
    executables = [a['executable'] for a in artifacts if a.get('reason') == 'compiler-artifact'
                   and a['target']['name'] == 'fake-worker' and a.get('executable')]
    if len(executables) != 1:
        raise RuntimeError('cargo did not report exactly one fake-worker executable')
    return Path(executables[0]).resolve()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('scenario')
    parser.add_argument('--update', action='store_true')
    args = parser.parse_args()
    scenario = Path(args.scenario)
    if not scenario.is_dir():
        scenario = ROOT / 'fixtures/fake-worker/apple' / args.scenario
    executable = build_worker()
    spec = json.loads((scenario / 'script/world.json').read_text())
    name = spec['name']
    ident = 'apple-' + name + '-' + datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%f')
    work = ROOT / 'tools/fake-worker/runs' / ident
    work.mkdir(parents=True)
    run = ROOT / 'tools/oracle/runs' / ident
    result = {'schema': 2, 'suite': name, 'terminal_outcome': 'running', 'required_artifacts': evidence.REQUIRED}
    write(work / 'manifest.json', result)
    worker = None
    try:
        inp = work / 'in'
        shutil.copytree(scenario / 'in', inp)
        # Repack only real fixture entries, byte for byte. Kotlin extracts both archives
        # at parse time. No Python extraction changes bundle bytes or worker paths.
        app = 'AppleWorld.app' if spec['platform'] == 'macos' else 'sample-app.app'
        test = ('AppleWorld.app/Contents/PlugIns/AppleWorldTests.xctest' if spec['platform'] == 'macos'
                else 'sample-appUITests-Runner.app' if name == 'ios-media'
                else 'sample-app.app/PlugIns/sample-appUnitTests.xctest')
        with zipfile.ZipFile(ROOT / 'fixtures/bundles' / (spec['platform'] + '.zip')) as source:
            for filename, prefix in [('app.zip', app), ('tests.zip', test)]:
                with zipfile.ZipFile(inp / filename, 'w', zipfile.ZIP_DEFLATED) as dst:
                    for entry in source.infolist():
                        if entry.filename.startswith(prefix + '/'):
                            # Remove the original app ancestors from standalone unit bundles.
                            entry.filename = entry.filename[len(prefix.rsplit('/', 1)[0]) + 1:] if '/' in prefix else entry.filename
                            dst.writestr(entry, source.read(entry.orig_filename))
        for key in ['host-key', 'client-key']:
            subprocess.run(['/usr/bin/ssh-keygen', '-q', '-t', 'ed25519', '-N', '', '-f', str(work / key)], check=True)
        config = dict(listen='127.0.0.1:0', root=str(work / 'root'), transcript=str(work / 'transcript.jsonl'), host_key=str(work / 'host-key'),
                      auth=dict(user='fixture', password=None, authorized_keys=[str(work / 'client-key.pub')], password_auth=False, keyboard_interactive=False),
                      max_sessions=16, rsync_binary='/usr/bin/rsync', fixtures=str(ROOT / 'fixtures/fake-worker'), run=name)
        write(work / 'config.json', config)
        worker = subprocess.Popen([str(executable), '--config', str(work / 'config.json'), '--apple', str(scenario / 'script/world.json'), '--state', str(work / 'state.json')], stdout=subprocess.PIPE, stderr=(work / 'worker.stderr').open('wb'))
        if not select.select([worker.stdout], [], [], 15)[0]:
            raise RuntimeError('worker readiness timeout')
        ready = json.loads(worker.stdout.readline())
        write(work / 'ready.json', ready)
        port = ready['endpoint'].rsplit(':', 1)[1]
        public = (work / 'host-key.pub').read_text().split()
        known = work / 'known_hosts'
        known.write_text(f'[127.0.0.1]:{port} {public[0]} {public[1]}\n')
        shims = ROOT / 'tools/fake-worker/shims'
        env = dict(JAVA_HOME=JDK, FW_FIXTURE_KEY=str(work / 'client-key'), FW_KNOWN_HOSTS=str(known), FW_PORT=port,
                   PATH=str(shims) + ':/usr/bin:/bin:/usr/sbin:/sbin', RSYNC4J_RSYNC=str(shims / 'rsync'),
                   FW_REAL_RSYNC='/usr/bin/rsync', FW_REAL_SSH='/usr/bin/ssh', FW_ARGV_LOG=str(work / 'host-argv.jsonl'))
        command = [str(ROOT / 'tools/oracle/run-isolated.sh'), '--id', ident, '--timeout', '120']
        for k, v in env.items():
            command += ['--env', k + '=' + v]
        command += ['--', str(ROOT / 'tools/oracle/build/marathon-src/cli/build/install/marathon/bin/marathon'), str(inp), 'run', '-m', '@IN@/Marathonfile', '--analyticsTracking', 'false', '--bugsnag', 'false']
        subprocess.run(command, cwd=ROOT, check=True, timeout=150)
        manifest = json.loads((run / 'manifest.json').read_text())
        result['exit_code'] = manifest['exitCode']
        result['timed_out'] = manifest['timedOut']
        for f in ['stdout', 'stderr']:
            shutil.copy2(run / f, work / f)
        result['terminal_outcome'] = 'recorded'
    except BaseException as error:
        result['terminal_outcome'] = 'failed'
        result['error'] = str(error)
        raise
    finally:
        if worker is not None:
            if worker.poll() is None:
                worker.send_signal(signal.SIGTERM)
            try:
                result['worker_exit'] = worker.wait(timeout=15)
            except subprocess.TimeoutExpired:
                worker.kill()
                worker.wait()
                result['worker_exit'] = 'timeout'
        write(work / 'manifest.json', result)
        print('Apple evidence:', work)
    try:
        assert not result.get('timed_out') and result.get('worker_exit') == 0, (work / 'worker.stderr').read_text()
        evidence.collect(work, run, spec)
        evidence.seal(work)
        evidence.validate(work, spec)
        expected = scenario / 'expected'
        if args.update:
            # Only new recordings replace reviewable goldens, after validation.
            if expected.exists():
                shutil.move(expected, work / 'previous-expected')
            evidence.curate(work, expected / 'raw')
            evidence.normalize(expected / 'raw',expected / 'normalized')
        else:
            evidence.validate(expected / 'raw', spec)
            evidence.compare(work,expected / 'raw')
            evidence.normalize(work, work / 'normalized')
            assert evidence.load(work / 'normalized/ready.json') == evidence.load(expected / 'normalized/ready.json'), 'readiness replay differs'
            actual=(work / 'semantic.json').read_text()
            golden=(expected / 'normalized/semantic.json').read_text()
            if actual!=golden:
                raise AssertionError('semantic replay differs:\n'+''.join(difflib.unified_diff(golden.splitlines(True),actual.splitlines(True),fromfile='expected',tofile='actual')))
        print('Completed:', name, evidence.load(work / 'semantic.json')['junit'])
    except BaseException as error:
        result = evidence.load(work / 'manifest.json')
        result.update(terminal_outcome='failed', error=str(error))
        write(work / 'manifest.json', result)
        raise


if __name__ == '__main__':
    main()
