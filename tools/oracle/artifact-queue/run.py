#!/usr/bin/env python3
"""Run frozen suites in a disposable checkout with observational test instrumentation."""
import pathlib, subprocess, os, json, re, xml.etree.ElementTree as ET, sys
here = pathlib.Path(__file__).resolve().parent
repo = here.parents[2]
source = pathlib.Path(os.environ.get('MARATHON_KT', os.path.expanduser('~/src/marathon')))
scratch = pathlib.Path(sys.argv[1])
out = pathlib.Path(sys.argv[2])
if not scratch.exists():
    subprocess.run(['git', 'clone', '--quiet', '--no-hardlinks', str(source), str(scratch)], check=True)
subprocess.run(['git', 'checkout', '--quiet', '--detach', 'e47352f2'], cwd=scratch, check=True)
base = scratch / 'vendor/vendor-apple/base'
tests = base / 'src/test/kotlin/com/malinskiy/marathon/apple'
(tests / 'ArtifactCapture.kt').write_text((here / 'Capture.kt').read_text())
for name in ['ArtifactProcessorTest', 'ArtifactProcessorFairnessTest', 'ArtifactQueueSizingTest', 'listener/ResultBundleRunListenerTest']:
    p = tests / (name + '.kt')
    s = p.read_text()
    annotation = '@org.junit.jupiter.api.extension.ExtendWith(com.malinskiy.marathon.apple.ArtifactCapture::class)\n'
    if annotation not in s: s = s.replace('class ' + p.stem, annotation + 'class ' + p.stem)
    p.write_text(s)
p = base / 'src/main/kotlin/com/malinskiy/marathon/apple/ArtifactProcessor.kt'
# Always start instrumentation from the frozen file, making reruns idempotent.
s = subprocess.check_output(['git', 'show', 'e47352f2:vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/ArtifactProcessor.kt'], cwd=scratch, text=True)
helper = r'''    private fun oracle(kind: String, work: Work? = null, time: Long? = null, extra: Map<String, Any?> = emptyMap()) {
        System.getProperty("artifact.trace")?.let { path ->
            val row = mapOf("kind" to kind, "queue" to System.identityHashCode(this),
                "scenario" to System.getProperty("artifact.scenario"), "id" to work?.let { System.identityHashCode(it) },
                "owner" to work?.owner, "batch" to work?.batchId, "worker" to work?.worker,
                "pending" to pending.map { listOf(it.key, it.value.count) }, "outstanding" to outstanding, "timeNanos" to time, "capacity" to capacity, "concurrency" to maxConcurrent, "share" to workerCapacity) + extra
            synchronized(java.io.File::class.java) { java.io.File(path).appendText(com.google.gson.Gson().toJson(row) + "\n") }
        }
    }
'''
s = s.replace('    private val logger =', helper + '    private val logger =')
s = s.replace('        val immediate = synchronized(lock) {', '        val immediate = synchronized(lock) {\n            oracle("submit", waiter.work, waiter.arrivedAt)')
s = s.replace('                        waiter.state = Admission.CANCELLED', '                        oracle("cancel", waiter.work)\n                        waiter.state = Admission.CANCELLED')
s = s.replace('        admissions++', '        admissions++\n        oracle("admit", work, work.enqueuedAt, mapOf("depth" to outstanding, "workerDepth" to entry.held, "blockedByRun" to waiter.blockedByRun, "startsWorker" to waiter.startsWorker))')
s = s.replace('                    workers.remove(worker)', '                    oracle("idle", extra = mapOf("worker" to worker))\n                    workers.remove(worker)')
s = s.replace('queue.removeFirst().also { running.add(it) }', 'queue.removeFirst().also { running.add(it); oracle("take", it) }')
s = s.replace('            closed = true', '            oracle("close")\n            closed = true')
s = s.replace('                running.toList().also', '                oracle("sweep")\n                running.toList().also')
s = s.replace('            val state = checkNotNull(pending[work.owner])', '            oracle("complete", work)\n            val state = checkNotNull(pending[work.owner])')
s = s.replace('        val admission = admissionStats()', '        val admission = admissionStats()\n        synchronized(lock) { oracle("stats", extra = mapOf("stats" to admission)) }')
p.write_text(s)
evidence = scratch / 'artifact-events.jsonl'
trace = scratch / 'artifact-trace.jsonl'
for f in [evidence, trace]: f.write_text('')
build = base / 'build.gradle.kts'
s = build.read_text()
if 'artifact.evidence' not in s:
    s += '\ntasks.withType<Test>().configureEach {\n'
    s += f'    systemProperty("artifact.evidence", "{evidence}")\n    systemProperty("artifact.trace", "{trace}")\n}}\n'
    build.write_text(s)
