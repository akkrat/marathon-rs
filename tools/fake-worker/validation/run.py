#!/usr/bin/env python3
"""Real clients/rsync matrix. Every subprocess has a deadline and saved outcome.
No retries: in particular each sshpass failure is retained (U4).
"""
import argparse
import base64
import copy
import hashlib
import json
import os
import pathlib
import platform
import select
import shutil
import signal
import subprocess
import sys
import time
import containment
from suite_plan import inventory

REPO = pathlib.Path(__file__).resolve().parents[3]
HERE = REPO / 'tools/fake-worker'
p = argparse.ArgumentParser()
p.add_argument('--output', required=True, type=pathlib.Path)
p.add_argument('--java', default=os.environ.get('JAVA_HOME', '') + '/bin/java')
p.add_argument('--worker',default=os.environ.get('FW_WORKER_BIN',str(REPO/'target/debug/fake-worker')))
p.add_argument('--rsync', default='/usr/bin/rsync')
p.add_argument('--sshpass', default=shutil.which('sshpass'))
p.add_argument('--skip-long', action='store_true', help='development only; incomplete acceptance')
p.add_argument('--only', choices=['standard', 'faults'])
a = p.parse_args()
a.output = a.output.resolve()
a.output.mkdir(parents=True, exist_ok=False)
results = []
suite = a.only or 'all'
plan = inventory(suite, sys.platform)
terminal_outcome = 'running'
env = os.environ.copy()
env.update(PATH=str(HERE/'shims') + ':' + env['PATH'], FW_REAL_RSYNC=a.rsync, FW_REAL_SSH='/usr/bin/ssh', FW_ARGV_LOG=str(a.output/'argv.jsonl'))
if a.sshpass:
    env['FW_REAL_SSHPASS'] = a.sshpass
for tool in ['rsync', 'sshpass']:
    env['RSYNC4J_' + tool.upper()] = str(HERE/'shims'/tool)

def save():
    (a.output/'results.json').write_text(json.dumps({'schema':2,'suite':suite,'platform_id':sys.platform,'platform':platform.platform(),'terminal_outcome':terminal_outcome,**plan,'results':results},indent=2)+'\n')

def kill_tree(child):
    # sshpass creates a new session for ssh, so killing only our process group
    # is insufficient. Include descendants before killing the parent.
    rows=subprocess.check_output(['/bin/ps','-axo','pid=,ppid=']).decode().splitlines()
    pairs=[tuple(map(int,row.split())) for row in rows if row.strip()]
    descendants={child.pid}
    while True:
        expanded=descendants|{pid for pid,ppid in pairs if ppid in descendants}
        if expanded==descendants:break
        descendants=expanded
    for pid in sorted(descendants,reverse=True):
        try:os.kill(pid,signal.SIGKILL)
        except ProcessLookupError:pass
    child.wait()


def run(name, cmd, expected=0, timeout=30, overrides=None, data=None, cwd=None):
    settings = dict(env)
    settings.update(overrides or {})
    start = time.monotonic()
    with (a.output/(name+'.stdout')).open('wb') as out, (a.output/(name+'.stderr')).open('wb') as err:
        child = subprocess.Popen([str(s) for s in cmd], env=settings, cwd=cwd, stdin=subprocess.PIPE if data is not None else subprocess.DEVNULL, stdout=out, stderr=err, start_new_session=True)
        try:
            child.communicate(data,timeout=timeout)
            outcome = 'completed' if child.returncode==expected else 'failed'
        except subprocess.TimeoutExpired:
            kill_tree(child)
            child.communicate()
            outcome = 'timeout'
    row = {'case':name,'outcome':outcome,'exit':child.returncode,'expected':expected,'seconds':round(time.monotonic()-start,3)}
    results.append(row);save();print(json.dumps(row),flush=True)
    return row

def check(name, condition):
    row={'case':name,'outcome':'completed' if condition else 'failed'}
    results.append(row);save();print(json.dumps(row),flush=True)
    if not condition:
        raise AssertionError(name)

