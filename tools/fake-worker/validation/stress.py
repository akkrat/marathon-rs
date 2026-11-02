#!/usr/bin/env python3
"""Repeat the full contract and deterministic cancellation suites; retain every outcome.

Usage: python3 tools/fake-worker/validation/stress.py ITERATIONS FRESH_OUTPUT_DIR
"""
import json, pathlib, subprocess, sys, time
count=int(sys.argv[1]);out=pathlib.Path(sys.argv[2]);out.mkdir(exist_ok=False,parents=True)
build=subprocess.run(['cargo','test','-p','fake-worker','--lib','--test','contracts','--no-run','--message-format=json'],stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
(out/'build.stderr').write_text(build.stderr)
build.check_returncode()
bins=[r['executable'] for line in build.stdout.splitlines() if (r:=json.loads(line)).get('reason')=='compiler-artifact' and r.get('executable') and r['profile']['test']]
assert len(bins)==2,bins
results=[];start=time.monotonic()
for i in range(1,count+1):
 for executable in bins:
  log=out/f'{i:03d}-{pathlib.Path(executable).name}.log'
  with log.open('wb') as f:
   try:
    r=subprocess.run([executable],stdout=f,stderr=subprocess.STDOUT,timeout=30)
    status=r.returncode
   except subprocess.TimeoutExpired:status='timeout'
  results.append({'iteration':i,'binary':executable,'exit':status})
  (out/'results.json').write_text(json.dumps({'requested_iterations':count,'results':results,'seconds':round(time.monotonic()-start,2)},indent=2)+'\n')
  if status!=0:raise SystemExit(f'FAILED {i} {executable}: {status}; see {log}')
 if i%25==0: print(f'{i}/{count} complete; {time.monotonic()-start:.1f}s',flush=True)
print(f'PASS: {count} full contract + deterministic-regression iterations',flush=True)
