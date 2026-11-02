#!/usr/bin/env python3
"""Lossless test transport for existing JVM outputs; model inputs mirror JcompatTables.kt.

Never invent expectations: expected UTF-16 units are transcribed from committed
JVM tables. The transport avoids serde_json rejecting lone surrogates.
"""
import json, pathlib
ROOT=pathlib.Path(__file__).resolve().parents[3]
BASE=ROOT/'fixtures/oracle'
def units(s):
    b=s.encode('utf-16-le','surrogatepass');return [b[i]+256*b[i+1] for i in range(0,len(b),2)]
def node(v):
    if v is None:return {'kind':'null'}
    if isinstance(v,bool):return {'kind':'bool','value':v}
    if isinstance(v,str):return {'kind':'string','units':units(v)}
    if isinstance(v,(int,float)):return {'kind':'number','value':str(v) if isinstance(v,int) else str(v).replace('e+','E').replace('e','E')}
    if isinstance(v,list):return {'kind':'array','items':[node(x) for x in v]}
    return {'kind':'object','entries':[[units(k),node(x)] for k,x in v.items()]}
def number(s):return {'kind':'number','value':s}
def rows(name):return [json.loads(x) for x in (BASE/name).read_text().split('\n')[1:] if x]
def device(serial='emulator-5554',read=False,empty=False):
    return dict(operatingSystem={'version':'' if empty else '17.4'},serialNumber=serial,model='m' if empty else 'iPhone 15',manufacturer='x' if empty else 'Apple',networkState='DISCONNECTED' if empty else 'CONNECTED',deviceFeatures=[] if empty else ['VIDEO','SCREENSHOT'],healthy=not empty,**{'safeSerialNumber$delegate':{'_value':'127.0.0.1-5555'} if read else {'initializer':{},'_value':{}}})
def test():return dict(pkg='com.example',clazz='FooTest',method='testBar',metaProperties=[{'name':'org.junit.Test','values':{}},{'name':'Owner','values':{'value':'team <a&b>'}}])
def result(fail=False):
    return dict(test=test(),device=device(),testBatchId='batch-2' if fail else 'batch-1',status='FAILURE' if fail else 'PASSED',startTime=0 if fail else 1700000000000,endTime=0 if fail else 1700000001500,stacktrace="java.lang.AssertionError: expected:<1> but was:<2>\n\tat Foo.bar(Foo.kt:12)\r\n'quoted' = \"double\" \u2028 \u0001 é ✓" if fail else None,attachments=[{'file':'/tmp/out/screen.png','type':'SCREENSHOT_PNG','name':'screen'},{'file':'<CWD>/relative/log.txt','type':'LOG','name':None}] if fail else [],isTimeInfoAvailable=not fail)
models={
'device-lazy-untouched':device(),'device-lazy-read':device('127.0.0.1:5555',True),'device-no-features':device('s',empty=True),
'bill':dict(device=device('A1B2-C3'),pool={'name':'omni'},start=1700000000000,end=1700000123456,duration=123456),
 'test-result-passed':result(),'test-result-failure':result(True),
 'raw-test-runs':[dict(package='com.example',**{'class':'FooTest'},method='testBar',metaProperties=[{'name':'Owner','values':{'value':'me'}}],deviceSerial='emulator-5554',status='PASSED',ignored=False,success=True,timestamp=1700000000000,duration=1500),dict(package='',**{'class':'Bar'},method='baz',metaProperties=[],deviceSerial='s',status='ASSUMPTION_FAILURE',ignored=True,success=False,timestamp=0,duration=0)],
 'meta-values':{'name':'m','values':dict(int=1,long=3000000000,double=1.0,float=0.5,string='s',null=None,list=[1,'a'],bool=True)},
 'meta-nan':{'name':'m','values':{'nan':float('nan')}},
 'strings':["<a href='x'>&amp;</a>",'=','\x7f','\u2028\u2029','😀','\ud800','tab\tnl\n'],
 'file':['/abs/path','<CWD>/rel/path'],'test':test(),'parse-result':{'tests':[test()],'flakyTests':None},'pool-id-map':{'DevicePoolId(name=omni)':1}}
models={k:node(v) for k,v in models.items()}
models['meta-nan']['entries'][1][1]['entries'][0][1]=number('NaN')
models['doubles']={'kind':'array','items':[number(s) for s in ['0.1','1.0E7','0.001','1.0E-4','1.23456789012E11','1.0E21','-0.0','100.0']]}
def ordered(v):
    if isinstance(v,list):return [ordered(x) for x in v]
    if not isinstance(v,dict):return v
    if 'tests' in v:keys=['tests','flakyTests']
    elif 'pkg' in v:keys=['pkg','clazz','method','metaProperties']
    elif 'name' in v and 'values' in v:keys=['name','values']
    elif set(v)=={'b','d','list','n','null','value'}:keys=['value','n','d','b','null','list']
    else:keys=list(v)
    return {k:ordered(v[k]) for k in keys if k in v}
cases=[]
for r in rows('jcompat/gson.jsonl'):
    expected=r['json'];cases.append(dict(id=r['id'],writer='gson',input=models[r['id']],expected={'ok':units(expected['ok'])} if 'ok' in expected else {'error':expected['error']['message']}))
for r in rows('jcompat/jackson_yaml_emit.jsonl'):
    cases.append(dict(id=r['id'],writer='yaml',input=node(ordered(r['input'])),expected={'ok':units(r['yaml']['ok'])}))
# The first two skipped spike rows have an explicit LinkedHashMap/data-class schema.
for r in rows('yaml-corpus.jsonl'):
    if r.get('id') in ('C60a','C60b'):
        v={k:r['input'][k] for k in ['name','count','word','colon','nothing','flag']}
        cases.append(dict(id=r['id'],writer='yaml',input=node(v),expected={'ok':units(r['emit']['ok'])}))
# C60c's bound configuration projection is recorded separately: WP 2.1 owns
# parsing, defaults, serializers and relative-path resolution.
projection=json.loads((ROOT/'tools/oracle/writer-probes/c60c-projection.json').read_text())
r=next(r for r in rows('yaml-corpus.jsonl') if r.get('id')=='C60c')
cases.append(dict(id='C60c',writer='yaml',input=node(projection),expected={'ok':units(r['emit']['ok'])}))
with (BASE/'jcompat/writer_models.jsonl').open('w') as out:
    out.write(json.dumps({'table':'writer_models','generator':'tools/oracle/writer-probes/prepare_cases.py','rows':len(cases)})+'\n')
    for case in cases:out.write(json.dumps(case,separators=(',',':'))+'\n')
