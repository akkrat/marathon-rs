#!/usr/bin/env python3
"""Run the unmodified e47352f2 factory against a recording device; never edit reference sources."""
import json, os, pathlib, plistlib, subprocess, sys, tempfile, zipfile
here=pathlib.Path(__file__).resolve().parent
repo=here.parents[2]
source=pathlib.Path(os.environ.get('MARATHON_KT',os.path.expanduser('~/src/marathon')))
lib=pathlib.Path(os.environ.get('MARATHON_KOTLIN_LIB',str(source/'cli/build/install/marathon/lib')))
cache=pathlib.Path.home()/'.gradle/caches/modules-2/files-2.1'
def jar(g,a,v): return next((cache/g/a/v).glob('*/*.jar'))
compiler=[jar('org.jetbrains.kotlin','kotlin-compiler-embeddable','1.9.24'),jar('org.jetbrains.kotlin','kotlin-stdlib','1.9.24'),jar('org.jetbrains.kotlin','kotlin-script-runtime','1.9.24'),jar('org.jetbrains.kotlin','kotlin-reflect','1.6.10'),jar('org.jetbrains.intellij.deps','trove4j','1.0.20200330'),jar('org.jetbrains','annotations','13.0')]
java=str(pathlib.Path(os.environ.get('JAVA_HOME','/Applications/Android Studio.app/Contents/jbr/Contents/Home'))/'bin/java')
cp=os.pathsep.join(map(str, sorted(lib.glob('*.jar'))))
base=source/'vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple'
files=[base/'AppleDevice.kt',base/'RemoteFileManager.kt',base/'DistributedDerivedData.kt',base/'xctestrun/TestRootFactory.kt',*sorted((base/'bin').rglob('*.kt')),*sorted((base/'model').glob('*.kt'))]
cases=[]
def case(id,**kw):
    c=dict(id=id,sdk='IPHONESIMULATOR',type='XCUITEST',reuse=True,skip=False,parser=False,app='CloudApp.app',runner='UITests-Runner.app',test='UITests-Runner.app/PlugIns/UITests.xctest')
    c.update(kw)
    if c['sdk']=='MACOS' and 'test' not in kw: c['test']='UITests-Runner.app/Contents/PlugIns/UITests.xctest'
    cases.append(c)
for sdk in ['IPHONESIMULATOR','MACOS']:
    for typ in ['XCUITEST','XCTEST']:
        for reuse in [False,True] if typ=='XCUITEST' else [False]:
            for skip in [False,True]:
                for parser in [False,True]:
                    case(f'{sdk}-{typ}-{reuse}-{skip}-{parser}',sdk=sdk,type=typ,reuse=reuse,skip=skip,parser=parser)
for sdk in ['IPHONESIMULATOR','MACOS','TV_SIMULATOR']:
    for arch in ['arm64','x86_64','arm64e']:
        for test in ['arm64','x86_64','arm64 x86_64','arm64e']:
            for runner in ['arm64','arm64 arm64e','x86_64']:
                case(f'arch-{sdk}-{arch}-{test}-{runner}',sdk=sdk,arch=arch,testArch=test,runnerArch=runner)
for sdk in ['IPHONESIMULATOR','MACOS']:
    for typ in ['XCUITEST','XCTEST']:
        case(f'env-{sdk}-{typ}',sdk=sdk,type=typ,parser=True,frameworks=True,dylibs=True,appEnv=[['A','a<&😀]]>']],testEnv=[['DYLD_FRAMEWORK_PATH',': \u00a0:x: x :x::'],['DYLD_LIBRARY_PATH','lib:lib:\t'],['DYLD_INSERT_LIBRARIES','i::i'],['XCInjectBundleInto','override'],['Z','😀<&]]>']],capture='SCREEN_RECORDING',systemLifetime='KEEP_NEVER',userLifetime='DELETE_ON_SUCCESS')
        case(f'framework-contents-{sdk}-{typ}',sdk=sdk,type=typ,contentsFrameworks=True,uppercaseDylib=True)
        case(f'framework-file-{sdk}-{typ}',sdk=sdk,type=typ,frameworkFile=True,dylibDirectory=True,capture='SCREENSHOTS')
for mode in ['exists-missing','exists-unknown','exists-test-missing','exists-test-unknown','exists-throw','push-app-false','push-test-false','push-false','push-throw','push-cancel','delete-fail','plist-false']:
    case(mode,type='XCTEST' if 'test' in mode or mode.startswith('exists') else 'XCUITEST',reuse=False,skip=mode.startswith('exists'),mode=mode)
for typ in ['XCTEST','XCUITEST','LOGIC_TEST']:
    case(f'phone-{typ}',sdk='IPHONEOS',type=typ,reuse=False)
    case(f'logic-{typ}',type=typ,noApp=typ!='LOGIC_TEST')
for edit in ['app-platforms','test-platforms','absent-platforms','minimum-absent','minimum-old','minimum-invalid','both-invalid']:
    case(edit,sdk='MACOS' if edit.startswith('minimum') else 'IPHONESIMULATOR',edit=edit)
case('no-binary-distributed',skip=True,noBinary=True)
case('no-binary-missing',skip=True,noBinary=True,mode='exists-missing')
case('relative-outside-runner',test='Outside.xctest')
case('minimum-both-invalid',sdk='MACOS',edit='minimum-invalid',os='also-bad')
case('a9-embedded',type='XCTEST',test='CloudApp.app/PlugIns/UITests.xctest',reuse=False)
for sdk,app,runner,test,udid in [('IPHONESIMULATOR','sample-app.app','sample-appUITests-Runner.app','sample-appUITests-Runner.app/PlugIns/sample-appUITests.xctest','00000000-0000-0000-0000-000000000001'),('MACOS','AppleWorld.app','AppleWorldUITests-Runner.app','AppleWorldUITests-Runner.app/Contents/PlugIns/AppleWorldUITests.xctest','FAKE-MAC')]:
    case('real-'+sdk,sdk=sdk,app=app,runner=runner,test=test,real=True,udid=udid)

