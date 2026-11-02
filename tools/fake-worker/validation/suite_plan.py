"""Acceptance inventory declared before any prerequisite or worker is started."""
import containment


def inventory(suite, platform):
    runs=[]; checks=['known-hosts-unchanged']; workers=[]
    if suite in ('standard','all'):
        workers += ['standard','keyboard','containment']
        runs += ['openssh-key','openssh-stdin','openssh-reuse-1','openssh-background','openssh-term','openssh-reuse-2','openssh-master-exit','openssh-reject-key','kotlin-standard-askpass','kotlin-auth-reject']
        runs += ['sshpass-attempt-%02d'%i for i in range(20)]+['sshpass-reject']
        runs += ['askpass-attempt-%02d'%i for i in range(10)]+['askpass-reject']
        runs += ['kotlin-cleanup','kotlin-keyboard','openssh-keyboard']+containment.RUN_CASES
        checks += ['openssh-streams','openssh-binary-stdin','openssh-master-ready','key-roundtrip','password-roundtrip','deletion','outside-sentinel-unchanged','writefile-bytes','writefile-mode','fallback-bytes','rsync-call-count']
        checks += ['exact-rsync-argv-%d'%i for i in range(6)]
        checks += ['debug-version-argv','receive-directory-slashes','shim-known-host-isolation','all-three-shims','writefile-client-guards','writefile-command-count','server-directions','openssh-term-on-reused-connection','virtual-term-delivered','virtual-root-clean']+containment.CHECK_CASES
        if platform=='darwin':checks += ['openrsync-generated-server-argv']
    if suite in ('faults','all'):
        workers += ['reject','kex','keepalive-delay','keepalive','close','timeout-close']
        runs += ['exec-rejection','kex-failure','keepalive-delayed-healthy','keepalive-with-output','t15','t15h']
        for stage in ['before_kex','after_auth','during_exec']:
            for reason in [12,2,11]:
                label=stage+'-'+str(reason)
                workers.append(label);runs.append(label)
                checks.append(stage+('-classification-' if stage=='after_auth' else '-wire-reason-')+str(reason))
    if suite not in ('standard','faults','all'):raise ValueError('unknown suite')
    checks += [worker+'-server-exit' for worker in workers]
    artifacts=['results.json','versions.json']
    if suite in ('standard','all'):artifacts.append('argv.jsonl')
    artifacts += [name+suffix for name in runs for suffix in ['.stdout','.stderr']]
    artifacts += [worker+'/'+name for worker in workers for name in ['transcript.jsonl','readiness.json','server.stderr']]
    if suite in ('standard','all'):artifacts.append('standard/master.stderr')
    return {'expected_cases':sorted(runs+checks), 'required_artifacts':sorted(artifacts)}
