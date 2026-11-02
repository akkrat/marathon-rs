#!/usr/bin/env python3
"""Capture the real frozen device and fake worker in an isolated /tmp checkout."""
import json, os, pathlib, re, subprocess, sys, xml.etree.ElementTree as ET
here = pathlib.Path(__file__).resolve().parent
repo = here.parents[2]
source = pathlib.Path(os.environ.get('MARATHON_KT', os.path.expanduser('~/src/marathon')))
scratch = pathlib.Path(sys.argv[1])
if not str(scratch).startswith('/tmp/'):
    raise SystemExit('scratch must be under /tmp')
if not scratch.exists():
    subprocess.run(['git', 'clone', '--quiet', '--no-hardlinks', str(source), str(scratch)], check=True)
subprocess.run(['git', 'checkout', '--quiet', '--detach', 'e47352f2'], cwd=scratch, check=True)
out = repo / 'fixtures/oracle/simulator-device'
ios = scratch / 'vendor/vendor-apple/ios'
base = scratch / 'vendor/vendor-apple/base'
main = ios / 'src/main/kotlin/com/malinskiy/marathon/apple/ios'
tests = ios / 'src/test/kotlin/com/malinskiy/marathon/apple/ios'
def frozen(path):
    return subprocess.check_output(['git', 'show', 'e47352f2:' + str(path.relative_to(scratch))], cwd=scratch, text=True)
def capture(s):
    return '@org.junit.jupiter.api.extension.ExtendWith(com.malinskiy.marathon.apple.ios.SimulatorCapture::class)\n' + s
helper = '''
    private fun oracle(kind: String, error: Throwable? = null) {
        System.getProperty("simulator.evidence")?.let { path ->
            fun chain(e: Throwable?): Any? = e?.let { mapOf("class" to it.javaClass.name, "message" to it.message, "cause" to chain(it.cause), "suppressed" to it.suppressed.map { s -> chain(s) }) }
            val row = mapOf("scenario" to System.getProperty("simulator.scenario"), "kind" to kind,
                "serial" to serialNumber, "lost" to lost, "retired" to retired, "disposed" to disposed,
                "waiting" to (waitingSince != null), "failure" to chain(error))
            synchronized(java.io.File::class.java) { java.io.File(path).appendText(com.google.gson.Gson().toJson(row) + "\\n") }
        }
    }
'''
p = main / 'AppleSimulatorDevice.kt'
s = frozen(p)
s = s.replace('previousBatchFinishedAt = System.nanoTime()', 'previousBatchFinishedAt = System.nanoTime()\n            oracle("batchFinally")')
s = s.replace('System.nanoTime()', 'SimulatorProbeClock.nanoTime()')
s += '\ninternal object SimulatorProbeClock { @Volatile var now: (() -> Long)? = null; fun nanoTime(): Long = now?.invoke() ?: System.nanoTime() }\n'
s = s.replace('    private val executionLock', helper + '\n    private val executionLock')
s = re.sub(r'(catch \(e: [^)]+\) \{)', r'\1\n            oracle("catch", e)', s)
s = s.replace('synchronized(executionLock) { retiredForGood = true }', 'synchronized(executionLock) { retiredForGood = true }\n        oracle("retire")')
s = s.replace('started.complete(Unit)', 'started.complete(Unit)\n                oracle("disposed")')
p.write_text(s)
p = base / 'src/main/kotlin/com/malinskiy/marathon/apple/bin/xcrun/xcodebuild/Xcodebuild.kt'
s = frozen(p).replace('        return commandExecutor.execute(', '\n        System.getProperty("simulator.evidence")?.let { path -> synchronized(java.io.File::class.java) { java.io.File(path).appendText(com.google.gson.Gson().toJson(mapOf("scenario" to System.getProperty("simulator.scenario"), "kind" to "xcodebuild", "argv" to command, "timeoutMillis" to configuration.testBatchTimeoutMillis, "idleTimeoutMillis" to configuration.testOutputTimeoutMillis)) + "\\n") } }\n        return commandExecutor.execute(')
p.write_text(s)
p = tests / 'device/FakeSimulatorWorker.kt'
s = frozen(p).replace('        commands.add(command)', '''        commands.add(command)
        System.getProperty("simulator.evidence")?.let { path -> synchronized(java.io.File::class.java) { java.io.File(path).appendText(Gson().toJson(mapOf("scenario" to System.getProperty("simulator.scenario"), "kind" to "command", "argv" to command, "timeout" to timeout.toString(), "idleTimeout" to idleTimeout.toString())) + "\\n") } }''')
