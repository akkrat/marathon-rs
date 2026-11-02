#!/usr/bin/env python3
"""Runner-overhead measurement: Rust native CLI vs the Kotlin JVM CLI.

Both sides run the identical deterministic fake-worker / fake-ADB scenarios that
tools/differential (WP 7.1) drives, each run under `/usr/bin/time -l`. The
shipped launchers are measured with their defaults (no extra JVM flags); the
7.1 oracle hooks/tapes are NOT used because they exist only in the patched JVM
build. A run is a valid sample only if it exits with the scenario's exit code and
its normalised report observation equals the first valid observation of the
scenario on either side (see `observe`). Writes raw JSON (every sample) and a
summary JSON; docs/perf/runner-overhead.md is written from these by hand.

  python3 -I tools/perf/runner_overhead.py --summarise <results.json> <summary.json>   # slim summary + tables
  python3 -I tools/perf/runner_overhead.py --jvm-bin <installDist>/bin/marathon \
      --native target/release/marathon --fake-worker target/release/fake-worker \
      --fake-adb target/release/fake-adb --output tools/perf/runs/new
"""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import re
import select
import shutil
import signal
import statistics
import subprocess
import sys
import tempfile
import threading
import time

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'tools/differential'))
import evidence as e  # noqa: E402
import vendors  # noqa: E402

DEFAULT_JDK = '/Applications/Android Studio.app/Contents/jbr/Contents/Home'
SCENARIOS = {
    'startup-version': dict(kind='startup', exit=0, what='`marathon version` (no config, no devices)'),
    'android-smoke': dict(kind='vendor', case='android-smoke', what='Android, 1 fake ADB device, 1 test'),
    'android-two-devices': dict(kind='vendor', case='android-two-devices',
                                what='Android, 2 fake ADB devices, 3 tests (pass/fail/ignored)'),
    'ios-smoke': dict(kind='vendor', case='ios-smoke', what='iOS, fake SSH worker, 3 tests, screenshots'),
    'macos-smoke': dict(kind='vendor', case='macos-smoke', what='macOS, fake SSH worker, 3 tests'),
}
UUID = re.compile(r'[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}')
LOAD_LIMIT = 4.0


def percentile(values, q):
    """Nearest-rank on the sorted samples; with 10 samples p90 is the 9th value."""
    ordered = sorted(values)
    return ordered[max(0, math.ceil(q * len(ordered)) - 1)]


def parse_time_l(text):
    """Fields of macOS `/usr/bin/time -l`; sizes are bytes."""
    out = {}
    m = re.search(r'([\d.]+)\s+real\s+([\d.]+)\s+user\s+([\d.]+)\s+sys', text)
    out.update(time_real=float(m[1]), time_user=float(m[2]), time_sys=float(m[3]))
    for key, label in [('max_rss', 'maximum resident set size'), ('instructions', 'instructions retired'),
                       ('cycles', 'cycles elapsed'), ('footprint', 'peak memory footprint'),
                       ('vol_ctx', 'voluntary context switches'), ('invol_ctx', 'involuntary context switches')]:
        m = re.search(r'^\s*(\d+)\s+' + label + r'\s*$', text, re.M)
        out[key] = int(m[1]) if m else None
    return out


def wait_load(limit=LOAD_LIMIT, patience=300):
    """Idle gate: wait for the 1-minute load average to fall below `limit`."""
    deadline = time.monotonic() + patience
    while True:
        load = os.getloadavg()[0]
        if load < limit:
            return load
        if time.monotonic() > deadline:
            raise SystemExit(f'load average stayed >= {limit} for {patience}s (last {load:.2f}); aborting')
        time.sleep(5)