if 'artifact-force-run' not in build.read_text():
    with build.open('a') as f:
        f.write('\ntasks.withType<Test>().configureEach { outputs.upToDateWhen { false } } // artifact-force-run\n')
env = dict(os.environ, JAVA_HOME='/Applications/Android Studio.app/Contents/jbr/Contents/Home')
args = ['./gradlew', ':vendor:vendor-apple:base:test', '--offline', '--console=plain']
for cls in ['ArtifactProcessorTest', 'ArtifactProcessorFairnessTest', 'ArtifactQueueSizingTest', 'ResultBundleRunListenerTest', 'ArtifactEvidenceTest']:
    args += ['--tests', '*' + cls]
with (scratch / 'gradle-capture.log').open('w') as log:
    subprocess.run(args, cwd=scratch, env=env, stdout=log, stderr=subprocess.STDOUT, check=True)
out.mkdir(parents=True, exist_ok=True)
# Keep the JVM exception suffix witness; also provide the approved D12 projection.
def normalize_path(s):
    return re.sub(r'(?:/private)?/var/folders/[^/]+/[^/]+/T/junit-[^/ ;,)]+', '<tmp>', s)
def normalized_message(s):
    return re.sub(r'; job=[^\s)]+@[0-9a-f]+', '', normalize_path(s))
events = []
for line in evidence.read_text().splitlines():
    row = json.loads(line)
    for key, value in list(row.items()):
        if isinstance(value, str):
            row[key] = normalize_path(value)
            projected = normalized_message(value)
            if projected != row[key]: row['rust' + key[0].upper() + key[1:]] = projected
    events.append(row)
(out / 'events.jsonl').write_text(''.join(json.dumps(r, separators=(',', ':')) + '\n' for r in events))
queues, work_ids = {}, {}
normalized = []
seen_queues = set()
for line in trace.read_text().splitlines():
    row = json.loads(line)
    q = row['queue']
    row['queue'] = queues.setdefault(q, len(queues))
    if 'id' in row: row['id'] = work_ids.setdefault((q, row['id']), len(work_ids))
    if row['queue'] not in seen_queues:
        normalized.append({key: row[key] for key in ['queue', 'scenario', 'capacity', 'concurrency', 'share']} | {'kind': 'queue'})
        seen_queues.add(row['queue'])
    for key in ['scenario', 'capacity', 'concurrency', 'share']: row.pop(key, None)
    if row['kind'] != 'submit':
        for key in ['owner', 'batch']: row.pop(key, None)
        if row['kind'] != 'idle': row.pop('worker', None)
    if row['kind'] not in ['complete', 'stats']: row.pop('pending', None)
    normalized.append(row)
(out / 'transitions.jsonl').write_text(''.join(json.dumps(row, separators=(',', ':')) + '\n' for row in normalized))
suites = []
reports = []
for f in sorted((base / 'build/test-results/test').glob('TEST-*.xml')):
    root = ET.parse(f).getroot()
    suites.append({k: root.attrib[k] for k in ['name', 'tests', 'failures', 'errors']})
    stream = root.findtext('system-out') or ''
    for header in ['Artifact queue harness:', 'Artifact queue sizing harness:']:
        if header in stream:
            report = stream[stream.index(header):].split('\n\n', 1)[0] + '\n'
            (out / (f.stem.removeprefix('TEST-').rsplit('.', 1)[-1] + '.txt')).write_text(report)
            for line in report.splitlines()[1:]:
                if header == 'Artifact queue harness:':
                    numbers = list(map(int, re.findall(r'\d+', line.replace('p95', 'percentile'))))
                    workers, share, runtime, mean, healthy, idle, depth, outstanding, occupancy = numbers
                    reports.append(dict(kind='characterization', workers=workers, share=share, skewed='skewed' in line,
                        expected=dict(runtime=runtime, mean=mean, healthy=healthy, idle=idle, depth=depth, outstanding=outstanding, occupancy=occupancy)))
                else:
                    capacity, concurrency, share, stalled, wait, healthy, p95, waited, admitted, for_run, outstanding, depth, stalled_depth, active = map(int, re.findall(r'\d+', line.replace('p95', 'percentile')))
                    reports.append(dict(kind='sizing', capacity=capacity, concurrency=concurrency, share=share, stalled=stalled,
                        spread='SPREAD' in line, expected=dict(wait=wait, healthy=healthy, p95=p95, waited=waited, admitted=admitted,
                        for_run=for_run, outstanding=outstanding, depth=depth, stalled_depth=stalled_depth, active=active)))
(out / 'reports.jsonl').write_text(''.join(json.dumps(r) + '\n' for r in reports))
(out / 'suites.jsonl').write_text(''.join(json.dumps(s) + '\n' for s in suites))
print(json.dumps(suites))
