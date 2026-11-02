#!/usr/bin/env python3
"""Fresh release overlay for every changed configuration/fixture-tree observation; retain baseline evidence."""
import concurrent.futures, gzip, hashlib, importlib.util, json, os, pathlib, subprocess
HERE=pathlib.Path(__file__).resolve().parent
ROOT=HERE.parents[2]
SHA='c95b212882c7ba7b1ad91b7981916ed175f1530e'
spec=importlib.util.spec_from_file_location('suite',HERE.parent/'config-differential-lib/suite.py')
suite=importlib.util.module_from_spec(spec);spec.loader.exec_module(suite)
manifest,cases=suite.cases()
baseline=suite.packed()
selected=[]
for case in cases:
    old=baseline[case['id']]
    vendor=(json.loads(old['rawText']).get('config') or {}).get('vendorConfiguration') or {}
    if vendor.get('bundle') is not None or case.get('setup','fixture')=='fixture': selected.append(case)
run=ROOT/'tools/oracle/runs/7.5-config-release'
run.mkdir(parents=True,exist_ok=False)

def observe(case,repeat):
    dest=run/case['id']/str(repeat)
    file,env_file=suite.setup(case,dest)
    env=suite.environment(dest)|{'ORACLE_SOURCE_SHA':SHA}
    suite.command([ROOT/'tools/oracle/config-dump',file,'--env-file',env_file,'--now',suite.NOW],dest/'cwd',env,30,dest/'stdout.json',dest/'stderr')
    text=(dest/'stdout.json').read_text()
    tree=suite.normalize(suite.snapshot(dest/'config',case,dest),dest)
    return dict(id=case['id'],evidenceId=case['id'],source_sha=SHA,inputSha256=hashlib.sha256(json.dumps(case,sort_keys=True).encode()).hexdigest(),rawSha256=hashlib.sha256(text.encode()).hexdigest(),rawText=text,normalizedText=suite.normalized_text(text,dest),sandbox=str(dest),tree=tree)

def capture(case):
    first=observe(case,0);second=observe(case,1)
    difference=suite.compare(suite.document(first['normalizedText']),suite.document(second['normalizedText'])) or suite.compare(first['tree'],second['tree'])
    if difference: raise ValueError('unstable release case '+case['id']+': '+difference)
    return first
with concurrent.futures.ThreadPoolExecutor(max_workers=6) as pool:
    rows=[]
    for index,row in enumerate(pool.map(capture,selected),1):
        rows.append(row)
        if index%100==0: print(f'{index}/{len(selected)} stable release captures',flush=True)
out=ROOT/'fixtures/oracle/coverage-c95b2128/config-differential.jsonl.gz'
raw=''.join(json.dumps(row,ensure_ascii=False,separators=(',',':'))+'\n' for row in rows).encode()
out.write_bytes(gzip.compress(raw,mtime=0))
libs=ROOT/f'tools/oracle/build-{SHA}/marathon-src/oracle-build/install/oracle/lib'
metadata=dict(source_sha=SHA,cases=len(rows),repeat='normalized documents and full filesystem trees identical',baseline_cases=manifest['cases'],baseline_outcomes_sha256=manifest['evidenceSha256'],fixture_sha256=hashlib.sha256(out.read_bytes()).hexdigest(),jars={p.name:hashlib.sha256(p.read_bytes()).hexdigest() for p in sorted(libs.glob('*.jar'))})
out.with_name('config-differential-metadata.json').write_text(json.dumps(metadata,indent=2)+'\n')
print(f'{len(rows)} release overlays -> {out}')