def timed(argv, cwd, env, work, timeout=120):
    """One measured process under /usr/bin/time -l; also wait4 rusage (microseconds)."""
    work.mkdir(parents=True)
    tfile = work / 'time.txt'
    with (work / 'stdout').open('wb') as out, (work / 'stderr').open('wb') as err:
        start = time.perf_counter()
        proc = subprocess.Popen(['/usr/bin/time', '-l', '-o', str(tfile), *map(str, argv)], cwd=cwd, env=env,
                                stdin=subprocess.DEVNULL, stdout=out, stderr=err, start_new_session=True)
        watchdog = threading.Timer(timeout, lambda: os.killpg(proc.pid, signal.SIGKILL))
        watchdog.start()
        try:
            _, status, ru = os.wait4(proc.pid, 0)
        finally:
            watchdog.cancel()
        wall = time.perf_counter() - start
    proc.returncode = os.waitstatus_to_exitcode(status)
    try:
        os.killpg(proc.pid, signal.SIGKILL)  # stray grandchildren, if any
    except ProcessLookupError:
        pass
    sample = parse_time_l(tfile.read_text())
    sample.update(exit=proc.returncode, wall=wall, user=ru.ru_utime, sys=ru.ru_stime,
                  cpu=ru.ru_utime + ru.ru_stime, rss_wait4=ru.ru_maxrss)
    return sample


def normalise(text, sandbox, port):
    text = text.replace(str(sandbox), '{sandbox}')
    if port:
        text = text.replace(str(port), '{port}')
    # With two devices the scheduler's choice of the executing device is not
    # controlled here (7.1 pins it with taped barriers); fold the serials.
    return re.sub(r'emulator-\d+', 'emulator-N', UUID.sub('{uuid}', text))


def observe(report, sandbox, port):
    """Output-equality witness: the report tree's paths and per-test outcomes.

    Weaker than 7.1's byte comparator (that needs the patched JVM's tapes), so
    it is deliberately structural: the sorted normalised file list plus each
    test's status from test_result/*.json.
    """
    paths, tests = [], []
    for p in sorted(report.rglob('*')):
        rel = normalise(str(p.relative_to(report)), sandbox, port)
        paths.append(rel + ('/' if p.is_dir() else ''))
        if (p.is_file() and p.suffix == '.json' and len(p.relative_to(report).parts) == 4
                and p.relative_to(report).parts[0] == 'test_result'):  # test_result/<pool>/<device>/<test>.json; raw.json is skipped
            data = json.loads(p.read_text())
            test = data['test']
            tests.append((f"{test['pkg']}.{test['clazz']}#{test['method']}", data['status']))
    paths.sort()
    return dict(paths=paths, tests=sorted(map(list, tests)))


def digest(obs):
    return hashlib.sha256(json.dumps(obs, sort_keys=True).encode()).hexdigest()


