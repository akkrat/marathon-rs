#!/usr/bin/env python3
"""Run the unmodified e47352f2 installer against a recording device; never edit reference sources."""
import json, os, pathlib, subprocess, sys, tempfile, zipfile
here=pathlib.Path(__file__).resolve().parent
repo=here.parents[2]
source=pathlib.Path(os.environ.get('MARATHON_KT',os.path.expanduser('~/src/marathon')))
lib=pathlib.Path(os.environ.get('MARATHON_KOTLIN_LIB',str(source/'cli/build/install/marathon/lib')))
cache=pathlib.Path.home()/'.gradle/caches/modules-2/files-2.1'
def jar(g,a,v): return next((cache/g/a/v).glob('*/*.jar'))
compiler=[jar('org.jetbrains.kotlin','kotlin-compiler-embeddable','1.9.24'),jar('org.jetbrains.kotlin','kotlin-stdlib','1.9.24'),jar('org.jetbrains.kotlin','kotlin-script-runtime','1.9.24'),jar('org.jetbrains.kotlin','kotlin-reflect','1.6.10'),jar('org.jetbrains.intellij.deps','trove4j','1.0.20200330'),jar('org.jetbrains','annotations','13.0')]
java=str(pathlib.Path(os.environ.get('JAVA_HOME','/Applications/Android Studio.app/Contents/jbr/Contents/Home'))/'bin/java')
cp=os.pathsep.join(map(str, [*sorted(lib.glob('*.jar')),jar('org.jetbrains.kotlinx','kotlinx-coroutines-test-jvm','1.8.1')]))
base=source/'vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple'
files=[base/'AppleDevice.kt',base/'RemoteFileManager.kt',base/'DistributedDerivedData.kt',base/'xctestrun/TestRootFactory.kt',*sorted((base/'bin').rglob('*.kt')),*sorted((base/'model').glob('*.kt'))]
cases=[]
def case(id,**kw):
    c=dict(id=id,sdk='IPHONESIMULATOR',type='XCUITEST',reuse=False,skip=False,parser=False,app='CloudApp.app',runner='UITests-Runner.app',test='UITests-Runner.app/PlugIns/UITests.xctest',udid='udid',installer=True)
    c.update(kw); cases.append(c)
for layout in ['cloud','runner','unit']:
    for skip in [False,True]:
        case(layout+('-skip' if skip else '-copy'),reuse=layout=='runner',skip=skip,type='XCTEST' if layout=='unit' else 'XCUITEST',test='UnitTests.xctest' if layout=='unit' else 'UITests-Runner.app/PlugIns/UITests.xctest')
for id,reuse,test,typ,missing in [
 ('missing-test',False,'UITests-Runner.app/PlugIns/UITests.xctest','XCUITEST','marathon.xctest/UITests'),
 ('missing-runner-test',True,'UITests-Runner.app/PlugIns/UITests.xctest','XCUITEST','xctestRunner.app/PlugIns/UITests.xctest/UITests'),
 ('missing-runner-binary',True,'UITests-Runner.app/PlugIns/UITests.xctest','XCUITEST','xctestRunner.app/UITests-Runner'),
 ('missing-app',False,'UITests-Runner.app/PlugIns/UITests.xctest','XCUITEST','appUnderTest.app/CloudApp'),
 ('missing-hosted-test',False,'UnitTests.xctest','XCTEST','appUnderTest.app/PlugIns/UnitTests.xctest/UnitTests')]:
    case(id,skip=True,reuse=reuse,test=test,type=typ,missing='/tmp/marathon/shared/'+missing)
for reuse in [False,True]:
    for mode in ['retry-success','retry-fail','retry-throw','retry-cancel','delay-cancel']:
        case(f'{mode}-{reuse}',reuse=reuse,mode=mode)
for sdk in ['IPHONESIMULATOR','IPHONEOS','MACOS']:
    for raw in [False,True]:
        for typ in ['XCTEST','XCUITEST','LOGIC_TEST','detect-empty','detect-ui','detect-kif','detect-both']:
            case(f'type-{sdk}-{raw}-{typ}',sdk=sdk,raw=raw,type=typ,test='UITests.xctest')
case('derived-data-ui',raw=False,derived=True,test='UITests.xctest',omitRunner=True)
case('derived-data-unit',raw=False,derived=True,type='XCTEST',test='UITests.xctest',omitRunner=True)
for mode in ['no-env','no-result','no-bundle','no-app','extras','extras-false','hook-fail']:
    case(mode,mode=mode)
