#!/usr/bin/env python3
"""Release installer return/argv/XML witness, reusing the established recording-device probe."""
import hashlib, importlib.util, json, os, pathlib, subprocess, sys, tempfile
HERE = pathlib.Path(__file__).resolve().parent
ROOT = HERE.parents[2]
SHA = 'c95b212882c7ba7b1ad91b7981916ed175f1530e'
SOURCE = pathlib.Path(os.environ.get('MARATHON_KT','/tmp/mrs-7.5-src'))
assert subprocess.check_output(['git','-C',str(SOURCE),'rev-parse','HEAD'],text=True).strip() == SHA
assert subprocess.check_output(['git','-C',str(ROOT/f'tools/oracle/build-{SHA}/marathon-src'),'rev-parse','HEAD'],text=True).strip() == SHA
os.environ['MARATHON_KT'] = str(SOURCE)
os.environ['MARATHON_KOTLIN_LIB'] = str(ROOT/f'tools/oracle/build-{SHA}/marathon-src/cli/build/install/marathon/lib')
spec = importlib.util.spec_from_file_location('installer',HERE.parent/'installer-probes/run.py')
p = importlib.util.module_from_spec(spec); spec.loader.exec_module(p)
probe = (HERE.parent/'installer-probes/Probe.kt').read_text()
probe = probe.replace('derivedDataDir=if(input.b("derived")) root else null)', 'derivedDataDir=if(input.b("derived")) root else null,xctestrun=if(input.b("coverageFile")) File(root,"AllTests.xctestrun") else null)')
probe = probe.replace('installer.prepareInstallation(fake,input.b("parser"))','installer.prepareInstallation(fake,input.b("parser"),codeCoverage=input.b("coverage"))')
# A failure after static-product directory creation exercises the source's transfer-error precedence.
p.cases.clear()
for sdk in ['IPHONESIMULATOR','MACOS']:
    for enabled in [False,True]:
        p.case(sdk+'-'+str(enabled),sdk=sdk,skip=True,coverage=enabled,coverageFile=True)
    p.case(sdk+'-hosted',sdk=sdk,type='XCTEST',skip=True,coverage=True,coverageFile=True,test='UnitTests.xctest')
for n in range(2): p.case('missing-'+str(n),coverage=True)
p.case('empty',coverage=True,coverageFile=True,empty=True)

def prepare(root,c):
    p.prepare(root,c)
    if not c.get('coverageFile'): return
    static = root/'Debug-iphonesimulator/Static.framework/Static'; static.parent.mkdir(parents=True); static.write_bytes(b'!<arch>\n')
    def info(name,path,static=False):
        return f'<dict><key>Name</key><string>{name}</string><key>IsStatic</key><{str(static).lower()}/><key>ProductPath</key><string>__TESTROOT__/{path}</string><key>Architecture</key><string>arm64</string></dict>'
    binary = '/Contents/MacOS/' if c['sdk']=='MACOS' else '/'
    contents = '' if c.get('empty') else info('CloudApp.app',c['app']+binary+'CloudApp')+info('UITests.xctest',c['test']+binary+('UnitTests' if c['test']=='UnitTests.xctest' else 'UITests'))+info('Static.framework','Debug-iphonesimulator/Static.framework/Static',True)
    (root/'AllTests.xctestrun').write_text('<?xml version="1.0" encoding="UTF-8"?>\n<plist version="1.0"><dict><key>__xctestrun_metadata__</key><dict><key>FormatVersion</key><integer>1</integer><key>CodeCoverageBuildableInfos</key><array>'+contents+'</array></dict></dict></plist>')

def capture(out):
    with tempfile.TemporaryDirectory(prefix='mrs-coverage-installer-') as tmp:
        tmp=pathlib.Path(tmp); classes=tmp/'classes'; bundles=tmp/'bundles'; bundles.mkdir()
        source=tmp/'Probe.kt'; source.write_text(probe)
        subprocess.run([p.java,'-cp',os.pathsep.join(map(str,p.compiler)),'org.jetbrains.kotlin.cli.jvm.K2JVMCompiler','-no-stdlib','-no-reflect','-nowarn','-jvm-target','17','-Xfriend-paths='+str(next(p.lib.glob('base-*.jar'))),'-cp',p.cp,'-d',str(classes),str(source)],check=True)
        for c in p.cases: prepare(bundles/c['id'],c)
        inputs=tmp/'inputs.jsonl'; inputs.write_text(''.join(json.dumps(c)+'\n' for c in p.cases))
        result=subprocess.run([p.java,'-cp',str(classes)+os.pathsep+p.cp,'ProbeKt',str(inputs),str(bundles)],check=True,stdout=subprocess.PIPE,timeout=120)
        lines=[l for l in result.stdout.decode().splitlines() if l.startswith('{"input"')]
        assert len(lines)==len(p.cases)
        out.write_text('\n'.join(lines)+'\n')
out=ROOT/'fixtures/oracle/coverage-c95b2128/installer.jsonl'
capture(out)
with tempfile.TemporaryDirectory(prefix='mrs-coverage-repeat-') as tmp:
    repeat=pathlib.Path(tmp)/'repeat.jsonl';capture(repeat);assert out.read_bytes()==repeat.read_bytes()
metadata=dict(source_sha=SHA,tests=len(p.cases),repeat='byte-identical',probe_sha256=hashlib.sha256(probe.encode()).hexdigest(),fixture_sha256=hashlib.sha256(out.read_bytes()).hexdigest())
out.with_name('installer-metadata.json').write_text(json.dumps(metadata,indent=2)+'\n')
print(f'{len(p.cases)} installer cases -> {out}')