class Scenario:
    """Prepares one fresh sandbox + fake per run; the CLI binary is a parameter."""

    def __init__(self, name, binaries, keys):
        self.name = name
        self.spec = SCENARIOS[name]
        self.case = self.spec.get('case')
        self.binaries = binaries
        self.keys = keys

    def run(self, side, root, index):
        work = root / f'{self.name}-{side}-{index}'
        work.mkdir(parents=True)
        env = dict(os.environ, HOME=str(work / 'home'), TMPDIR=str(work / 'tmp'), LANG='en_US.UTF-8',
                   LC_ALL='en_US.UTF-8', TZ='UTC', JAVA_HOME=self.binaries['java_home'])
        for key in ('JAVA_OPTS', 'MARATHON_OPTS', 'DEFAULT_JVM_OPTS'):
            env.pop(key, None)
        (work / 'home').mkdir()
        (work / 'tmp').mkdir()
        binary = self.binaries[side]
        if self.spec['kind'] == 'startup':
            sample = timed([binary, 'version'], work, env, work / 'cli')
            sample['valid'] = sample['exit'] == 0
            sample['observation'] = None
            return sample
        env['PATH'] = '/usr/bin:/bin:/usr/sbin:/sbin'
        env['MRS_ADB_TRIPWIRE'] = str(work / 'adb-invoked')
        fake = None
        handles = []
        port = None
        try:
            fake, port, fake_env, handles = self.start_fake(work, env)
            env.update(fake_env)
            report = work / 'in/report'
            report.mkdir(exist_ok=True)
            sample = timed([binary, 'run', '-m', work / 'in/Marathonfile'], work, env, work / 'cli')
        finally:
            fake_ru = self.stop_fake(fake, handles)
        sample['fake'] = fake_ru
        argv_log = work / 'host-argv.jsonl'  # ssh/rsync shim invocations (Apple only), shared cost
        sample['shim_calls'] = len(argv_log.read_bytes().splitlines()) if argv_log.exists() else 0
        ok = (sample['exit'] == vendors.CASES[self.case]['exit'] and fake_ru['exit'] == 0
              and not (work / 'adb-invoked').exists() and report.is_dir())
        obs = observe(report, work, port) if ok else None
        sample['valid'] = ok
        sample['observation'] = obs
        sample['observation_sha256'] = digest(obs) if obs else None
        return sample

    # -- fake worker / ADB (same setup as tools/differential/vendors.raw_capture) --
    def start_fake(self, work, env):
        handles = []
        if vendors.apple(self.case):
            vendors.prepare_apple(self.case, work)
            for key in ('host-key', 'client-key'):
                shutil.copyfile(self.keys / key, work / key)
                os.chmod(work / key, 0o600)
                shutil.copyfile(self.keys / (key + '.pub'), work / (key + '.pub'))
            config = dict(listen='127.0.0.1:0', root=str(work / 'root'), transcript=str(work / 'transcript.jsonl'),
                          host_key=str(work / 'host-key'),
                          auth=dict(user='fixture', password=None, authorized_keys=[str(work / 'client-key.pub')],
                                    password_auth=False, keyboard_interactive=False),
                          max_sessions=32, rsync_binary='/usr/bin/rsync',
                          fixtures=str(ROOT / 'fixtures/fake-worker'), run=self.case)
            vendors.save(work / 'worker-config.json', config)
            err = (work / 'fake.stderr').open('wb')
            handles.append(err)
            fake = subprocess.Popen([self.binaries['fake-worker'], '--config', work / 'worker-config.json', '--apple',
                                     vendors.world_input(self.case), '--state', work / 'state.json'],
                                    stdout=subprocess.PIPE, stderr=err, start_new_session=True, umask=0o022)
            e.require(select.select([fake.stdout], [], [], 15)[0], 'worker readiness timeout')
            ready = json.loads(fake.stdout.readline())
            port = ready['endpoint'].rsplit(':', 1)[1]
            public = (work / 'host-key.pub').read_text().split()
            (work / 'known_hosts').write_text(f'[127.0.0.1]:{port} {public[0]} {public[1]}\n')
            shims = ROOT / 'tools/fake-worker/shims'
            fake_env = dict(FW_FIXTURE_KEY=str(work / 'client-key'), FW_KNOWN_HOSTS=str(work / 'known_hosts'),
                            FW_PORT=port, PATH=str(shims) + ':' + env['PATH'], RSYNC4J_RSYNC=str(shims / 'rsync'),
                            FW_REAL_RSYNC='/usr/bin/rsync', FW_REAL_SSH='/usr/bin/ssh',
                            FW_ARGV_LOG=str(work / 'host-argv.jsonl'))
            return fake, port, fake_env, handles
        script = vendors.prepare_adb(work, self.case)
        out = (work / 'fake.stdout').open('wb')
        err = (work / 'fake.stderr').open('wb')
        handles += [out, err]
        fake = subprocess.Popen([self.binaries['fake-adb'], '--script', script, '--port', '0', '--port-file',
                                 work / 'port', '--transcript', work / 'transcript.jsonl', '--state',
                                 work / 'state.json', '--strict'],
                                stdout=out, stderr=err, start_new_session=True, umask=0o022)
        port = vendors.wait_file(work / 'port', fake).strip()
        e.require(port != '5037', 'refuse standard adb port')
        self.write_android_config(work, port)
        return fake, port, {}, handles

    def write_android_config(self, work, port):
        vendor = dict(type='Android', androidSdk='./sdk', applicationApk='./app-debug.apk',
                      testApplicationApk='./app-debug-androidTest.apk', autoGrantPermission=False,
                      mockLocation=False, applicationPmClear=False, testApplicationPmClear=False,
                      allureConfiguration=dict(enabled=False),
                      screenRecordConfiguration=dict(videoConfiguration=dict(enabled=False),
                                                     screenshotConfiguration=dict(enabled=False)),
                      adbServers=[dict(host='127.0.0.1', port=int(port))])
        config = dict(name='perf-android', outputDir='./report', debug=False, analyticsTracking=False,
                      bugsnagReporting=False, uncompletedTestRetryQuota=0,
                      filteringConfiguration=dict(allowlist=[dict(type='fully-qualified-test-name',
                                                                  values=['com.example.MainActivityTest#testText'])]),
                      vendorConfiguration=vendor)
        if self.case == 'android-two-devices':
            config['filteringConfiguration']['allowlist'][0]['values'] += [
                'com.example.MainActivityAllureTest#testFailure', 'com.example.ClassIgnoredTest#testAlwaysIgnored']
            config['batchingStrategy'] = dict(type='fixed-size', size=10)
        vendors.save(work / 'in/Marathonfile', config)

    def stop_fake(self, fake, handles):
        result = dict(exit=None)
        if fake is not None:
            if fake.poll() is None:
                fake.send_signal(signal.SIGTERM)
            ru = None
            if fake.returncode is None:  # still ours to reap: wait4 gives its rusage
                _, status, ru = os.wait4(fake.pid, 0)
                fake.returncode = os.waitstatus_to_exitcode(status)
            result = dict(exit=fake.returncode)
            if ru is not None:
                result.update(user=ru.ru_utime, sys=ru.ru_stime, cpu=ru.ru_utime + ru.ru_stime,
                              max_rss=ru.ru_maxrss)
            if fake.stdout:
                fake.stdout.close()
        for handle in handles:
            handle.close()
        return result


