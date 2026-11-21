#!/usr/bin/env python3
"""Dispatch-gap and report-tail measurement (WP x105) on generated fake-ADB scenarios.

One run = one fake-adb process playing N Android devices plus one `marathon run`. The
scenario is generated: T tests (remote test parser, so no APK of that size is needed),
fixed-size batches of B tests, each test "running" for a scripted time on the fake device.
Batches are deterministic (queue order), so each batch gets its own exact `am instrument`
expectation in strict mode; a batch that differs from the prediction fails the run.

What is measured, all from what the unmodified CLI writes:
  * per-device idle gap at each batch boundary: first test start of the next batch minus
    last test end of the previous batch on the same device (test_result/raw.json, ms);
  * report tail: last test end -> CLI exit, split per reporter by the newest mtime of each
    reporter's output files (reporters run serially in a fixed slot order).
Optional: `--trace` keeps the stderr lines a scratch instrumented build prints (prefix
`X105 `) for a finer breakdown; the shipped build prints none.

Console sinks: `normal` drains the CLI's stdout pipe as fast as it can; `slow` reads 1 KiB
every 50 ms (about 20 KB/s), so the pipe fills and writes block once output is faster.
Metrics: `--graphite` points `analyticsConfiguration` at a local TCP sink that accepts and
discards plaintext metrics (the Graphite read API is on port 80 of the same host and
refuses at once on this machine, so the read side falls back immediately).

  python3 -I tools/perf/dispatch_report.py run --native target/release/marathon \
      --fake-adb target/release/fake-adb --devices 8 --tests 320 --batch 5 \
      --test-ms 100 --console normal --output tools/perf/runs/x105/d8-b5
  python3 -I tools/perf/dispatch_report.py summarise tools/perf/runs/x105 > summary.json
"""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import random
import shutil
import socket
import statistics
import subprocess
import sys
import threading
import time

ROOT = Path(__file__).resolve().parents[2]
LOAD_LIMIT = 4.0
TEST_PACKAGE = 'com.example.test'
RUNNER = 'androidx.test.runner.AndroidJUnitRunner'
PER_CLASS = 50
# Reporter slot order (marathon-core analytics::event_pipeline::ReporterSlot) and the files each writes.
REPORTERS = [
    ('device-info', lambda p: p.startswith('devices/')),
    ('billing', lambda p: p.startswith('bill/')),
    ('junit', lambda p: p.startswith('tests/')),
    ('timeline', lambda p: p.startswith('html/timeline/')),
    ('raw-json', lambda p: p == 'test_result/raw.json'),
    ('test-json', lambda p: p.startswith('test_result/') and p != 'test_result/raw.json'),
    ('allure', lambda p: p.startswith('allure-results/')),
    ('html', lambda p: p.startswith('html/') and not p.startswith('html/timeline/')),
]


def percentile(values, q):
    ordered = sorted(values)
    return ordered[max(0, math.ceil(q * len(ordered)) - 1)]


def tests_of(count):
    return [(f'com.example.perf.Perf{i // PER_CLASS:04d}Test', f'test{i:05d}') for i in range(count)]


def status(fields, code):
    return ''.join(f'INSTRUMENTATION_STATUS: {k}={v}\n' for k, v in fields) + f'INSTRUMENTATION_STATUS_CODE: {code}\n'


def discovery(tests):
    out = []
    for cls, method in tests:
        out.append(status([('class', cls), ('test', method), ('numtests', 1)], 1))
        out.append('INSTRUMENTATION_STATUS: com.malinskiy.adam.junit4.android.listener.TestAnnotationProducer.v4='
                   '[16Lorg.junit.Test()]\nINSTRUMENTATION_STATUS_CODE: 2\n')
        out.append(status([('class', cls), ('test', method)], 0))
    out.append('INSTRUMENTATION_CODE: -1\n')
    return ''.join(out)


def durations(count, test_ms):
    """Per-test fake run time, uniform in [0.5, 1.5] x test_ms with a fixed seed: equal durations would keep the
    devices in lockstep, so that every batch completion collides with all the others."""
    rng = random.Random(105)
    return [round(test_ms * (0.5 + rng.random())) for _ in range(count)]


