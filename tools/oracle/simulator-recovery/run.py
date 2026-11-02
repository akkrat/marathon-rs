#!/usr/bin/env python3
"""Compile frozen provisioning sources/tests in /tmp; reference checkout and installed jars are read-only."""
import json, os, pathlib, subprocess, sys, tempfile
here = pathlib.Path(__file__).resolve().parent
repo = here.parents[2]
source = pathlib.Path(os.environ.get('MARATHON_KT', os.path.expanduser('~/src/marathon')))
head = subprocess.check_output(['git', '-C', str(source), 'rev-parse', 'HEAD'], text=True).strip()
if not head.startswith('e47352f2'):
    raise SystemExit(f'expected frozen e47352f2, got {head}')
lib = pathlib.Path(os.environ.get('MARATHON_KOTLIN_LIB', str(source / 'cli/build/install/marathon/lib')))
cache = pathlib.Path.home() / '.gradle/caches/modules-2/files-2.1'
def jar(group, name, version):
    return next((cache / group / name / version).glob('*/*.jar'))
compiler = [jar(*a) for a in [('org.jetbrains.kotlin','kotlin-compiler-embeddable','1.9.24'), ('org.jetbrains.kotlin','kotlin-stdlib','1.9.24'), ('org.jetbrains.kotlin','kotlin-script-runtime','1.9.24'), ('org.jetbrains.kotlin','kotlin-reflect','1.6.10'), ('org.jetbrains.intellij.deps','trove4j','1.0.20200330'), ('org.jetbrains','annotations','13.0')]]
extras = [('org.mockito','mockito-core','5.12.0'), ('org.mockito.kotlin','mockito-kotlin','5.4.0'), ('net.bytebuddy','byte-buddy','1.14.15'), ('net.bytebuddy','byte-buddy-agent','1.14.15'), ('org.objenesis','objenesis','3.3'), ('org.jetbrains.kotlinx','kotlinx-coroutines-test-jvm','1.8.1'), ('org.junit.jupiter','junit-jupiter-api','5.11.0'), ('org.junit.jupiter','junit-jupiter-params','5.11.0'), ('org.opentest4j','opentest4j','1.3.0'), ('org.apiguardian','apiguardian-api','1.1.2'), ('org.amshove.kluent','kluent','1.73'), ('org.amshove.kluent','kluent-common','1.73'), ('com.willowtreeapps.assertk','assertk-jvm','0.28.1')]
cp = os.pathsep.join(map(str, [*sorted(lib.glob('*.jar')), *[jar(*a) for a in extras]]))
java = str(pathlib.Path(os.environ.get('JAVA_HOME','/Applications/Android Studio.app/Contents/jbr/Contents/Home')) / 'bin/java')
base = source / 'vendor/vendor-apple/ios/src'
main = base / 'main/kotlin/com/malinskiy/marathon/apple/ios/device'
tests = base / 'test/kotlin/com/malinskiy/marathon/apple/ios/device'
files = [main / (name + '.kt') for name in ['WorkerProvisioningContext','SimulatorFactory','SimulatorProvisioner','ProvisioningPlan','DeviceTracker']]
files += [source / 'configuration/src/main/kotlin/com/malinskiy/marathon/config/vendor/VendorConfiguration.kt', source / 'configuration/src/main/kotlin/com/malinskiy/marathon/config/vendor/apple/ios/WorkerConnectionConfiguration.kt']
files += [source / 'configuration/src/main/kotlin/com/malinskiy/marathon/config/vendor/apple/TimeoutConfiguration.kt']
files += [main.parent / 'AppleSimulatorDevice.kt', source / 'vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/device/ConnectionFailures.kt']
files += [main / (name + '.kt') for name in ['AppleSimulatorProvider', 'PendingWorker', 'Detached']]
files += [main.parent / 'extensions/ByteBuffer.kt', tests / 'FakeSimulatorWorker.kt']
files += [source / 'vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/ArtifactProcessor.kt']
files += [source / 'vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/bin/xcrun/xcresulttool/Xcresulttool.kt']
files += [source / 'vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/device/ConnectionFactory.kt']
files += [tests / (name + 'Test.kt') for name in ['AppleSimulatorProviderReconnect', 'AppleSimulatorProviderWorkerRetry']]
files += [tests.parent / 'StartupPhasesTest.kt']
output = pathlib.Path(next((a for a in sys.argv[1:] if not a.startswith('--')), repo / 'fixtures/oracle/simulator-recovery/probes.jsonl'))
if '--pool-only' not in sys.argv:
    with tempfile.TemporaryDirectory(prefix='mrs55b-probe-', dir='/tmp') as tmp:
        checkout = pathlib.Path(tmp) / 'kotlin'
        subprocess.run(['git', 'clone', '--quiet', '--no-hardlinks', '--no-checkout', str(source), str(checkout)], check=True)
        subprocess.run(['git', '-C', str(checkout), 'checkout', '--quiet', '--detach', 'e47352f2'], check=True)
        copied = [checkout / file.relative_to(source) for file in files]
        classes = pathlib.Path(tmp) / 'classes'
        compiled = subprocess.run([java,'-cp',os.pathsep.join(map(str,compiler)), 'org.jetbrains.kotlin.cli.jvm.K2JVMCompiler','-no-stdlib','-no-reflect','-nowarn','-jvm-target','17','-cp',cp,'-d',str(classes),str(here / 'Probe.kt'),str(here / 'Witness.kt'),*map(str,copied)],capture_output=True,text=True)
        if compiled.returncode:
            print(compiled.stderr, file=sys.stderr)
            raise SystemExit(compiled.returncode)
        run = subprocess.run([java,'-XX:+EnableDynamicAgentLoading','-cp',os.pathsep.join([str(classes),str(base / 'test/resources'),cp]),'com.malinskiy.marathon.apple.ios.device.ProbeKt',tmp],text=True, capture_output=True, timeout=240, env=dict(os.environ, PROVIDER_CYCLE='${PROVIDER_CYCLE}'))
        if run.returncode:
            print(run.stderr, file=sys.stderr)
            print(run.stdout[-6000:], file=sys.stderr)
            raise SystemExit(run.returncode)
        if os.environ.get("PROVIDER_RAW_LOG"):
            pathlib.Path(os.environ["PROVIDER_RAW_LOG"]).write_text(run.stdout)
        rows = [json.loads(line) for line in run.stdout.splitlines() if line.startswith('{"case":')]
        if not rows: raise SystemExit('probe produced no structured evidence')
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(''.join(json.dumps(row, ensure_ascii=False, separators=(',',':')) + '\n' for row in rows))
        print(f'{len(rows)} JVM rows -> {output}')