METRICS = [('wall', 's'), ('cpu', 's'), ('user', 's'), ('sys', 's'), ('max_rss', 'bytes'),
           ('footprint', 'bytes'), ('instructions', ''), ('cycles', '')]


def stats(values):
    return dict(n=len(values), median=statistics.median(values), p90=percentile(values, .9),
                min=min(values), max=max(values), mean=statistics.fmean(values))


def summarise(raw):
    summary = {}
    for name, sides in raw['samples'].items():
        row = {}
        for side, samples in sides.items():
            valid = [s for s in samples if s['valid']]
            row[side] = dict(valid=len(valid), invalid=len(samples) - len(valid),
                             **{m: stats([s[m] for s in valid if s.get(m) is not None]) for m, _ in METRICS
                                if any(s.get(m) is not None for s in valid)})
        if 'jvm' in row and 'native' in row:
            row['ratio_jvm_over_native'] = {
                m: {k: row['jvm'][m][k] / row['native'][m][k] for k in ('median', 'p90')
                    if row['native'][m][k]} for m, _ in METRICS if m in row['jvm'] and m in row['native']}
        digests = {s.get('observation_sha256') for side in sides.values() for s in side if s['valid']}
        row['observation_digests'] = sorted(d for d in digests if d)
        row['outputs_equal_across_sides'] = len(row['observation_digests']) <= 1
        summary[name] = row
    return summary


def shim_cost(scratch):
    """Wall time of one tools/fake-worker/shims/rsync call (python3 start-up + exec of `rsync --version`).

    Apple scenarios route every ssh/rsync child through this shim on both sides;
    its cost is inside the CLI's wait4 rusage and wall time, so report it.
    """
    shims = ROOT / 'tools/fake-worker/shims'
    env = dict(PATH='/usr/bin:/bin', FW_REAL_RSYNC='/usr/bin/rsync', FW_ARGV_LOG=str(scratch / 'shim-argv.jsonl'))
    samples = []
    for _ in range(20):
        start = time.perf_counter()
        subprocess.run([str(shims / 'rsync'), '--version'], env=env, stdout=subprocess.DEVNULL, check=True)
        samples.append(time.perf_counter() - start)
    return dict(per_call_wall=stats(samples))