def execution(batch, sleeps):
    steps = []
    n = len(batch)
    for j, ((cls, method), sleep) in enumerate(zip(batch, sleeps), 1):
        common = [('class', cls), ('current', j), ('id', 'AndroidJUnitRunner'), ('numtests', n)]
        steps.append({'stdout': status(common + [('stream', f'\n{cls}:'), ('test', method)], 1)})
        if sleep:
            steps.append({'sleep_ms': sleep})
        steps.append({'stdout': status(common + [('stream', '.'), ('test', method)], 0)})
    steps.append({'stdout': f'INSTRUMENTATION_RESULT: stream=\n\nTime: 0.1\n\nOK ({n} tests)\n\n\nINSTRUMENTATION_CODE: -1\n'})
    return steps


def instrument_command(batch):
    classes = ','.join(f'{c}#{m}' for c, m in batch)
    return f'am instrument -w -r --no-window-animation -e class {classes} {TEST_PACKAGE}/{RUNNER}'


def generate(work, devices, count, batch_size, test_ms):
    # The runner-cli script is JSON text in a .yaml file; its non-instrumentation rules are reused as they are.
    base = json.loads((ROOT / 'fixtures/fake-adb/android-runner-cli/script/devices.yaml').read_text())
    tests = tests_of(count)
    (work / 'discovery.txt').write_text(discovery(tests))
    rules = []
    for rule in base['shell']:
        if rule.get('regex', '').startswith('^am instrument') or rule.get('prefix') == 'am instrument':
            continue
        if 'stdout_file' in rule:
            rule['stdout_file'] = str((ROOT / 'fixtures/fake-adb/android-runner-cli/script' / rule['stdout_file']).resolve())
        rules.append(rule)
    rules.append({'regex': '^am instrument.* -e log true', 'stdout_file': str(work / 'discovery.txt')})
    batches = [tests[i:i + batch_size] for i in range(0, count, batch_size)]
    sleeps = durations(count, test_ms)
    for k, b in enumerate(batches):
        rules.append({'exact': instrument_command(b), 'times': 1,
                      'output': execution(b, sleeps[k * batch_size:(k + 1) * batch_size])})
    template = base['devices'][0]
    script = {'server': base['server'], 'shell': rules,
              'devices': [dict(template, serial=f'emulator-{5554 + 2 * i}') for i in range(devices)]}
    (work / 'script.json').write_text(json.dumps(script))
    return tests, batches


def marathonfile(inp, port, batch_size, graphite_port):
    vendor = dict(type='Android', androidSdk='./sdk', applicationApk='./app-debug.apk',
                  testApplicationApk='./app-debug-androidTest.apk', autoGrantPermission=False, mockLocation=False,
                  applicationPmClear=False, testApplicationPmClear=False, allureConfiguration=dict(enabled=False),
                  screenRecordConfiguration=dict(videoConfiguration=dict(enabled=False),
                                                 screenshotConfiguration=dict(enabled=False)),
                  testParserConfiguration=dict(type='remote'),
                  adbServers=[dict(host='127.0.0.1', port=int(port))])
    config = dict(name='x105-perf', outputDir='./report', debug=False, analyticsTracking=False, bugsnagReporting=False,
                  uncompletedTestRetryQuota=0, batchingStrategy=dict(type='fixed-size', size=batch_size),
                  vendorConfiguration=vendor)
    if graphite_port:
        config['analyticsConfiguration'] = dict(type='graphite', host='127.0.0.1', port=str(graphite_port), prefix='x105')
    (inp / 'Marathonfile').write_text(json.dumps(config, indent=1))


class Sink(threading.Thread):
    """Drains a pipe: fast, or 1 KiB per 50 ms."""

    def __init__(self, fd, path, slow):
        super().__init__(daemon=True)
        self.fd, self.path, self.slow, self.bytes = fd, path, slow, 0

    def run(self):
        with open(self.path, 'wb') as out:
            while True:
                data = os.read(self.fd, 1024 if self.slow else 65536)
                if not data:
                    break
                self.bytes += len(data)
                out.write(data)
                if self.slow:
                    time.sleep(0.05)
        os.close(self.fd)


