#!/usr/bin/env python3
"""Compile unchanged e47352f2 runner/parser sources and tests against recording seams."""
import importlib.util, os, pathlib, subprocess, tempfile
here=pathlib.Path(__file__).resolve().parent
spec=importlib.util.spec_from_file_location('installer',here.parent/'installer-probes/run.py')
p=importlib.util.module_from_spec(spec);spec.loader.exec_module(p)
jars=[('org.junit.jupiter','junit-jupiter-api','5.11.0'),('org.junit.platform','junit-platform-commons','1.11.0'),('org.amshove.kluent','kluent','1.73'),('org.mockito.kotlin','mockito-kotlin','5.4.0'),('org.mockito','mockito-core','5.12.0'),('net.bytebuddy','byte-buddy','1.14.15'),('net.bytebuddy','byte-buddy-agent','1.14.15'),('org.objenesis','objenesis','3.3'),('org.opentest4j','opentest4j','1.3.0'),('org.apiguardian','apiguardian-api','1.1.2')]
# assertk's JVM artifact is also present in the installed CLI's build cache.
cache=p.cache
extra=list((cache/'com.willowtreeapps.assertk/assertk-jvm').glob('*/*/*.jar'))
jars += [('org.junit.jupiter','junit-jupiter-params','5.11.0')]
cp=p.cp+os.pathsep+os.pathsep.join(str(p.jar(*j)) for j in jars)+os.pathsep+os.pathsep.join(map(str,extra))
files=[*p.files,*[p.base/(n+'.kt') for n in ['AppleApplicationInstaller','XcodebuildSession','AppleDeviceTestRunner','NmTestParser','XCTestParser','AppleTestBundleIdentifier']],p.base/'cmd/SharedTransfers.kt',p.base/'extensions/ConfigurationExtensions.kt']
tests=p.source/'vendor/vendor-apple/base/src/test/kotlin/com/malinskiy/marathon/apple'
ios=p.source/'vendor/vendor-apple/ios/src/test/kotlin/com/malinskiy/marathon/apple/ios'
files += [ios/'StartupPhasesTest.kt',ios/'SkipCopyingDerivedDataTest.kt',ios/'device/FakeSimulatorWorker.kt']
files += [tests/'AppleDeviceTestRunnerTest.kt',tests/'XCTestParserTest.kt',here/'Probe.kt',here/'Cases.kt']
config=p.source/'configuration/src/main/kotlin/com/malinskiy/marathon/config'
files += [config/'vendor/VendorConfiguration.kt',config/'vendor/apple/TimeoutConfiguration.kt',config/'vendor/apple/ios/WorkerConnectionConfiguration.kt']
iosmain=p.source/'vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios'
files += [iosmain/'AppleSimulatorDevice.kt',*sorted((iosmain/'device').glob('*.kt')),p.base/'device/ConnectionFailures.kt',p.base/'device/ConnectionFactory.kt']
files=list(dict.fromkeys(files))
with tempfile.TemporaryDirectory(prefix='mrs48-oracle-',dir='/tmp') as temp:
    temp=pathlib.Path(temp); classes=temp/'classes'
    subprocess.run([p.java,'-cp',os.pathsep.join(map(str,p.compiler)),'org.jetbrains.kotlin.cli.jvm.K2JVMCompiler','-no-stdlib','-no-reflect','-nowarn','-jvm-target','17','-cp',cp,'-d',str(classes),*map(str,files)],check=True)
    output=subprocess.run([p.java,'-cp',str(classes)+os.pathsep+cp,'ProbeKt',str(temp/'bundles')],stdout=subprocess.PIPE,check=True,timeout=120).stdout.decode()
    lines=[l[4:] for l in output.splitlines() if l.startswith('ROW=')]
    out=p.repo/'fixtures/oracle/test-runner/probes.jsonl';out.parent.mkdir(parents=True,exist_ok=True);out.write_text('\n'.join(lines)+'\n')
    print(f'{len(lines)} probes -> {out}')
