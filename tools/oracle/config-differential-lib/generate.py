"""Deterministic bounded key/operator corpus; no production parsing or rendering."""
import copy
import hashlib
import gzip
import json
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[3]
OUT = ROOT / 'fixtures/oracle/config-differential'
OPS = ['drop','wrong-type','unknown-sibling','quote','literal','substitution','duplicate','tag','alias','null']

class Raw(str): pass
class Dup:
    def __init__(self,a,b): self.a,self.b=a,b

def emit(v, indent=0):
    pad = ' ' * indent
    if isinstance(v, Raw): return str(v)
    if isinstance(v, dict) and v:
        lines = []
        for k,x in v.items():
            values = [x.a,x.b] if isinstance(x,Dup) else [x]
            for x in values:
                key = json.dumps(k, ensure_ascii=False) + ':'
                if isinstance(x,(dict,list)) and x:
                    lines.append(pad+key+'\n'+emit(x,indent+2))
                else:
                    text=emit(x,indent+2)
                    lines.append(pad+key+' '+text)
        return '\n'.join(lines)
    if isinstance(v,list) and v:
        return '\n'.join(pad+'- '+( ('\n'+emit(x,indent+2)) if isinstance(x,(dict,list)) and x else emit(x,indent+2)) for x in v)
    return json.dumps(v,ensure_ascii=False)

def transformed(v):
    if isinstance(v,dict): return {k:transformed(x) for k,x in v.items()}
    if isinstance(v,list): return [transformed(x) for x in v]
    if isinstance(v,str): return v.replace('$DIR','@ROOT@/config')
    return v