def hashes(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def machine():
    def sysctl(key):
        return subprocess.run(['sysctl', '-n', key], capture_output=True, text=True).stdout.strip()
    return dict(cpu=sysctl('machdep.cpu.brand_string'), cores=int(sysctl('hw.ncpu')),
                performance_cores=int(sysctl('hw.perflevel0.logicalcpu') or 0),
                efficiency_cores=int(sysctl('hw.perflevel1.logicalcpu') or 0),
                ram_bytes=int(sysctl('hw.memsize')), macos=subprocess.run(['sw_vers', '-productVersion'], capture_output=True,
                                                                          text=True).stdout.strip(),
                build=subprocess.run(['sw_vers', '-buildVersion'], capture_output=True, text=True).stdout.strip(),
                arch=platform.machine(), python=platform.python_version())


def verify_receipt(jvm_bin):
    """The JVM receipt's jar hashes must still match the installed lib/."""
    install = Path(jvm_bin).resolve().parents[1]
    build = install.parents[4]  # <build>/marathon-src/cli/build/install/marathon -> <build>
    receipt = json.loads((build / 'receipt-reference.json').read_text())
    lib = install / 'lib'
    actual = {p.name: hashes(p) for p in sorted(lib.glob('*.jar'))}
    bad = {k: v for k, v in receipt['jars'].items() if actual.get(k) != v}
    extra = sorted(set(actual) - set(receipt['jars']))
    if bad or extra:
        raise SystemExit(f'receipt mismatch: {sorted(bad)} extra={extra}')
    return dict(receipt=build.name + '/receipt-reference.json', source=receipt['source'],
                variant=receipt['variant'], jars_verified=len(receipt['jars']),
                jars_total_sha256=hashlib.sha256(json.dumps(actual, sort_keys=True).encode()).hexdigest())


def java_version(java_home):
    out = subprocess.run([str(Path(java_home) / 'bin/java'), '-version'], capture_output=True, text=True).stderr
    flags = subprocess.run([str(Path(java_home) / 'bin/java'), '-XX:+PrintFlagsFinal', '-version'],
                           capture_output=True, text=True).stdout
    keep = {}
    for key in ('MaxHeapSize', 'InitialHeapSize', 'UseG1GC', 'UseSerialGC', 'CICompilerCount', 'TieredStopAtLevel',
                'UseCompressedOops', 'ActiveProcessorCount'):
        m = re.search(r'^\s*\w+\s+' + key + r'\s+=\s+(\S+)', flags, re.M)
        if m:
            keep[key] = m[1]
    return out.strip(), keep


def fmt(metric, value):
    if metric in ('wall', 'cpu'):
        return f'{value * 1000:.0f} ms' if value < 1 else f'{value:.2f} s'
    if metric in ('max_rss', 'footprint'):
        return f'{value / 2**20:.1f} MiB'
    return f'{value / 1e9:.2f} G' if value >= 1e9 else f'{value / 1e6:.0f} M'


def markdown(raw):
    """Results tables (median [p90] per side, ratio of medians) for docs/perf/runner-overhead.md."""
    lines = []
    for metric, title in [('wall', 'wall time (real)'), ('cpu', 'CPU time, user+sys'), ('max_rss', 'max RSS'),
                          ('footprint', 'peak memory footprint'), ('instructions', 'instructions retired')]:
        lines += [f'**{title}**: median [p90] (min-max)', '',
                  '| scenario | JVM | native | JVM/native (median) | JVM/native (p90) |', '|---|---|---|---|---|']
        for name, row in raw['summary'].items():
            j, n, r = row['jvm'][metric], row['native'][metric], row['ratio_jvm_over_native'][metric]
            cell = lambda x: (f"{fmt(metric, x['median'])} [{fmt(metric, x['p90'])}] "
                              f"({fmt(metric, x['min'])}-{fmt(metric, x['max'])})")
            lines.append(f"| {name} | {cell(j)} | {cell(n)} | {r['median']:.1f}x | {r['p90']:.1f}x |")
        lines.append('')
    return '\n'.join(lines)


def summarise_file(results, summary):
    raw = json.loads(Path(results).read_text())
    slim = {k: v for k, v in raw.items() if k != 'samples'}
    slim['samples_file'] = 'tools/perf/runs/<run>/results.json (not committed, >200 KB)'
    slim['max_load_before_run'] = max(s['load_before'] for n in raw['samples'].values()
                                      for side in n.values() for s in side)
    Path(summary).write_text(json.dumps(slim, indent=1, sort_keys=True) + '\n')
    print(markdown(raw))


def main():
    if len(sys.argv) == 4 and sys.argv[1] == '--summarise':  # <results.json> <summary.json>
        return summarise_file(sys.argv[2], sys.argv[3])
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--jvm-bin', type=Path, required=True)
    parser.add_argument('--native', type=Path, required=True)
    parser.add_argument('--fake-worker', type=Path, required=True)
    parser.add_argument('--fake-adb', type=Path, required=True)
    parser.add_argument('--java-home', default=os.environ.get('JAVA_HOME', DEFAULT_JDK))
    parser.add_argument('--runs', type=int, default=10)
    parser.add_argument('--warmups', type=int, default=1)
    parser.add_argument('--scenarios', nargs='*', default=list(SCENARIOS))
    parser.add_argument('--output', type=Path, required=True, help='new directory for sandboxes and raw logs')
    parser.add_argument('--results', type=Path, help='raw JSON (all samples) + summary; default <output>/results.json')
    args = parser.parse_args()
    os.umask(0o022)
    args.output = args.output.resolve()
    args.output.mkdir(parents=True, exist_ok=False)
    binaries = {'jvm': str(args.jvm_bin.resolve()), 'native': str(args.native.resolve()),
                'fake-worker': str(args.fake_worker.resolve()), 'fake-adb': str(args.fake_adb.resolve()),
                'java_home': args.java_home}
    jdk, jvm_flags = java_version(args.java_home)
    receipt = verify_receipt(args.jvm_bin)
    keys = args.output / 'keys'
    keys.mkdir()
    for key in ('host-key', 'client-key'):
        subprocess.run(['/usr/bin/ssh-keygen', '-q', '-t', 'ed25519', '-N', '', '-f', str(keys / key)], check=True)
    raw = dict(format='marathon-perf/runner-overhead@1', machine=machine(), java=jdk, jvm_default_flags=jvm_flags,
               jvm_launcher_default_opts='DEFAULT_JVM_OPTS="" (no JAVA_OPTS / MARATHON_OPTS set by the harness)',
               jvm_receipt=receipt,
               native_sha256=hashes(args.native), fake_worker_sha256=hashes(args.fake_worker),
               fake_adb_sha256=hashes(args.fake_adb),
               native_commit=subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip(),
               tree_dirty=bool(subprocess.check_output(['git', 'status', '--porcelain', '--', 'crates', 'Cargo.lock'],
                                                       cwd=ROOT, text=True).strip()),
               runs=args.runs, warmups=args.warmups, load_limit=LOAD_LIMIT,
               scenarios={n: SCENARIOS[n]['what'] for n in args.scenarios}, samples={}, load_before=[])
    work = args.output / 'work'
    reference = {}
    for name in args.scenarios:
        scenario = Scenario(name, binaries, keys)
        raw['samples'][name] = {'jvm': [], 'native': []}
        for side in ('jvm', 'native'):  # warm-ups: discarded, but their exit/output is still checked
            for i in range(args.warmups):
                wait_load()
                warm = scenario.run(side, work, f'warm{i}')
                if not warm['valid']:
                    raise SystemExit(f'{name}/{side} warm-up invalid (exit {warm["exit"]}); see {work}')
                reference.setdefault(name, warm.get('observation_sha256'))
                if warm.get('observation_sha256') != reference[name]:
                    raise SystemExit(f'{name}/{side} warm-up output differs from the first observation')
        for i in range(args.runs):
            for side in (('jvm', 'native') if i % 2 == 0 else ('native', 'jvm')):  # alternate who goes first
                for attempt in range(3):
                    load = wait_load()
                    sample = scenario.run(side, work, f'{i}' + ('' if attempt == 0 else f'.retry{attempt}'))
                    if sample['valid'] and sample.get('observation_sha256') != reference[name]:
                        sample['valid'] = False
                    sample.update(load_before=load, index=i, attempt=attempt)
                    raw['samples'][name][side].append(sample)
                    if sample['valid']:
                        break
                # a sandbox is only kept if it was invalid; delete the valid ones to bound disk use
                if sample['valid']:
                    shutil.rmtree(work / f'{name}-{side}-{i}', ignore_errors=True)
        print(f'{name}: done', flush=True)
    raw['shim_cost'] = shim_cost(keys)
    raw['summary'] = summarise(raw)
    results = args.results or args.output / 'results.json'
    results.write_text(json.dumps(raw, indent=1, sort_keys=True))
    print('shim call wall median %.3fs' % raw['shim_cost']['per_call_wall']['median'])
    for name, row in raw['summary'].items():
        j, n = row['jvm'], row['native']
        print(f"{name:22s} wall med jvm {j['wall']['median']:.3f}s native {n['wall']['median']:.3f}s "
              f"(valid {j['valid']}/{n['valid']}, equal={row['outputs_equal_across_sides']})")


if __name__ == '__main__':
    try:
        main()
    except (e.Invalid, OSError, ValueError, subprocess.SubprocessError) as error:
        print(str(error), file=sys.stderr)
        sys.exit(1)
