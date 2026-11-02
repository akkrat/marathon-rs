#!/usr/bin/env python3
"""Run the thirteen unmodified Kotlin tests through their real simulator installer."""
import json, os, pathlib, subprocess, tempfile
import run as probe
jars=[('org.junit.jupiter','junit-jupiter-api','5.11.0'),('org.junit.platform','junit-platform-commons','1.11.0'),('org.amshove.kluent','kluent','1.73'),('org.mockito.kotlin','mockito-kotlin','5.4.0'),('org.mockito','mockito-core','5.12.0'),('net.bytebuddy','byte-buddy','1.14.15'),('net.bytebuddy','byte-buddy-agent','1.14.15'),('org.objenesis','objenesis','3.3'),('org.opentest4j','opentest4j','1.3.0'),('org.apiguardian','apiguardian-api','1.1.2')]
cp=probe.cp+os.pathsep+os.pathsep.join(str(probe.jar(*j)) for j in jars)
tests=probe.source/'vendor/vendor-apple/ios/src/test/kotlin/com/malinskiy/marathon/apple/ios'
files=[*probe.files,probe.base/'AppleApplicationInstaller.kt',probe.base/'cmd/SharedTransfers.kt',probe.base/'extensions/ConfigurationExtensions.kt',tests/'SkipCopyingDerivedDataTest.kt',tests/'device/FakeSimulatorWorker.kt',probe.here/'Harness.kt']
with tempfile.TemporaryDirectory(prefix='mrs410b-harness-', dir='/tmp') as temp:
    temp=pathlib.Path(temp);classes=temp/'classes'
    subprocess.run([probe.java,'-cp',os.pathsep.join(map(str,probe.compiler)),'org.jetbrains.kotlin.cli.jvm.K2JVMCompiler','-no-stdlib','-no-reflect','-nowarn','-jvm-target','17','-cp',cp,'-d',str(classes),*map(str,files)],check=True)
    output=subprocess.run([probe.java,'-cp',str(classes)+os.pathsep+cp,'HarnessKt',str(temp/'bundles')],stdout=subprocess.PIPE,check=True,timeout=90).stdout.decode()
    results=json.loads(next(l.removeprefix('HARNESS=') for l in output.splitlines() if l.startswith('HARNESS=')))
    out=probe.repo/'fixtures/oracle/installer/kotlin-tests.jsonl'
    out.write_text(''.join(json.dumps(row)+'\n' for row in results))
    print(f'{len(results)} unchanged Kotlin tests passed')
