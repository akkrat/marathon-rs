#!/usr/bin/env python3
"""Capture a new immutable release generation using the existing whole-vendor adapters/contracts."""
from pathlib import Path
import json, os, runpy, shutil, subprocess, sys
HERE=Path(__file__).resolve().parent
sys.path.insert(0,str(HERE))
import evidence as e
ROOT=HERE.parents[1]
SHA=(ROOT/'docs/release-source.sha').read_text().strip()
os.environ['ORACLE_SOURCE_SHA']=SHA
d=runpy.run_path(str(HERE/'run'))
os.umask(0o022)
work=HERE/'runs/release-c95b2128'
work.mkdir(parents=True,exist_ok=True)
binaries=d['build_vendor_binaries'](work/'release-binaries')
verification=ROOT/'fixtures/differential/vendor-verification-c95b2128'
verification.mkdir(parents=True,exist_ok=True)
manifest=dict(format='marathon-differential/verification@1',source=SHA,native_sha=subprocess.check_output(['git','rev-parse','HEAD'],cwd=ROOT,text=True).strip(),records={})
variants={}
for name,row in d['adapters']().items():
    if row['kind']!='vendor': continue
    recordings=[]
    for implementation,label in [('jvm','jvm-a'),('jvm','jvm-b'),('native','native')]:
        target=work/name/label
        result=target/'recording'
        if not result.exists():
            target.mkdir(parents=True,exist_ok=False)
            result=d['capture'](name,implementation,target.resolve(),binaries)
        facts=e.validate(result,d['contract'](name))
        e.require(facts['source']==SHA and facts['provenance']['native_sha']==manifest['native_sha'],'stale resumed capture')
        if implementation=='jvm' and recordings:
            a,b=e.normalized(recordings[0],d['contract'](name)),e.normalized(result,d['contract'](name))
            changed={key for key in set(a)|set(b) if a.get(key)!=b.get(key)}
            allowed={path for path in d['contract'](name)['paths'] if path.startswith('reports/logs/')}
            e.require(changed<=allowed,'JVM repeat differs outside the approved A20 log partitions: '+repr(changed))
            if changed:
                variants[name]=dict(reason='A20: finite complete JVM log-partition witnesses',paths=sorted(changed),
                    alternates=['fixtures/differential/vendors/'+name+'/jvm-c95b2128-b'])
        if implementation=='native': e.compare_variants(recordings,result,d['contract'](name),target/'mismatch')
        else: recordings.append(result)
        destination=verification/name/label;destination.parent.mkdir(parents=True,exist_ok=True)
        if destination.exists(): e.require((destination/'evidence.json').read_bytes()==(result/'evidence.json').read_bytes(),'changed resumed verification')
        else: shutil.copytree(result,destination)
        manifest['records'][name+'/'+label]=e.sha((destination/'evidence.json').read_bytes())
    for index,recording in enumerate(recordings if name in variants else recordings[:1]):
        golden=ROOT/'fixtures/differential/vendors'/name/('jvm-c95b2128' if index==0 else 'jvm-c95b2128-b')
        if not golden.exists(): shutil.copytree(recording,golden)
    print(name+': JVM repeat and native pass'+(' (finite A20 variants)' if name in variants else ''),flush=True)
(verification/'manifest.json').write_bytes(e.encoded(manifest))
(HERE/'release-variants.json').write_bytes(e.encoded(variants))
print('15 complete release vendor pairs / 45 records passed',flush=True)