case("baseline-ios",sdk="IPHONESIMULATOR",type="XCTEST",reuse=False,real=True,app="sample-app.app",test="sample-app.app/PlugIns/sample-appUnitTests.xctest")
for parser in [False,True]:
    case("baseline-macos-"+str(parser),sdk="MACOS",type="XCTEST",reuse=False,real=True,parser=parser,app="AppleWorld.app",test="AppleWorld.app/Contents/PlugIns/AppleWorldTests.xctest")

case('temp-prefix-slash',type='XCTEST',reuse=False,udid='prefix/with/slash',prefixEdge=True)
case("writer-fail",mode="writer-fail")
for field in ["appEnv","testEnv"]: case("surrogate-"+field,surrogateField=field)

def prepare(root,c):
    root.mkdir()
    if c.get('real'):
        with zipfile.ZipFile(repo/'fixtures/bundles'/('macos.zip' if c['sdk']=='MACOS' else 'ios.zip')) as z: z.extractall(root)
        return
    mac=c['sdk']=='MACOS'
    platform={'IPHONESIMULATOR':'iPhoneSimulator','IPHONEOS':'iPhoneOS','MACOS':'MacOSX','TV_SIMULATOR':'AppleTVSimulator'}[c['sdk']]
    for key,name in [('app','CloudApp'),('runner','UITests-Runner'),('test','UITests')]:
        folder=root/c[key]/('Contents' if mac else '')
        binary=folder/('MacOS' if mac else '')
        binary.mkdir(parents=True,exist_ok=True)
        info=dict(CFBundleIdentifier='com.example.'+name,CFBundleName=name,CFBundleSupportedPlatforms=[platform],LSMinimumSystemVersion='10.0')
        edit=c.get('edit','')
        if edit==key+'-platforms' or edit=='both-invalid': info['CFBundleSupportedPlatforms']=['Other','Second']
        if edit=='absent-platforms' and key=='app': del info['CFBundleSupportedPlatforms']
        if edit=='minimum-absent' and key=='app': del info['LSMinimumSystemVersion']
        if edit=='minimum-old' and key=='app': info['LSMinimumSystemVersion']='99.0'
        if edit=='minimum-invalid' and key=='app': info['LSMinimumSystemVersion']='bad'
        (folder/'Info.plist').write_bytes(plistlib.dumps(info))
        if not (c.get('noBinary') and key=='app'): (binary/name).write_bytes(bytes.fromhex('cffaedfe00000000'))
    if c.get('frameworks'): (root/c['app']/'Frameworks').mkdir()
    if c.get('frameworkFile'): (root/c['app']/'Frameworks').write_text('file')
    if c.get('contentsFrameworks'): (root/c['app']/'Contents/Frameworks').mkdir(parents=True)
    if c.get('dylibs') or c.get('uppercaseDylib') or c.get('dylibDirectory'):
        folder=root/c['runner']/('Contents/Frameworks' if mac else 'Frameworks'); folder.mkdir(parents=True)
        if c.get('dylibDirectory'): (folder/'directory.dylib').mkdir()
        else: (folder/('lib.DYLIB' if c.get('uppercaseDylib') else 'lib.dylib')).write_text('file')

out=pathlib.Path(sys.argv[1]) if len(sys.argv)>1 else repo/'fixtures/oracle/test-root/probes.jsonl'
with tempfile.TemporaryDirectory(prefix='mrs45b-oracle-') as temp:
    temp=pathlib.Path(temp); classes=temp/'classes'; bundles=temp/'bundles'; bundles.mkdir()
    subprocess.run([java,'-cp',os.pathsep.join(map(str,compiler)),'org.jetbrains.kotlin.cli.jvm.K2JVMCompiler','-no-stdlib','-no-reflect','-nowarn','-jvm-target','17','-cp',cp,'-d',str(classes),str(here/'Probe.kt'),*map(str,files)],check=True)
    for c in cases: prepare(bundles/c['id'],c)
    inputs=temp/'inputs.jsonl'; inputs.write_text(''.join(json.dumps(c,ensure_ascii=False)+'\n' for c in cases))
    result=subprocess.run([java,'-Djava.security.manager=allow','-cp',str(classes)+os.pathsep+cp,'ProbeKt',str(inputs),str(bundles)],check=True,stdout=subprocess.PIPE,timeout=120)
    lines=[l for l in result.stdout.decode().splitlines() if l.startswith('{"input"')]
    assert len(lines)==len(cases),(len(lines),len(cases))
    out.parent.mkdir(parents=True,exist_ok=True)
    for name, selected in [('probes.jsonl',[l for l in lines if not json.loads(l)['input'].get('surrogateField') and not json.loads(l)['input'].get('prefixEdge') and json.loads(l)['input'].get('mode')!='writer-fail']),('staging-edges.jsonl',[l for l in lines if json.loads(l)['input'].get('surrogateField') or json.loads(l)['input'].get('mode')=='writer-fail']),('temp-prefixes.jsonl',[l for l in lines if json.loads(l)['input'].get('prefixEdge')])]:
        (out if name=='probes.jsonl' else out.with_name(name)).write_text('\n'.join(selected)+'\n')
    print(f'{len(lines)} cases -> {out}')