for reuse in [False,True]:
    for unknown in [False,True]:
        case(f'parsing-error-{reuse}-{unknown}',reuse=reuse,requireOnly=True,missing='/tmp/marathon/shared/'+('xctestRunner.app/UITests-Runner' if reuse else 'marathon.xctest/UITests'),unknown=unknown)
case('macos-real',sdk='MACOS',real=True,app='AppleWorld.app',runner='AppleWorldUITests-Runner.app',test='AppleWorldUITests-Runner.app/Contents/PlugIns/AppleWorldUITests.xctest',reuse=True)
case('parser-enabled',parser=True)
case('default-hook',noop=True)
for reuse in [False,True]: case(f'parsing-present-{reuse}',reuse=reuse,requireOnly=True)
for unknown in [False,True]: case(f'parsing-runner-test-{unknown}',reuse=True,requireOnly=True,missing='/tmp/marathon/shared/xctestRunner.app/PlugIns/UITests.xctest/UITests',unknown=unknown)
case('outside-runner',reuse=True,test='Outside.xctest')
for mode in ['mkdir-fail','mkdir-cancel']: case(mode,mode=mode)
case('unknown-test',skip=True,missing='/tmp/marathon/shared/marathon.xctest/UITests',unknown=True)

def prepare(root,c):
    root.mkdir()
    if c.get('real'):
        with zipfile.ZipFile(repo/'fixtures/bundles/macos.zip') as z: z.extractall(root)
        return
    mac=c['sdk']=='MACOS'
    platform={'IPHONESIMULATOR':'iPhoneSimulator','IPHONEOS':'iPhoneOS','MACOS':'MacOSX'}[c['sdk']]
    for key,name in [('app','CloudApp'),('runner','UITests-Runner'),('test','UnitTests' if c['test']=='UnitTests.xctest' else 'UITests')]:
        if c.get('omitRunner') and key=='runner': continue
        folder=root/c[key]/('Contents' if mac else '')
        binary=folder/('MacOS' if mac else '')
        binary.mkdir(parents=True,exist_ok=True)
        # Exact SkipCopyingDerivedDataTest bundle bytes (plus macOS's required minimum).
        text=f'''<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
    <key>CFBundleIdentifier</key><string>com.example.{name}</string>
    <key>CFBundleName</key><string>{name}</string>
    <key>CFBundleSupportedPlatforms</key><array><string>{platform}</string></array>
</dict>
</plist>'''
        if mac: text=text.replace('</dict>','    <key>LSMinimumSystemVersion</key><string>10.0</string>\n</dict>')
        (folder/'Info.plist').write_text(text)
        (binary/name).write_bytes(bytes.fromhex('cffaedfe00000000'))
    if c.get('mode','').startswith('extras'):
        for name in ['first.app','second.app','wrong.APP']: (root/name).mkdir()
        (root/'.app').write_text('file')

def main():
    out=pathlib.Path(sys.argv[1]) if len(sys.argv)>1 else repo/'fixtures/oracle/installer/probes.jsonl'
    with tempfile.TemporaryDirectory(prefix='mrs410b-oracle-', dir='/tmp') as temp:
        temp=pathlib.Path(temp); classes=temp/'classes'; bundles=temp/'bundles'; bundles.mkdir()
        files.extend([base/'AppleApplicationInstaller.kt',base/'cmd/SharedTransfers.kt',base/'extensions/ConfigurationExtensions.kt'])
        subprocess.run([java,'-cp',os.pathsep.join(map(str,compiler)),'org.jetbrains.kotlin.cli.jvm.K2JVMCompiler','-no-stdlib','-no-reflect','-nowarn','-jvm-target','17','-cp',cp,'-d',str(classes),str(here/'Probe.kt'),*map(str,files)],check=True)
        for c in cases: prepare(bundles/c['id'],c)
        inputs=temp/'inputs.jsonl'; inputs.write_text(''.join(json.dumps(c,ensure_ascii=False)+'\n' for c in cases))
        result=subprocess.run([java,'-cp',str(classes)+os.pathsep+cp,'ProbeKt',str(inputs),str(bundles)],check=True,stdout=subprocess.PIPE,timeout=120)
        lines=[l for l in result.stdout.decode().splitlines() if l.startswith('{"input"')]
        assert len(lines)==len(cases),(len(lines),len(cases))
        out.parent.mkdir(parents=True,exist_ok=True);out.write_text('\n'.join(lines)+'\n')
        print(f'{len(lines)} cases -> {out}')

if __name__=="__main__": main()