class GraphiteSink(threading.Thread):
    def __init__(self):
        super().__init__(daemon=True)
        self.server = socket.socket()
        self.server.bind(('127.0.0.1', 0))
        self.server.listen(16)
        self.port = self.server.getsockname()[1]
        self.bytes = 0
        self.connections = 0

    def run(self):
        while True:
            try:
                conn, _ = self.server.accept()
            except OSError:
                return
            self.connections += 1
            threading.Thread(target=self.drain, args=(conn,), daemon=True).start()

    def drain(self, conn):
        with conn:
            while True:
                data = conn.recv(65536)
                if not data:
                    return
                self.bytes += len(data)


def load1():
    return os.getloadavg()[0]


def wait_idle(limit_s=1800):
    deadline = time.monotonic() + limit_s
    while True:
        load = load1()
        if load < LOAD_LIMIT:
            return load
        if time.monotonic() > deadline:
            raise SystemExit(f'load average stayed at {load:.2f} >= {LOAD_LIMIT} for {limit_s} s; not measuring')
        time.sleep(15)


def sha256(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def one_run(args, work):
    work.mkdir(parents=True)
    for d in ('home', 'tmp', 'in/sdk/platform-tools'):
        (work / d).mkdir(parents=True)
    inp = work / 'in'
    for apk in ('app-debug.apk', 'app-debug-androidTest.apk'):
        shutil.copyfile(ROOT / 'fixtures/vendor-android/apk' / apk, inp / apk)
    shim = inp / 'sdk/platform-tools/adb'
    shim.write_text('#!/bin/sh\nprintf "%s\\n" "$*" >> "$MRS_ADB_TRIPWIRE"\nexit 70\n')
    shim.chmod(0o755)
    tests, batches = generate(work, args.devices, args.tests, args.batch, args.test_ms)
    env = dict(os.environ, HOME=str(work / 'home'), TMPDIR=str(work / 'tmp'), LANG='en_US.UTF-8',
               LC_ALL='en_US.UTF-8', TZ='UTC', PATH='/usr/bin:/bin:/usr/sbin:/sbin',
               MRS_ADB_TRIPWIRE=str(work / 'adb-invoked'))
    if args.trace:
        env['MRS_X105_TRACE'] = '1'
    if args.combined:
        env['MRS_X105_COMBINED'] = '1'
    handles = []
    fake = None
    graphite = None
    try:
        out = (work / 'fake.stdout').open('wb')
        err = (work / 'fake.stderr').open('wb')
        handles += [out, err]
        fake = subprocess.Popen([args.fake_adb, '--script', work / 'script.json', '--port', '0', '--port-file',
                                 work / 'port', '--transcript', work / 'transcript.jsonl', '--strict'],
                                stdout=out, stderr=err, start_new_session=True)
        deadline = time.monotonic() + 30
        while not (work / 'port').exists() or not (work / 'port').read_text().strip():
            if fake.poll() is not None or time.monotonic() > deadline:
                raise SystemExit(f'fake-adb did not start: {work}')
            time.sleep(0.01)
        port = (work / 'port').read_text().strip()
        if args.graphite:
            graphite = GraphiteSink()
            graphite.start()
        marathonfile(inp, port, args.batch, graphite.port if graphite else None)
        load = load1()
        r, w = os.pipe()
        sink = Sink(r, work / 'marathon.stdout', args.console == 'slow')
        sink.start()
        cerr = (work / 'marathon.stderr').open('wb')
        handles.append(cerr)
        start_wall = time.time()
        start = time.perf_counter()
        cli = subprocess.Popen([args.native, '--marathonfile', 'Marathonfile'], cwd=inp, env=env,
                               stdin=subprocess.DEVNULL, stdout=w, stderr=cerr)
        os.close(w)
        # Hang guard only: a stuck run is killed and then fails validity on its exit status.
        guard = threading.Timer(args.timeout, cli.kill)
        guard.start()
        _, status_word, usage = os.wait4(cli.pid, 0)
        guard.cancel()
        cli.returncode = os.waitstatus_to_exitcode(status_word)
        exit_wall = time.time()
        wall = time.perf_counter() - start
        sink.join(60)
    finally:
        if fake is not None and fake.poll() is None:
            fake.terminate()
            try:
                fake.wait(30)
            except subprocess.TimeoutExpired:
                fake.kill()
        for h in handles:
            h.close()
        if graphite is not None:
            graphite.server.close()
    return dict(work=str(work), exit=cli.returncode, fake_exit=fake.returncode, load_before=load,
                start_wall_ms=start_wall * 1000, exit_wall_ms=exit_wall * 1000, wall_s=wall,
                cpu_s=usage.ru_utime + usage.ru_stime, maxrss_bytes=usage.ru_maxrss,
                stdout_bytes=sink.bytes, graphite_bytes=graphite.bytes if graphite else None,
                graphite_connections=graphite.connections if graphite else None,
                adb_tripwire=(work / 'adb-invoked').exists(), batches=[[f'{c}#{m}' for c, m in b] for b in batches])


def analyse(run):
    work = Path(run['work'])
    report = work / 'in/report'
    raw = json.loads((report / 'test_result/raw.json').read_text())
    batch_of = {name: i for i, b in enumerate(run['batches']) for name in b}
    per_device = {}
    for t in raw:
        name = f"{t['package']}.{t['class']}#{t['method']}" if t['package'] else f"{t['class']}#{t['method']}"
        per_device.setdefault(t['deviceSerial'], []).append(
            (t['timestamp'], t['timestamp'] + t['duration'], batch_of.get(name), t['status']))
    gaps = []
    for serial, items in per_device.items():
        items.sort()
        for prev, nxt in zip(items, items[1:]):
            if prev[2] != nxt[2]:
                gaps.append(nxt[0] - prev[1])
    statuses = {}
    for t in raw:
        statuses[t['status']] = statuses.get(t['status'], 0) + 1
    last_end = max(t['timestamp'] + t['duration'] for t in raw)
    first_start = min(t['timestamp'] for t in raw)
    files = []
    for p in report.rglob('*'):
        if p.is_file():
            files.append((str(p.relative_to(report)), p.stat().st_mtime_ns / 1e6))
    tail = {}
    prev = last_end
    unmatched = []
    for name, match in REPORTERS:
        mine = [m for rel, m in files if match(rel)]
        if not mine:
            tail[name] = None
            continue
        done = max(mine)
        tail[name] = dict(files=len(mine), done_after_last_end_ms=done - last_end, step_ms=done - prev)
        prev = max(prev, done)
    for rel, _ in files:
        if not any(match(rel) for _, match in REPORTERS):
            unmatched.append(rel)
    trace = []
    stderr = (work / 'marathon.stderr').read_text(errors='replace')
    for line in stderr.splitlines():
        if line.startswith('X105 '):
            trace.append(line[5:])
    traced = analyse_trace(trace, last_end, run['exit_wall_ms']) if trace else None
    transcript_warnings = sum(1 for line in (work / 'transcript.jsonl').read_text().splitlines()
                              if '"type":"warning"' in line)
    return dict(results=len(raw), statuses=statuses, devices_used=len(per_device), gaps_ms=gaps,
                last_end_to_exit_ms=run['exit_wall_ms'] - last_end, first_start_after_launch_ms=first_start - run['start_wall_ms'],
                last_reporter_to_exit_ms=run['exit_wall_ms'] - prev, reporters=tail, unmatched_files=unmatched[:20],
                fake_warnings=transcript_warnings, traced=traced)


def analyse_trace(lines, last_end_ms, exit_ms):
    """Breakdown from a scratch instrumented build: `<epoch us> <event> <key> <a> <b> <c>`."""
    events = []
    for line in lines:
        us, event, key, a, b, c = line.split(' ')
        events.append((int(us), event, key, int(a), int(b), int(c)))
    events.sort(key=lambda e: e[0])
    by_serial = {}
    for e in events:
        by_serial.setdefault(e[2], []).append(e)
    out = dict(queue_wait_us=[], own_handling_us=[], after_own_us=[], completed_to_exec_us=[],
               flush_us=[], tracker_us=[], te_us=[], observe_us=[])
    for serial, evs in by_serial.items():
        for i, e in enumerate(evs):
            if e[1] == 'te':
                out['flush_us'].append(e[3] + e[4])
                out['tracker_us'].append(e[5])
                out['te_us'].append(e[3] + e[4] + e[5])
            if e[1] != 'pool_completed':
                continue
            rest = evs[i + 1:]
            def first(name):
                return next((x[0] for x in rest if x[1] == name), None)
            start, observed, released, execute = first('q_done_start'), first('q_done_observed'), first('q_done_released'), first('pool_exec')
            if None in (start, released):
                continue
            out['queue_wait_us'].append(start - e[0])
            out['own_handling_us'].append(released - start)
            out['observe_us'].append(observed - start)
            if execute is not None:
                out['after_own_us'].append(execute - released)
                out['completed_to_exec_us'].append(execute - e[0])
    reps = {}
    open_at = {}
    close_start = None
    for us, event, key, *_ in events:
        if event == 'rep_close_start':
            close_start = us
        elif event == 'rep_start':
            open_at[key] = us
        elif event == 'rep_end':
            reps[key.rsplit('::', 1)[-1]] = (us - open_at[key]) / 1000
    last_rep = max((us for us, event, *_ in events if event == 'rep_end'), default=None)
    out = {k: stats(v) for k, v in out.items()}
    out.update(reporters_ms=reps,
               last_end_to_close_start_ms=(close_start / 1000 - last_end_ms) if close_start else None,
               reporters_total_ms=sum(reps.values()),
               last_reporter_to_exit_ms=(exit_ms - last_rep / 1000) if last_rep else None)
    return out


def stats(values):
    if not values:
        return None
    return dict(n=len(values), p50=percentile(values, .5), p95=percentile(values, .95), max=max(values),
                mean=statistics.fmean(values))


def cmd_run(args):
    args.native, args.fake_adb = str(Path(args.native).resolve()), str(Path(args.fake_adb).resolve())
    out = Path(args.output).resolve()
    out.mkdir(parents=True, exist_ok=True)
    meta = dict(argv=sys.argv, machine=platform.platform(), python=platform.python_version(),
                native_sha256=sha256(args.native), fake_adb_sha256=sha256(args.fake_adb),
                devices=args.devices, tests=args.tests, batch=args.batch, test_ms=args.test_ms,
                console=args.console, graphite=args.graphite, trace=args.trace, combined=args.combined, no_gate=args.no_gate)
    samples = []
    for i in range(args.repeat):
        load = wait_idle() if not args.no_gate else load1()
        work = out / f'run-{i:02d}'
        if work.exists():
            raise SystemExit(f'{work} exists')
        run = one_run(args, work)
        run['load_gate'] = load
        if run['exit'] != 0 or run['adb_tripwire']:
            run['invalid'] = 'exit code or adb tripwire'
        a = analyse(run)
        if a['results'] != args.tests or a['statuses'] != {'PASSED': args.tests} or a['fake_warnings']:
            run['invalid'] = f"results {a['results']} {a['statuses']} fake warnings {a['fake_warnings']}"
        run.update(analysis=a)
        del run['batches']
        samples.append(run)
        print(json.dumps(dict(run=i, exit=run['exit'], invalid=run.get('invalid'), wall_s=round(run['wall_s'], 3),
                              load=round(load, 2), gaps=stats(a['gaps_ms']),
                              tail_ms=round(a['last_end_to_exit_ms'], 1))), flush=True)
        if not args.keep:
            for sub in ('in/report', 'transcript.jsonl'):
                p = work / sub
                if p.is_dir():
                    shutil.rmtree(p)
                elif p.exists():
                    p.unlink()
    (out / 'results.json').write_text(json.dumps(dict(meta=meta, samples=samples), indent=1))


def cmd_summarise(args):
    rows = []
    for results in sorted(Path(args.root).rglob('results.json')):
        data = json.loads(results.read_text())
        meta, samples = data['meta'], [s for s in data['samples'] if 'invalid' not in s]
        gaps = [g for s in samples for g in s['analysis']['gaps_ms']]
        reporters = {}
        for s in samples:
            for name, r in s['analysis']['reporters'].items():
                if r:
                    reporters.setdefault(name, []).append(r['step_ms'])
        rows.append(dict(name=str(results.parent.relative_to(args.root)),
                         **{k: meta.get(k) for k in ('devices', 'tests', 'batch', 'test_ms', 'console', 'graphite',
                                                     'trace', 'combined', 'no_gate')},
                         valid=len(samples), invalid=len(data['samples']) - len(samples),
                         load_max=max((s['load_gate'] for s in data['samples']), default=None),
                         gaps_ms=stats(gaps), wall_s=stats([s['wall_s'] for s in samples]),
                         cpu_s=stats([s['cpu_s'] for s in samples]),
                         tail_ms=stats([s['analysis']['last_end_to_exit_ms'] for s in samples]),
                         after_reporters_ms=stats([s['analysis']['last_reporter_to_exit_ms'] for s in samples]),
                         reporters={k: stats(v) for k, v in reporters.items()},
                         native_sha256=meta['native_sha256']))
    json.dump(rows, sys.stdout, indent=1)


def series(item):
    """The x105 measured set: (name, devices, tests, batch, test_ms, console, graphite)."""
    out = []
    if item in ('dispatch', 'dispatch-graphite', 'dispatch-trace'):
        for console, graphite in (('normal', False), ('slow', False), ('normal', True)):
            for devices in (1, 8, 32):
                for batch in (1, 5, 20):
                    if graphite != (item == 'dispatch-graphite') and item != 'dispatch-trace':
                        continue
                    if graphite and devices == 1 and batch != 5:
                        continue
                    if item == 'dispatch-trace' and devices != 32:
                        continue
                    per_device = max(12, math.ceil(48 / devices))
                    out.append((f'd{devices}-b{batch}-{console}' + ('-graphite' if graphite else ''),
                                devices, devices * batch * per_device, batch, 100, console, graphite))
    elif item == 'compare':
        out.append(('d32-b5-normal', 32, 32 * 5 * 12, 5, 100, 'normal', False))
    elif item == 'tail':
        for count in (1000, 10000):
            out.append((f'tail-{count // 1000}k', 8, count, 20, 2, 'normal', False))
    return out


def cmd_matrix(args):
    root = Path(args.output)
    for name, devices, tests, batch, test_ms, console, graphite in series(args.item):
        run = argparse.Namespace(native=args.native, fake_adb=args.fake_adb, devices=devices, tests=tests, batch=batch,
                                 test_ms=test_ms, console=console, graphite=graphite, trace=args.trace,
                                 combined=args.combined, repeat=args.repeat, no_gate=False, keep=False, timeout=1800,
                                 output=str(root / name))
        print(f'# {name}', flush=True)
        cmd_run(run)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest='command', required=True)
    run = sub.add_parser('run')
    run.add_argument('--native', required=True)
    run.add_argument('--fake-adb', required=True)
    run.add_argument('--devices', type=int, required=True)
    run.add_argument('--tests', type=int, required=True)
    run.add_argument('--batch', type=int, required=True)
    run.add_argument('--test-ms', type=int, default=100)
    run.add_argument('--console', choices=['normal', 'slow'], default='normal')
    run.add_argument('--graphite', action='store_true')
    run.add_argument('--trace', action='store_true')
    run.add_argument('--combined', action='store_true',
                     help='set MRS_X105_COMBINED for a scratch prototype build (no effect on a shipped build)')
    run.add_argument('--repeat', type=int, default=3)
    run.add_argument('--timeout', type=float, default=1800, help='kill a CLI run after this many seconds')
    run.add_argument('--no-gate', action='store_true', help='skip the idle gate (trial runs only; recorded)')
    run.add_argument('--keep', action='store_true', help='keep report trees and transcripts')
    run.add_argument('--output', required=True)
    run.set_defaults(func=cmd_run)
    matrix = sub.add_parser('matrix', help='run one of the x105 measured sets')
    matrix.add_argument('item', choices=['dispatch', 'dispatch-graphite', 'dispatch-trace', 'compare', 'tail'])
    matrix.add_argument('--native', required=True)
    matrix.add_argument('--fake-adb', required=True)
    matrix.add_argument('--trace', action='store_true')
    matrix.add_argument('--combined', action='store_true')
    matrix.add_argument('--repeat', type=int, default=3)
    matrix.add_argument('--output', required=True)
    matrix.set_defaults(func=cmd_matrix)
    summ = sub.add_parser('summarise')
    summ.add_argument('root')
    summ.set_defaults(func=cmd_summarise)
    args = parser.parse_args()
    args.func(args)


if __name__ == '__main__':
    main()