# Re-execute the existing 3.8 source probes in independent scratch staging. They
# instrument the actual pool actor observationally and retain its original tests.
import importlib.util
pool = repo / 'tools/oracle/pool-probes'
spec = importlib.util.spec_from_file_location('recovery_pool_adapt', pool / 'adapt.py')
adapter = importlib.util.module_from_spec(spec)
spec.loader.exec_module(adapter)
with tempfile.TemporaryDirectory(prefix='mrs55b-pool-', dir='/tmp') as tmp:
    scratch = pathlib.Path(tmp)
    inputs = adapter.sources(scratch)
    classes = scratch / 'classes'
    pool_cp = os.pathsep.join(p for p in cp.split(os.pathsep) if not pathlib.Path(p).name.startswith('kluent'))
    compiled = subprocess.run([java, '-cp', os.pathsep.join(map(str, compiler)), 'org.jetbrains.kotlin.cli.jvm.K2JVMCompiler',
                    '-no-stdlib', '-no-reflect', '-nowarn', '-jvm-target', '17', '-cp', pool_cp, '-d', str(classes),
                    str(pool / 'PoolProbes.kt'), str(pool / 'SchedulerProbes.kt'), *map(str, inputs)],
                   capture_output=True, text=True)
    if compiled.returncode:
        print(compiled.stderr, file=sys.stderr)
        raise SystemExit(compiled.returncode)
    pool_output = scratch / 'evidence'
    subprocess.run([java, '-XX:+EnableDynamicAgentLoading', '-cp', os.pathsep.join([str(classes), cp]),
                    'oracle.pool.PoolProbesKt', str(pool_output)], check=True, stdout=subprocess.DEVNULL, timeout=90)
    import hashlib
    manifest = {'source': head, 'comparison': 'byte-identical re-execution', 'files': {}}
    for name in ['tests.jsonl', 'scheduler.jsonl']:
        captured = (pool_output / name).read_bytes()
        baseline = repo / 'fixtures/oracle/pool' / name
        if captured != baseline.read_bytes():
            raise SystemExit(f'pool source re-execution differs from {baseline}')
        manifest['files'][name] = {'path': str(baseline.relative_to(repo)), 'rows': len(captured.splitlines()),
                                   'sha256': hashlib.sha256(captured).hexdigest(), 'matchesCommitted': True}
    (output.parent / 'pool.json').write_text(json.dumps(manifest, indent=2) + '\n')
    print('50 actual pool/Scheduler rows match the committed corpus; digest -> pool.json')