save()
try:
    if not a.sshpass:
        raise RuntimeError('missing sshpass prerequisite')
    known_hosts = [pathlib.Path.home()/'.ssh/known_hosts',pathlib.Path.home()/'.ssh/known_hosts2']
    def digest(p):
        return hashlib.sha256(p.read_bytes()).hexdigest() if p.exists() else None
    before = {str(f):digest(f) for f in known_hosts}
    key = a.output/'fixture-key'
    hostkey = a.output/'host-key'
    for target in [key,hostkey,a.output/'wrong-key']:
        subprocess.run(['/usr/bin/ssh-keygen','-q','-t','ed25519','-N','','-C','disposable-fake-worker-fixture','-f',str(target)],check=True)
    env['FW_KEY']=str(key)
    versions={}
    for name,cmd in [('rsync',[a.rsync,'--version']),('ssh',['/usr/bin/ssh','-V']),('sshpass',[a.sshpass,'-V'] if a.sshpass else None),('java',[a.java,'-version'])]:
        if not cmd:
            versions[name]={'missing':True};continue
        r=subprocess.run(cmd,stdout=subprocess.PIPE,stderr=subprocess.STDOUT)
        r.check_returncode()
        versions[name]={'output':r.stdout.decode(errors='replace'),'binary_sha256':digest(pathlib.Path(cmd[0]))}
    (a.output/'versions.json').write_text(json.dumps(versions,indent=2)+'\n')
    script=json.loads((REPO/'fixtures/fake-worker/scenario.json').read_text())
    script['rules'].append({'name':'busy','matcher':{'exact':'busy'},'action':{'type':'output','chunks':[{'data':'tick\n','delay_ms':100} for _ in range(120)]}})

    class Worker:
        def __init__(self,label,faults=None,auth=None):
            self.label=label
            self.dir=a.output/label
            self.dir.mkdir()
            s=copy.deepcopy(script);s['faults']=faults or {}
            config={'listen':'127.0.0.1:0','root':str(self.dir/'root'),'transcript':str(self.dir/'transcript.jsonl'),'host_key':str(hostkey),'auth':{'user':'fixture','password':'fake-password','authorized_keys':[str(key)+'.pub'],**(auth or {})},'max_sessions':4,'rsync_binary':a.rsync,'script':s,'fixtures':str(REPO/'fixtures/fake-worker'),'run':label}
            cfg=self.dir/'config.json';cfg.write_text(json.dumps(config));cfg.chmod(0o600)
            self.err=(self.dir/'server.stderr').open('wb')
            try:
                self.proc=subprocess.Popen([a.worker,'--config',str(cfg)],stdout=subprocess.PIPE,stderr=self.err,start_new_session=True)
                ready=select.select([self.proc.stdout],[],[],15)[0]
                if not ready:
                    self.proc.kill();self.proc.wait();raise RuntimeError('missing readiness')
                line=self.proc.stdout.readline()
                if not line:
                    self.proc.wait(timeout=5);self.err.close()
                    raise RuntimeError('server startup failed: '+str(self.dir))
                (self.dir/'readiness.json').write_bytes(line)
                self.port=json.loads(line)['endpoint'].rsplit(':',1)[1]
            except BaseException:
                if hasattr(self, 'proc') and self.proc.poll() is None:kill_tree(self.proc)
                self.err.close()
                raise

        def ssh(self,*options,identity=None):
            return [HERE/'shims/ssh','-F','/dev/null','-o','StrictHostKeyChecking=no','-o','LogLevel=ERROR','-o','ConnectTimeout=3','-i',identity or key,'-l','fixture','-p',self.port,*options,'127.0.0.1']
        def kotlin(self,name,case,timeout=60,overrides=None):
            cp=str(HERE/'build/classes')+':'+str(HERE/'build/lib/*')
            return run(name,[a.java,'-Dlogback.configurationFile='+str(REPO/'spikes/ssh-sshj/logback-spike.xml'),'-cp',cp,'BoundaryKt',case],timeout=timeout,overrides={'FW_PORT':self.port,'FW_WORK':str(self.dir),**(overrides or {})})
        def close(self):
            self.proc.terminate()
            try:self.proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                kill_tree(self.proc);results.append({'case':self.label+'-shutdown','outcome':'timeout'});save()
            self.err.close()
            check(self.label+'-server-exit',self.proc.returncode==0)
        def __enter__(self):return self
        def __exit__(self,*args):self.close()

    def manifest(root):
        result={}
        for p in root.rglob('*'):
            if p.is_symlink(): result[str(p.relative_to(root))]={'link':os.readlink(p)}
            elif p.is_file():result[str(p.relative_to(root))]={'sha256':digest(p),'mode':p.stat().st_mode&0o777}
        return result

    def argv_assertions(w):
        rows=[json.loads(line) for line in (a.output/'argv.jsonl').read_text().splitlines()]
        transfers=[r['argv'] for r in rows if r['program']=='rsync' and '--rsh' in r['argv']]
        check('rsync-call-count',len(transfers)==6)
        archive=['--recursive','--links','--perms','--owner','--group','--devices','--specials','--times']
        suffix=['--rsync-path=/usr/bin/rsync','--delete','--partial','--partial-dir=.rsync-partial','--delay-updates']
        for i,args in enumerate(transfers):
            debug=i==1
            flags=(['--verbose'] if debug else [])+archive
            rsh="ssh -o 'StrictHostKeyChecking no' -F /dev/null "
            if not debug:rsh+='-o ServerAliveInterval=1 -o ServerAliveCountMax=4 '
            if i<4:rsh+='-i '+str(key)+' '
            rsh+='-l fixture -p '+w.port+' '+('-vvv' if debug else '')
            check('exact-rsync-argv-%d'%i,args[:-2]==flags+['--rsh',rsh]+suffix)
        check('debug-version-argv',any(r['program']=='rsync' and r['argv']==['--version','/tmp','/tmp'] for r in rows))
        check('receive-directory-slashes',all(transfers[i][-2].endswith('/') and transfers[i][-1].endswith('/') for i in [3,5]))
        ssh_rows=[r for r in rows if r['program']=='ssh']
        check('shim-known-host-isolation',all('UserKnownHostsFile=/dev/null' in r['harness_options'] and 'GlobalKnownHostsFile=/dev/null' in r['harness_options'] and 'UpdateHostKeys=no' in r['harness_options'] for r in ssh_rows))
        check('all-three-shims',set(r['program'] for r in rows)=={'ssh','sshpass','rsync'})
        events=[json.loads(line) for line in (w.dir/'transcript.jsonl').read_text().splitlines()]
        execs=[base64.b64decode(r['detail']['base64']) for r in events if r['event']=='exec']
        check('writefile-client-guards',not any(b'oversize-guard' in command or b'with space' in command for command in execs))
        check('writefile-command-count',sum(b'umask 077 && echo' in command for command in execs)==4)
        servers=[r['detail']['argv'] for r in events if r['event']=='child_start']
        check('server-directions',any('--sender' in row for row in servers) and any('--sender' not in row for row in servers))
        mux=[r for r in events if r['event']=='exec' and base64.b64decode(r['detail']['base64']).startswith(b'kill -s TERM')]
        check('openssh-term-on-reused-connection',len(mux)==1 and sum(r['event']=='exec' and r['connection']==mux[0]['connection'] for r in events)==4)
        check('virtual-term-delivered',any(r['event']=='background_signal' and r['detail']['signal']=='TERM' for r in events))
        if sys.platform=='darwin':
            check('openrsync-generated-server-argv',all('--delete-before' in row and '--dirs' in row for row in servers))


    try:
        if a.only!='faults':
            with Worker('standard') as w:
                src=w.dir/'source';(src/'nested').mkdir(parents=True)
                (src/'nested/binary.bin').write_bytes(bytes(range(256))*1500)
                (src/'nested/binary.bin').chmod(0o640)
                (src/'text').write_text('fixture\n')
                os.symlink('nested/binary.bin',src/'link')
                os.symlink('/tmp/marathon/shared/runner.app',src/'virtual-absolute-link')
                os.symlink('../../outside-sentinel',src/'unsafe-link')
                sentinel=w.dir/'outside-sentinel';sentinel.write_bytes(b'outside the backing root\n');sentinel_hash=digest(sentinel)
                (w.dir/'root/tree').mkdir();(w.dir/'root/tree/stale').write_text('deleted by rsync')
                row=run('openssh-key',w.ssh()+['probe'],expected=7)
                check('openssh-streams',(a.output/'openssh-key.stdout').read_bytes()==b'hello\n' and (a.output/'openssh-key.stderr').read_bytes()==b'error\n')
                data=bytes(range(256))*1200
                run('openssh-stdin',w.ssh()+['cat'],data=data)
                check('openssh-binary-stdin',(a.output/'openssh-stdin.stdout').read_bytes()==data)
                control=w.dir/'control.sock'
                master_log=(w.dir/'master.stderr').open('wb')
                master=subprocess.Popen([str(x) for x in w.ssh('-M','-N','-S','control.sock')],cwd=w.dir,env=env,stdout=subprocess.DEVNULL,stderr=master_log,start_new_session=True)
                try:
                    deadline=time.monotonic()+5
                    while not control.exists() and master.poll() is None and time.monotonic()<deadline:time.sleep(.02)
                    check('openssh-master-ready',control.exists())
                    run('openssh-reuse-1',w.ssh('-S','control.sock')+['probe'],expected=7,cwd=w.dir)
                    run('openssh-background',w.ssh('-S','control.sock')+["sh -c 'nohup /usr/bin/fixture-background > /dev/null 2>&1 & echo $! > /tmp/marathon/fixture.pid'"],cwd=w.dir)
                    pid=(w.dir/'root/fixture.pid').read_text().strip()
                    run('openssh-term',w.ssh('-S','control.sock')+['kill -s TERM '+pid],cwd=w.dir)
                    run('openssh-reuse-2',w.ssh('-S','control.sock')+['probe'],expected=7,cwd=w.dir)
                    run('openssh-master-exit',w.ssh('-S','control.sock','-O','exit'),cwd=w.dir)
                    master.wait(timeout=5)
                finally:
                    if master.poll() is None:os.killpg(master.pid,signal.SIGKILL);master.wait()
                    master_log.close()
                run('openssh-reject-key' ,w.ssh('-o','BatchMode=yes',identity=a.output/'wrong-key')+['probe'],expected=255)
                w.kotlin('kotlin-standard-askpass','standard',timeout=90,overrides={'FW_USE_FIXTURE_ASKPASS':'1'})
                expected=manifest(src)
                check('key-roundtrip',manifest(w.dir/'back-key')==expected)
                check('password-roundtrip',manifest(w.dir/'back-password')==expected)
                check('deletion',not (w.dir/'root/tree/stale').exists())
                check('outside-sentinel-unchanged',digest(sentinel)==sentinel_hash)
                check('writefile-bytes',(w.dir/'root/shared/marathon.xctestrun').read_bytes()==(w.dir/'expected.xctestrun').read_bytes())
                check('writefile-mode', (w.dir/'root/shared/marathon.xctestrun').stat().st_mode&0o777==0o600)
                check('fallback-bytes',(w.dir/'root/large.bin').read_bytes()==(w.dir/'large.bin').read_bytes() and (w.dir/'root/debug.bin').read_bytes()==(w.dir/'small.bin').read_bytes())
                w.kotlin('kotlin-auth-reject','auth_reject')
                if a.sshpass:
                    for i in range(20):
                        run('sshpass-attempt-%02d'%i,[HERE/'shims/sshpass','-p','fake-password',*w.ssh('-o','PreferredAuthentications=password','-o','PubkeyAuthentication=no'),'probe'],expected=7,timeout=12)
                    run('sshpass-reject',[HERE/'shims/sshpass','-p','wrong-fixture-password',*w.ssh('-o','PreferredAuthentications=password','-o','PubkeyAuthentication=no'),'probe'],expected=255,timeout=12)
                    for i in range(10):
                        run('askpass-attempt-%02d'%i,[HERE/'shims/sshpass','-p','fake-password',*w.ssh('-o','PreferredAuthentications=password','-o','PubkeyAuthentication=no'),'probe'],expected=7,timeout=12,overrides={'FW_USE_FIXTURE_ASKPASS':'1'})
                    run('askpass-reject',[HERE/'shims/sshpass','-p','wrong-fixture-password',*w.ssh('-o','PreferredAuthentications=password','-o','PubkeyAuthentication=no'),'probe'],expected=255,timeout=12,overrides={'FW_USE_FIXTURE_ASKPASS':'1','FW_ASKPASS_PASSWORD':'wrong-fixture-password'})
                else:check('sshpass-prerequisite',False)
                argv_assertions(w)
                w.kotlin('kotlin-cleanup','cleanup')
                check('virtual-root-clean',list((w.dir/'root').iterdir())==[])
            with Worker('keyboard',auth={'password_auth':False}) as w:
                w.kotlin('kotlin-keyboard','keyboard')
                run('openssh-keyboard',[HERE/'shims/sshpass','-p','fake-password',*w.ssh('-o','PreferredAuthentications=keyboard-interactive','-o','PubkeyAuthentication=no'),'probe'],expected=7,timeout=12)
            with Worker('containment') as w:
                containment.exercise(w, run, check, a.rsync)
        if a.only!='standard':
            with Worker('reject',{'reject_exec':True}) as w:w.kotlin('exec-rejection','reject')
            for stage in ['before_kex','after_auth','during_exec']:
                for reason in [12,2,11]:
                    with Worker(stage+'-'+str(reason),{stage:{'reason':reason,'message':'fixture disconnect '+str(reason)}}) as w:
                        w.kotlin(stage+'-'+str(reason),'exec_fault' if stage=='during_exec' else 'post_auth' if stage=='after_auth' else 'connect_fault')
                        observed=[json.loads(line) for line in (a.output/(stage+'-'+str(reason)+'.stdout')).read_text().splitlines() if line.startswith('{')]
                        chain=next(row['value'] for row in observed if row['case'] in ['exec_fault','post_auth','connect_fault'])
                        if stage=='after_auth':check(stage+'-classification-'+str(reason),any(e['class']=='IllegalStateException' and e['message']=='Not connected' for e in chain))
                        else:check(stage+'-wire-reason-'+str(reason),any(e.get('reason')=={12:'TOO_MANY_CONNECTIONS',2:'PROTOCOL_ERROR',11:'BY_APPLICATION'}[reason] for e in chain))
            with Worker('kex',{'no_common_kex':True}) as w:w.kotlin('kex-failure','connect_fault')
            with Worker('keepalive-delay',{'keepalive':{'delay':250}}) as w:
                w.kotlin('keepalive-delayed-healthy','healthy_keepalive',timeout=20)
            with Worker('keepalive',{'keepalive':'withhold'}) as w:
                w.kotlin('keepalive-with-output','keepalive',timeout=15)
            if not a.skip_long:
                with Worker('close',{'withhold_close':True}) as w:w.kotlin('t15','t15',timeout=45)
                with Worker('timeout-close',{'withhold_close':True}) as w:w.kotlin('t15h','t15h',timeout=100)
            else:results.append({'case':'long-close-cases','outcome':'skipped'});save()
    finally:
        check('known-hosts-unchanged',before=={str(f):digest(f) for f in known_hosts})
        # Keys/config contain disposable fixture credentials; never copy them into
        # committed evidence. Raw transcripts do not contain authentication secrets.
        save()

except BaseException as error:
    results.append({'case':'runner-exception','outcome':'failed','error':type(error).__name__+': '+str(error)})
finally:
    cases=[row['case'] for row in results]
    missing=sorted(set(plan['expected_cases'])-set(cases))
    artifacts=[name for name in plan['required_artifacts'] if not (a.output/name).is_file()]
    if missing or artifacts or len(cases)!=len(set(cases)):
        results.append({'case':'run-inventory','outcome':'failed','missing_cases':missing,'missing_artifacts':artifacts})
    terminal_outcome='completed' if all(row['outcome']=='completed' for row in results) else 'failed'
    save()
sys.exit(0 if terminal_outcome=='completed' else 1)
