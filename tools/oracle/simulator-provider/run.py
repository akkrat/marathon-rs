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
files += [tests / (name + 'Test.kt') for name in ['AppleSimulatorProvider', 'AppleSimulatorProviderUpdate', 'AppleSimulatorProviderArtifact', 'WorkerProvisioningContext']]
files += [tests.parent / 'StartupPhasesTest.kt']
output = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else repo / 'fixtures/oracle/simulator-provider/probes.jsonl'
with tempfile.TemporaryDirectory(prefix='mrs55a-probe-', dir='/tmp') as tmp:
    checkout = pathlib.Path(tmp) / 'kotlin'
    subprocess.run(['git', 'clone', '--quiet', '--no-hardlinks', '--no-checkout', str(source), str(checkout)], check=True)
    subprocess.run(['git', '-C', str(checkout), 'checkout', '--quiet', '--detach', 'e47352f2'], check=True)
    copied = [checkout / file.relative_to(source) for file in files]
    classes = pathlib.Path(tmp) / 'classes'
    compiled = subprocess.run([java,'-cp',os.pathsep.join(map(str,compiler)), 'org.jetbrains.kotlin.cli.jvm.K2JVMCompiler','-no-stdlib','-no-reflect','-nowarn','-jvm-target','17','-cp',cp,'-d',str(classes),str(here / 'Probe.kt'),str(here / 'Witness.kt'),*map(str,copied)],capture_output=True,text=True)
    if compiled.returncode:
        print(compiled.stderr, file=sys.stderr)
        raise SystemExit(compiled.returncode)
    run = subprocess.run([java,'-XX:+EnableDynamicAgentLoading','-cp',os.pathsep.join([str(classes),str(base / 'test/resources'),cp]),'com.malinskiy.marathon.apple.ios.device.ProbeKt',tmp],text=True, capture_output=True, timeout=180, env=dict(os.environ, PROVIDER_CYCLE='${PROVIDER_CYCLE}'))
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
