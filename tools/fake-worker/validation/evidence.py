#!/usr/bin/env python3
"""Curate evidence without keys/config; normalize fields without editing payloads."""
import argparse
import json
import pathlib
import shutil
from suite_plan import inventory


def completed(path):
    if not path.is_file():
        return False
    try:
        manifest=json.loads(path.read_text())
        if manifest['schema']!=2 or manifest['terminal_outcome']!='completed':return False
        plan=inventory(manifest['suite'],manifest['platform_id'])
        if any(manifest[key]!=value for key,value in plan.items()):return False
        rows=manifest['results'];cases=[row['case'] for row in rows]
        if sorted(cases)!=plan['expected_cases'] or any(row['outcome']!='completed' for row in rows):return False
        for name in plan['required_artifacts']:
            artifact=path.parent/name
            if not artifact.is_file():return False
            if name.endswith('/transcript.jsonl'):
                events=[json.loads(line) for line in artifact.read_text().splitlines()]
                if not events or events[-1].get('event')!='cleanup' or events[-1]['detail'].get('outcome')!='completed':return False
        return True
    except (OSError,ValueError,KeyError,TypeError,AttributeError):
        return False


def normalize(value, run_root, ports, pids=None, key=''):
    pids={} if pids is None else pids
    if isinstance(value,dict):
        result={}
        for k,v in value.items():
            if k=='seconds':continue
            if k=='pid' and v is not None:
                result[k]=pids.setdefault(v,'<pid-%d>'%len(pids));continue
            if k=='host_key_fingerprint':result[k]='<host-key>';continue
            if k=='size' and value.get('kind')=='directory':result[k]=0;continue
            result[k]=normalize(v,run_root,ports,pids,k)
        return result
    if isinstance(value,list):return [normalize(v,run_root,ports,pids,key) for v in value]
    if isinstance(value,str) and key not in {'base64','sha256','data','path','link'}:
        value=value.replace(str(run_root),'<run>')
        for port,label in ports.items():
            if value==port:value=label
            value=value.replace('127.0.0.1:'+port,'127.0.0.1:'+label).replace('-p '+port+' ','-p '+label+' ')
        # Only the wrapper's executable location, not arbitrary command bytes.
        if key=='SSH_ASKPASS':value='<shims>/fixture-askpass'
    return value


def curate(source,destination):
    source=source.resolve();destination.mkdir(parents=True,exist_ok=False)
    configs=sorted(source.rglob('config.json'))
    original_root=pathlib.Path(json.loads(configs[0].read_text())['root']).parents[1] if configs else source
    raw=destination/'raw';norm=destination/'normalized';raw.mkdir();norm.mkdir()
    manifest_path=source/'results.json'
    required=set(json.loads(manifest_path.read_text()).get('required_artifacts',[])) if manifest_path.exists() else set()
    candidates=[]
    for p in source.rglob('*'):
        if not p.is_file():continue
        if p.name in {'config.json','host-key','fixture-key','wrong-key'} or p.suffix=='.pub':continue
        if str(p.relative_to(source)) in required or p.name in {'results.json','versions.json','argv.jsonl','transcript.jsonl','readiness.json','server.stderr'} or (p.parent==source and p.suffix in {'.stdout','.stderr'}):candidates.append(p)
    ports={}
    for p in sorted(source.rglob('readiness.json')):
        port=json.loads(p.read_text())['endpoint'].rsplit(':',1)[1]
        ports[port]='<port-%d>'%len(ports)
    pids={}
    for p in sorted(candidates):
        relative=p.relative_to(source);r=raw/relative;n=norm/relative;r.parent.mkdir(parents=True,exist_ok=True);n.parent.mkdir(parents=True,exist_ok=True);shutil.copyfile(p,r)
        if p.suffix=='.jsonl':
            rows=[normalize(json.loads(line),original_root,ports,pids) for line in p.read_text().splitlines()]
            n.write_text(''.join(json.dumps(row,sort_keys=True,separators=(',',':'))+'\n' for row in rows))
        elif p.suffix=='.json':n.write_text(json.dumps(normalize(json.loads(p.read_text()),original_root,ports,pids),indent=2,sort_keys=True)+'\n')
        # Stream artifacts are opaque bytes, even when they happen to be UTF-8.
        else:shutil.copyfile(p,n)


def compare(left,right):
    if not all(completed(root/variant/'results.json') for root in [left,right] for variant in ['raw','normalized']):
        return False
    files={p.relative_to(left/'normalized') for p in (left/'normalized').rglob('*') if p.is_file()}
    other={p.relative_to(right/'normalized') for p in (right/'normalized').rglob('*') if p.is_file()}
    return bool(files) and files==other and all((left/'normalized'/p).read_bytes()==(right/'normalized'/p).read_bytes() for p in files)


if __name__=='__main__':
    parser=argparse.ArgumentParser();sub=parser.add_subparsers(dest='command',required=True)
    c=sub.add_parser('curate');c.add_argument('source',type=pathlib.Path);c.add_argument('destination',type=pathlib.Path)
    c=sub.add_parser('compare');c.add_argument('left',type=pathlib.Path);c.add_argument('right',type=pathlib.Path)
    args=parser.parse_args()
    if args.command=='curate':curate(args.source,args.destination)
    else:raise SystemExit(0 if compare(args.left,args.right) else 1)