def main():
    cases=[]; coverage=[]
    def add(id,text=None,**kw):
        case={'id':id,**kw}
        if text is not None: case['text']=text+'\n'
        cases.append(case)
        return id
    # The 30 original goldens are independently replayed by their existing Rust test,
    # and are freshly recorded here through the same immutable seed tree.
    for p in sorted((ROOT/'fixtures/oracle/config-dump').glob('*.json')):
        meta=json.loads(p.read_text())['golden']
        add('golden-'+p.stem, input=meta['input'],env=meta['env'],base='fixtures/oracle/config-dump/'+p.name,
            operator='baseline',vendor='fixture')
    for line in (ROOT/'fixtures/oracle/config-validation-corpus.jsonl').read_text().splitlines()[1:]:
        r=json.loads(line)
        args={'base':'config-validation:'+r['id'],'operator':'baseline','vendor':'validation',
              'env':dict(({} if r['noSdk'] else {'ANDROID_HOME':'/android-sdk'}), A='${B}', B='${A}')}
        if r['kind']=='fixture':
            add('validation-'+r['id'].replace('/','__'),input=r['input'],**args)
        else:
            add('validation-'+r['id'],r['input'].replace('@DIR@','@ROOT@/config'),setup='validation',
                linux=r['id'] in ['a-devices-symlink-loop','a-known-hosts-symlink-loop','a-bundle-before-devices'],**args)
    seeds=json.loads((OUT/'seeds.json').read_text())
    # Optional keys omitted by serialize() still need reachable, typed inputs.
    byid={s['id']:s for s in seeds}
    for seed in seeds:
        parser=seed['value']['vendorConfiguration'].get('testParserConfiguration',{})
        if parser.get('testClassRegexes')==['^((?!Abstract).)*Test[s]*$']:
            # Serialize emits the built-in lookahead; omitting it rebinds the
            # approved built-in default without turning every seed into D6.
            del parser['testClassRegexes']

    android=byid['m-android-full']['value']['vendorConfiguration']
    apple=byid['m-ios-full']['value']['vendorConfiguration']
    android['testParserConfiguration']={'type':'local'}
    apple['bundle'].update(application='@ROOT@/config/derivedDataDir/sample.app',
                            testApplication='@ROOT@/config/derivedDataDir/ui.xctest',extraApplications=[])
    # Keep the separate remote parser seed, and absolute success-rate/time variants.
    extra=copy.deepcopy(byid['m-android-full'])
    extra['id']='android-remote'; extra['value']['vendorConfiguration']['testParserConfiguration']={'type':'remote','instrumentationArgs':{'listener':'x'}}
    seeds.append(extra)
    static=copy.deepcopy(byid['m-ios-full']); static['id']='ios-static'
    static['value']['vendorConfiguration']['deviceProvider']={'type':'static','path':'@ROOT@/config/Marathondevices'}
    seeds.append(static)
    seen=set()
    def fields(v,path=(),ctx=()):
        if isinstance(v,dict):
            here=ctx+((str(v['type']),) if 'type' in v else ())
            for k,x in v.items():
                if k=='vendorConfiguration': childctx=('vendor',str(x.get('type')))
                else: childctx=here
                yield path+(k,),childctx,x
                # Arbitrary map keys are represented by one value witness, rather
                # than pretending their names are an enumerable schema.
                yield from fields(x,path+(k,),childctx)
        elif isinstance(v,list):
            for i,x in enumerate(v): yield from fields(x,path+(i,),ctx)
    for seed in seeds:
        value=transformed(seed['value'])
        add('seed-'+seed['id'],emit(value),base=seed['source']+'#'+seed['id'],operator='seed',vendor=value['vendorConfiguration']['type'],setup='validation')
        for path,ctx,old in fields(value):
            signature=(tuple('[]' if isinstance(p,int) else p for p in path),ctx)
            if signature in seen: continue
            seen.add(signature)
            key='.'.join(str(p) for p in path)
            slug=hashlib.sha256(repr(signature).encode()).hexdigest()[:12]
            ledger={'field':key,'variant':list(ctx),'seed':seed['id'],'operators':{}}
            for op in OPS:
                data=copy.deepcopy(value); parent=data
                for p in path[:-1]: parent=parent[p]
                k=path[-1]
                if op=='drop': del parent[k]
                elif op=='wrong-type': parent[k]=[] if not isinstance(old,list) else {'bad':'shape'}
                elif op=='unknown-sibling': parent['unknownDifferentialKey']=True
                elif op=='quote': parent[k]=json.dumps(old,ensure_ascii=False,separators=(',',':')) if not isinstance(old,str) else old
                elif op=='literal': parent[k]=Raw('|-\n'+' '*(len(path)*2+2)+json.dumps(old,ensure_ascii=False))
                elif op=='substitution': parent[k]='${MRS26_VALUE:-'+(old if isinstance(old,str) else json.dumps(old,separators=(',',':')))+'}'
                elif op=='duplicate': parent[k]=Dup(old,old)
                elif op=='tag': parent[k]=Raw('!probe '+json.dumps(old,ensure_ascii=False))
                elif op=='alias': parent[k]=Dup(Raw('&mrs26 '+json.dumps(old,ensure_ascii=False)),Raw('*mrs26'))
                elif op=='null': parent[k]=None
                id='mutation-'+slug+'-'+op
                ledger['operators'][op]={'case':id}
                add(id,emit(data),base='seed-'+seed['id'],field=key,operator=op,
                    vendor=value['vendorConfiguration']['type'],setup='validation')
            coverage.append(ledger)
    base='name: t\noutputDir: out\nvendorConfiguration: {type: stub}\n'
    for id,text,env in [
        ('u49-tab','name: t\noutputDir: out\nvendorConfiguration:\n\ttype: stub',{}),
        ('u49-eof','name: t\noutputDir: out\nvendorConfiguration: [',{}),
        ('r1-wrapper-comma','name: t\noutputDir: out\nvendorConfiguration: [stub,',{}),
        ('r1-wrapper-eof','name: t\noutputDir: out\nvendorConfiguration: [stub',{}),
        ('r1-wrapper-end','name: t\noutputDir: out\nvendorConfiguration: [stub]',{}),
        ('r1-wrapper-body','name: t\noutputDir: out\nvendorConfiguration: [stub, {}]',{}),
        ('r1-wrapper-extra','name: t\noutputDir: out\nvendorConfiguration: [stub, {}, {}]',{}),
        ('r1-wrapper-body-eof','name: t\noutputDir: out\nvendorConfiguration: [stub, {}',{}),
        ('r1-wrapper-unknown-end','name: t\noutputDir: out\nvendorConfiguration: [unknown]',{}),
        ('r1-p2-null-type-id','name: t\noutputDir: out\nvendorConfiguration: [null, {}]\n',{}),
        ('r2-regex-int',base+'includeSerialRegexes: [1]',{}),
        ('r2-regex-yes',base+'includeSerialRegexes: [yes]',{}),
        ('r2-regex-false',base+'includeSerialRegexes: [false]',{}),
        ('r2-regex-float',base+'includeSerialRegexes: [1.5]',{}),
        ('r2-regex-binary',base+'includeSerialRegexes: [!!binary aGk=]',{}),
        ('r2-regex-filter',base+'filteringConfiguration: {allowlist: [{type: simple-class-name, regex: 5}]}',{}),
        ('r2-reader-control','name: \x01t\noutputDir: out\nvendorConfiguration: {type: stub}\n',{}),
        ('r2-reader-control-comment',base+'# \x01',{}),
        ('r2-code-point-limit','name: "'+'a'*(3*1024*1024+10)+'"\noutputDir: out\nvendorConfiguration: {type: stub}\n',{}),
        ('r2-code-point-unknown',base+'x: "'+'a'*(3*1024*1024+10)+'"',{}),
        ('r2-creator-root-drop-name','outputDir: out\nunknownDifferentialKey: 1\nvendorConfiguration: {type: stub}\n',{}),
        ('r2-creator-root-null-name','name: null\nunknownDifferentialKey: 1\noutputDir: out\nvendorConfiguration: {type: stub}\n',{}),
        ('r2-creator-root-drop-name-late','outputDir: out\nvendorConfiguration: {type: stub}\nunknownDifferentialKey: 1\n',{}),
        ('r2-creator-root-later-type','unknownDifferentialKey: 1\nname: t\noutputDir: out\ndebug: []\nvendorConfiguration: {type: stub}\n',{}),
        ('r2-creator-root-later-eof','name: t\noutputDir: out\nunknownDifferentialKey: 1\nvendorConfiguration: {type: stub}\ny: [1\n',{}),
        ('r2-creator-root-two-unknown','name: t\noutputDir: out\nb: 1\na: 2\nvendorConfiguration: {type: stub}\n',{}),
        ('r2-creator-nested-then-outer','name: t\noutputDir: out\nvendorConfiguration: {type: stub}\noutputConfiguration: {unknownDifferentialKey: 1}\ndebug: []\n',{}),
        ('r2-creator-composition-drop-op','name: t\noutputDir: out\nvendorConfiguration: {type: stub}\nfilteringConfiguration: {allowlist: [{type: composition, filters: [], unknownDifferentialKey: 1}]}\n',{}),
        ('r2-creator-composition-drop-both','name: t\noutputDir: out\nvendorConfiguration: {type: stub}\nfilteringConfiguration: {allowlist: [{type: composition, unknownDifferentialKey: 1}]}\n',{}),
        ('r2-creator-composition-drop-filters','name: t\noutputDir: out\nvendorConfiguration: {type: stub}\nfilteringConfiguration: {allowlist: [{type: composition, op: UNION, unknownDifferentialKey: 1}]}\n',{}),
        ('r2-creator-composition-later-op','name: t\noutputDir: out\nvendorConfiguration: {type: stub}\nfilteringConfiguration: {allowlist: [{type: composition, unknownDifferentialKey: 1, filters: [], op: []}]}\n',{}),
        ('r2-creator-annotation-drop-name','name: t\noutputDir: out\nvendorConfiguration: {type: stub}\nfilteringConfiguration: {allowlist: [{type: annotationData, valueRegex: a, unknownDifferentialKey: 1}]}\n',{}),
        ('r2-creator-fragmentation-later-type','name: t\noutputDir: out\nvendorConfiguration: {type: stub}\nfilteringConfiguration: {allowlist: [{type: fragmentation, unknownDifferentialKey: 1, index: []}]}\n',{}),
        ('r2-creator-simple-later-regex','name: t\noutputDir: out\nvendorConfiguration: {type: stub}\nfilteringConfiguration: {allowlist: [{type: simple-class-name, unknownDifferentialKey: 1, regex: []}]}\n',{}),
        ('r2-creator-filtering-later-type','name: t\noutputDir: out\nvendorConfiguration: {type: stub}\nfilteringConfiguration: {unknownDifferentialKey: 1, allowlist: 5}\n',{}),
        ('r2-creator-output-later-type','name: t\noutputDir: out\nvendorConfiguration: {type: stub}\noutputConfiguration: {unknownDifferentialKey: 1, maxPath: []}\n',{}),
        ('r2-creator-execution-later-type','name: t\noutputDir: out\nvendorConfiguration: {type: stub}\nexecutionStrategy: {unknownDifferentialKey: 1, mode: []}\n',{}),
        ('r2-creator-retry-later-type','name: t\noutputDir: out\nvendorConfiguration: {type: stub}\nretryStrategy: {type: fixed-quota, unknownDifferentialKey: 1, totalAllowedRetryQuota: []}\n',{}),
        ('r2-creator-sorting-later-type','name: t\noutputDir: out\nvendorConfiguration: {type: stub}\nsortingStrategy: {type: success-rate, unknownDifferentialKey: 1, timeLimit: []}\n',{}),
        ('r2-creator-singleton-later-eof','name: t\noutputDir: out\nvendorConfiguration: {type: stub}\npoolingStrategy: {type: omni, unknownDifferentialKey: 1, y: [1\n',{}),
        ('r2-creator-android-drop-sdk','name: t\noutputDir: out\nvendorConfiguration: {type: Android, unknownDifferentialKey: 1}\n',{}),
        ('r2-creator-android-later-type','name: t\noutputDir: out\nvendorConfiguration: {type: Android, androidSdk: /s, unknownDifferentialKey: 1, autoGrantPermission: []}\n',{}),
        ('r2-creator-wrapper-later-type','name: t\noutputDir: out\nvendorConfiguration: [Android, {androidSdk: /s, unknownDifferentialKey: 1, autoGrantPermission: []}]\n',{}),
        ('r2-creator-ios-later-bundle','name: t\noutputDir: out\nvendorConfiguration: {type: iOS, unknownDifferentialKey: 1, bundle: 5}\n',{}),
        ('r2-creator-ios-ssh-later-type','name: t\noutputDir: out\nvendorConfiguration: {type: iOS, ssh: {unknownDifferentialKey: 1, authentication: []}}\n',{}),
        ('subst-value',base.replace('name: t','name: ${V}'),{'V':'value'}),
        ('subst-default',base.replace('name: t','name: ${V:-d}'),{}),
        ('subst-escape',base.replace('name: t','name: $${V}'),{'V':'value'}),
        ('subst-undefined',base.replace('name: t','name: ${V}'),{}),
        ('subst-empty',base.replace('name: t','name: ${V:-d}'),{'V':''}),
        ('subst-recursive',base.replace('name: t','name: ${V}'),{'V':'${W}','W':'value'}),
        ('subst-cycle',base.replace('name: t','name: ${V}'),{'V':'${W}','W':'${V}'}),
        ('subst-name',base.replace('name: t','name: ${A_${B}}'),{'B':'x','A_x':'nested'}),
        ('first-document',base+'---\nunknownKey: true',{}),
        ('yaml-literals',base.replace('name: t','name: yes').replace('outputDir: out','outputDir: 0123'),{}),
        ('d9-filter-null','name: t\noutputDir: out\nvendorConfiguration: {type: stub}\nfilteringConfiguration: {allowlist: [{type: simple-class-name, values: [a, null, b]}]}',{}),
        ('d9-list-null',base+'includeSerialRegexes: [null]',{}),
        ('d9-map-null','name: t\noutputDir: out\nvendorConfiguration: {type: Android, instrumentationArgs: {a: null}}',{'ANDROID_HOME':'/sdk'}),
        ('d6-regex',base+'includeSerialRegexes: ["(?=a)a"]',{}),
    ]:
        add(id,text,env=env,base='focused',operator='interaction',vendor='stub',setup='empty')
    for alias in ['iOS','watchOS','tvOS','visionOS','macOS','Android','stub','empty']:
        add('alias-'+alias,'name: t\noutputDir: out\nvendorConfiguration: {type: '+alias+'}',base='focused',operator='alias',vendor=alias)
    for id,env,explicit in [('sdk-home',{'ANDROID_HOME':'/home-sdk','ANDROID_SDK_ROOT':'/root-sdk'},''),
                            ('sdk-root',{'ANDROID_SDK_ROOT':'/root-sdk'},''),('sdk-empty',{},''),
                            ('sdk-explicit',{'ANDROID_HOME':'/home-sdk'},', androidSdk: /explicit-sdk')]:
        add(id,'name: t\noutputDir: out\nvendorConfiguration: {type: Android'+explicit+'}',env=env,base='focused',operator='interaction',vendor='Android')
    # Net-effect snapshots: deletion, changed bytes, creations, links retained,
    # partial parse effects and extraction surviving a later logical rejection.
    for id,tail in [('effects-ok',''),('effects-parse','\n  devices: loop/Marathondevices'),
                    ('effects-logical','\nexecutionStrategy: {mode: ALL_SUCCESS}\nretryStrategy: {type: fixed-quota, totalAllowedRetryQuota: 1, retryPerTestQuota: 1}')]:
        text='name: t\noutputDir: out\nvendorConfiguration:\n  type: iOS\n  bundle:\n    application: app.ipa\n    testApplication: tests.zip'+tail
        add(id,text,base='focused',operator='effects',vendor='iOS',setup='validation',
            files={'config/app/stale':'stale','config/app/Payload/A.app/empty':'old'},
            links={'config/contained-link':'app.apk'},
            archives=[{'path':'config/app.ipa','entries':{'Payload/A.app/empty':'new'}},
                      {'path':'config/tests.zip','entries':{'Tests.xctest/empty':''}}],
            linux=id=='effects-parse')
    # Focused interactions complement (rather than multiply) per-key mutations.
    def focused(id, value, **kw):
        add('focused-'+id,emit(transformed(value)),base='focused',operator='interaction',
            vendor=value['vendorConfiguration']['type'],setup='validation',**kw)
    for id, backend in [('android-sets','m-android-full'),('ios-sets','m-ios-full'),('macos-sets','m-macos-full')]:
        value=copy.deepcopy(byid[backend]['value']); vendor=value['vendorConfiguration']
        if id=='android-sets':
            vendor['fileSyncConfiguration']={'pull':[{'relativePath':'b','pathRoot':'APP_DATA','aggregationMode':'DEVICE'},
                                                      {'relativePath':'a','pathRoot':'LOCAL_TMP','aggregationMode':'TEST_RUN'}],
                                              'push':[{'path':'b','pathRoot':'LOCAL_TMP'},{'path':'a','pathRoot':'APP_DATA'}]}
            vendor['instrumentationArgs']={'😀':'supplementary','ｚ':'bmp','literal':'/System/Volumes/Data/user@deadbeef'}
        elif id=='ios-sets':
            vendor['permissions']['grant']=['contacts','microphone']
            vendor['lifecycle']['onPrepare']=['ERASE','SHUTDOWN']; vendor['lifecycle']['onDispose']=['SHUTDOWN','TERMINATE']
        else:
            vendor['permissions']['grant']={'com.example':['developer-tool','accessibility']}
        focused(id,value)
    for id,timeout in [('derived-timeouts',{'shell':'PT10M','boot':'PT20M'}),
                       ('explicit-timeouts',{'shell':'PT10M','boot':'PT20M','erase':'PT1S','bootWait':'PT2S','xcresulttool':'PT3S'})]:
        value=copy.deepcopy(byid['b-ios-tvos-ok']['value']);value['vendorConfiguration']['timeoutConfiguration']=timeout
        focused(id,value)
    for id,sorting in [('relative-time',{'type':'execution-time','percentile':'0.8','timeLimit':'-PT1H'}),
                       ('recursive-time',{'type':'execution-time','nested':{'percentile':80,'timeLimit':'PT1H'}}),
                       ('bad-execution-time',{'type':'execution-time','percentile':80,'timeLimit':'bad'}),
                       ('absolute-success-rate',{'type':'success-rate','timeLimit':'1970-01-01T00:00:00Z'}),
                       ('relative-success-rate',{'type':'success-rate','timeLimit':'PT1H'})]:
        value=copy.deepcopy(byid['a-stub']['value']);value['sortingStrategy']=sorting
        focused(id,value)
    for id,batch in [('bad-fixed-time',{'type':'fixed-size','size':'2','durationMillis':{'x':1},'timeLimit':'bad'}),
                     ('recursive-fixed-time',{'type':'fixed-size','nested':{'size':'2','percentile':[], 'timeLimit':'PT1H'}})]:
        value=copy.deepcopy(byid['a-stub']['value']);value['batchingStrategy']=batch
        focused(id,value)
    value=copy.deepcopy(byid['m-ios-full']['value']);value['vendorConfiguration']['deviceProvider']={'type':'marathondevices','path':'devices'}
    focused('provider-alias',value)
    # Absolute spellings stay verbatim, relative links canonicalise against the
    # Marathonfile directory even though cwd is elsewhere.
    add('focused-path-symlink',base.replace('outputDir: out','outputDir: link/../out'),
        base='focused',operator='path',vendor='stub',setup='validation',links={'config/link':'derivedDataDir'})
    add('focused-path-absolute',base.replace('outputDir: out','outputDir: @ROOT@/config/./out'),
        base='focused',operator='path',vendor='stub',setup='validation')
    for id,appentries,testentries in [
        ('archive-root-metadata',{'Payload/A.app/empty':'new','top':'root','.DS_Store':'skip','__MACOSX':'skip','__MACOSX/keep':'yes'}, {'T.xctest/empty':''}),
        ('archive-two-apps',{'Payload/A.app/empty':'a','Payload/B.app/empty':'b'}, {'T.xctest/empty':''}),
        ('archive-partial-failure',{'Payload/A.app/empty':'new'}, {'not-a-test':'broken'}),
    ]:
        add('focused-'+id,'name: t\noutputDir: out\nvendorConfiguration:\n  type: iOS\n  bundle: {application: app.ipa, testApplication: tests.zip}',
            base='focused',operator='effects',vendor='iOS',setup='validation',
            files={'config/app/stale':'stale'},archives=[{'path':'config/app.ipa','entries':appentries},{'path':'config/tests.zip','entries':testentries}])
    add('focused-archive-same-stem','name: t\noutputDir: out\nvendorConfiguration:\n  type: iOS\n  bundle: {application: app.ipa, testApplication: app.zip}',
        base='focused',operator='effects',vendor='iOS',setup='validation',
        archives=[{'path':'config/app.ipa','entries':{'Payload/A.app/empty':'app'}},{'path':'config/app.zip','entries':{'T.xctest/empty':'test'}}])
    add('focused-effects-link','name: t\noutputDir: out\nvendorConfiguration:\n  type: iOS\n  bundle: {application: app.ipa, testApplication: missing.zip}',
        base='focused',operator='effects',vendor='iOS',setup='validation',
        links={'config/app/stale-link':'missing'},
        files={'config/app/stale':'stale'},archives=[{'path':'config/app.ipa','entries':{'Payload/A.app/empty':'new'}}])
    data=''.join(json.dumps(r,ensure_ascii=False,separators=(',',':'))+'\n' for r in cases)
    (OUT/'cases.jsonl.gz').write_bytes(gzip.compress(data.encode(),mtime=0))
    (OUT/'cases.jsonl').unlink(missing_ok=True)
    binder_names=set()
    for name in ['bind.rs','bind_vendor.rs']:
        source=(ROOT/'crates/marathon-config/src'/name).read_text()
        binder_names.update(re.findall(r'"(\w+)"\s*=>\s*\{',source))
        binder_names.update(re.findall(r'\.(?:get|find_value)\("(\w+)"\)',source))
    covered_names={row['field'].split('.')[-1] for row in coverage}
    missing=binder_names-covered_names
    if missing: raise ValueError('uncovered binding keys: '+str(sorted(missing)))
    manifest={'schema':1,'bindingKeyNames':sorted(binder_names),'cases':len(cases),'inputsSha256':hashlib.sha256(data.encode()).hexdigest(),
              'operators':OPS,'coverage':coverage,'productionInputs':0,
              'baselineGoldens':30,'validationRows':87,'linuxReplacements':4,'carriedLinuxReplacements':3}
    (OUT/'manifest.json').write_text(json.dumps(manifest,ensure_ascii=False,indent=2)+'\n')
    print(f'{len(cases)} cases, {len(coverage)} key/variant rows')

if __name__=='__main__': main()