s = s.replace('        return Session(before(command) ?: answer(command))', '''        val result = before(command) ?: answer(command)
        System.getProperty("simulator.evidence")?.let { path -> synchronized(java.io.File::class.java) { java.io.File(path).appendText(Gson().toJson(mapOf("scenario" to System.getProperty("simulator.scenario"), "kind" to "result", "argv" to command, "exit" to result.exitCode, "stdout" to result.stdout, "stderr" to result.stderr)) + "\\n") } }
        return Session(result)''')
p.write_text(s)
classes = ['AppleSimulatorDeviceBatchTest', 'AppleSimulatorDeviceBatchFailureTest', 'AppleSimulatorDeviceSessionTest', 'AppleSimulatorDeviceDisposeTest', 'AppleSimulatorDeviceArtifactTest', 'AppleSimulatorDeviceXcodeVersionTest', 'StartupPhasesTest', 'device/WorkerProvisioningContextTest']
for cls in classes:
    p = tests / (cls + '.kt')
    p.write_text(frozen(p).replace('class ' + p.stem, capture('class ' + p.stem)))
(tests / 'SimulatorCapture.kt').write_text((here / 'Capture.kt').read_text())
(tests / 'SimulatorEvidenceTest.kt').write_text((here / 'Witness.kt').read_text())
evidence = scratch / 'simulator-events.jsonl'
evidence.write_text('')
p = ios / 'build.gradle.kts'
p.write_text(frozen(p) + f'\ntasks.withType<Test>().configureEach {{\n systemProperty("simulator.evidence", "{evidence}")\n outputs.upToDateWhen {{ false }}\n}}\n')
env = dict(os.environ, JAVA_HOME='/Applications/Android Studio.app/Contents/jbr/Contents/Home')
args = ['./gradlew', ':vendor:vendor-apple:ios:test', '--offline', '--console=plain']
for cls in classes + ['SimulatorEvidenceTest', 'SimulatorSetupEvidenceTest', 'SimulatorClassifierEvidenceTest']:
    args += ['--tests', '*' + cls.rsplit('/', 1)[-1]]
with (scratch / 'gradle-capture.log').open('w') as log:
    result = subprocess.run(args, cwd=scratch, env=env, stdout=log, stderr=subprocess.STDOUT)
if result.returncode:
    print((scratch / 'gradle-capture.log').read_text()[-8000:]); raise SystemExit(result.returncode)
base_args = ['./gradlew', ':vendor:vendor-apple:base:test', '--offline', '--console=plain', '--tests', '*BatchFailurePolicyTest', '--tests', '*ResultBundleRunListenerTest']
with (scratch / 'gradle-capture.log').open('a') as log:
    result = subprocess.run(base_args, cwd=scratch, env=env, stdout=log, stderr=subprocess.STDOUT)
if result.returncode:
    print((scratch / 'gradle-capture.log').read_text()[-8000:]); raise SystemExit(result.returncode)
out.mkdir(parents=True, exist_ok=True)
rows = []
for path in sorted([*(ios / 'build/test-results/test').glob('TEST-*.xml'), *(base / 'build/test-results/test').glob('TEST-*.xml')]):
    for test in ET.parse(path).getroot().findall('testcase'):
        rows.append(dict(suite=test.attrib['classname'], test=test.attrib['name'], passed=test.find('failure') is None and test.find('error') is None))
(out / 'tests.jsonl').write_text(''.join(json.dumps(r, separators=(',', ':')) + '\n' for r in rows))
# Preserve actual messages, replacing only the disposable checkout path.
text = evidence.read_text().replace(str(scratch.resolve()), '<scratch>').replace(str(scratch), '<scratch>')
(out / 'events.jsonl').write_text(text)
java = str(pathlib.Path(env['JAVA_HOME']) / 'bin/java')
flags = []
for options in [[], ['-ea']]:
    flags.append(subprocess.check_output([java, *options, '-cp', str(source / 'cli/build/install/marathon/lib/*'), str(here / 'RuntimeFlags.java')], text=True).strip())
(out / 'runtime-flags.jsonl').write_text('\n'.join(flags) + '\n')
print(f'{len(rows)} JVM tests; {len(text.splitlines())} events -> {out}')
