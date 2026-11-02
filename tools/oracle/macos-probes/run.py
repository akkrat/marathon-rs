#!/usr/bin/env python3
"""Compile unchanged macOS sources/tests in /tmp against the reference JDK and cached jars."""
import importlib.util, os, pathlib, subprocess, tempfile
here = pathlib.Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location('installer', here.parent/'installer-probes/run.py')
p = importlib.util.module_from_spec(spec); spec.loader.exec_module(p)
coordinates = [('org.junit.jupiter','junit-jupiter-api','5.11.0'),('org.junit.platform','junit-platform-commons','1.11.0'),('org.amshove.kluent','kluent','1.73'),('org.mockito.kotlin','mockito-kotlin','5.4.0'),('org.mockito','mockito-core','5.12.0'),('net.bytebuddy','byte-buddy','1.14.15'),('net.bytebuddy','byte-buddy-agent','1.14.15'),('org.objenesis','objenesis','3.3'),('org.opentest4j','opentest4j','1.3.0'),('org.apiguardian','apiguardian-api','1.1.2')]
cp = p.cp + os.pathsep + os.pathsep.join(str(p.jar(*j)) for j in coordinates)
mac = p.source/'vendor/vendor-apple/macos/src'
main = mac/'main/kotlin/com/malinskiy/marathon/apple/macos'
tests = mac/'test/kotlin/com/malinskiy/marathon/apple/macos'
config = p.source/'configuration/src/main/kotlin/com/malinskiy/marathon/config'
files = [*p.files, *sorted(main.glob('*.kt')), *sorted(tests.glob('*.kt'))]
files += [p.base/(n+'.kt') for n in ['AppleApplicationInstaller','XcodebuildSession','AppleDeviceTestRunner','AppleTestBundleIdentifier','cmd/SharedTransfers','extensions/ConfigurationExtensions','device/ConnectionFactory','device/ConnectionFailures']]
files += [config/'vendor/VendorConfiguration.kt', config/'vendor/apple/TimeoutConfiguration.kt', config/'vendor/apple/ios/WorkerConnectionConfiguration.kt']
with tempfile.TemporaryDirectory(prefix='mrs56-oracle-', dir='/tmp') as temp:
    temp = pathlib.Path(temp)
    subprocess.run([p.java,'-cp',os.pathsep.join(map(str,p.compiler)),'org.jetbrains.kotlin.cli.jvm.K2JVMCompiler','-no-stdlib','-no-reflect','-nowarn','-jvm-target','17','-cp',cp,'-d',str(temp/'classes'),*map(str,dict.fromkeys(files)),str(here/'Probe.kt'),str(here/'Supplemental.kt')],check=True)
    result = subprocess.run([p.java,'-cp',str(temp/'classes')+os.pathsep+cp,'ProbeKt',str(temp/'cases')],capture_output=True,check=True,timeout=150)
    rows = [s[4:] for s in result.stdout.decode().splitlines() if s.startswith('ROW=')]
    out = p.repo/'fixtures/oracle/macos/probes.jsonl'; out.write_text('\n'.join(rows)+'\n')
    print(f'{len(rows)} JVM rows -> {out}')
