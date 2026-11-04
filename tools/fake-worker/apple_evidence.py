"""Fail-closed Apple scenario evidence; opaque streams are always copied losslessly."""
import base64
from collections import Counter
import hashlib
import json
from pathlib import Path
import re
import shutil
import struct
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[2]
REQUIRED = ['transcript.jsonl', 'state.json', 'host-argv.jsonl', 'stdout', 'stderr', 'results.json',
            'report-manifest.json', 'pushed-xctestrun.json', 'media.json', 'semantic.json', 'oracle-manifest.json']
STATUS = {'passed': 'PASSED', 'failed': 'FAILURE', 'skipped': 'IGNORED'}


def save(path, data):
    path.write_text(json.dumps(data, indent=2, sort_keys=True) + '\n')


def load(path):
    return json.loads(path.read_bytes())


def sha(data):
    return hashlib.sha256(data).hexdigest()


def rows(path):
    raw = path.read_bytes()
    assert raw.endswith(b'\n'), f'truncated JSONL: {path}'
    return [json.loads(line) for line in raw.split(b'\n')[:-1]]


def final_files(work):
    state=load(work/'state.json')
    identities={}
    for session in state['sessions']:
        if session['result']:
            batch=session['result'].rsplit('.',2)[-2]
            assert re.fullmatch(r'[0-9a-f-]{36}',batch),'invalid typed batch ID'
            identities[batch]='@BATCH-'+str(len(identities))+'@'
    def location(value):
        for original,role in identities.items():
            value=value.replace(original,role)
        return value
    files=[]
    for entry in rows(work/'transcript.jsonl')[-1]['detail']['files']:
        assert entry['path']=='/tmp/marathon' or entry['path'].startswith('/tmp/marathon/'),'filesystem escaped virtual root'
        entry=dict(entry,path=location(entry['path']))
        if entry['link'] is not None:
            entry['link']=location(entry['link'])
        if entry['kind']=='directory':
            entry.pop('size')
        files.append(entry)
    return sorted(files,key=lambda f:f['path'])


def compare(actual,expected):
    assert load(actual/'semantic.json')==load(expected/'semantic.json'),'semantic replay differs'
    assert final_files(actual)==final_files(expected),'final worker filesystem differs'


def gif_metadata(data):
    assert data[:6] in (b'GIF89a',b'GIF87a') and data[-1:]==b';','invalid GIF'
    width,height=struct.unpack('<HH',data[6:10])
    p=13+(3*(2**((data[10]&7)+1)) if data[10]&128 else 0)
    frames=0
    def skip_blocks(p):
        while data[p]:
            p+=1+data[p]
        return p+1
    while data[p]!=0x3b:
        tag=data[p];p+=1
        if tag==0x21:
            p=skip_blocks(p+1)
        else:
            assert tag==0x2c,'invalid GIF block'
            flags=data[p+8];p+=9
            if flags&128:
                p+=3*(2**((flags&7)+1))
            p=skip_blocks(p+1)
            frames+=1
    gce=data.index(b'\x21\xf9\x04')
    app=data.index(b'NETSCAPE2.0')
    return dict(format='gif',width=width,height=height,frames=frames,
                delay_centiseconds=struct.unpack('<H',data[gce+4:gce+6])[0],
                disposal=(data[gce+3]>>2)&7,loop=struct.unpack('<H',data[app+13:app+15])[0])


def report_families(manifest):
    # MacosDevice sends parsed events before notifying LogListener, while the
    # consumer resets its buffer. A trailing line may belong to the last test
    # or a separate batch file (A20). Retain full raw manifests, but allow this
    # one partition difference explicitly rather than claiming byte equality.
    files=[e for e in manifest if e['kind']=='file']
    batch_logs=[e for e in files if Path(e['path']).parts[0]=='logs' and Path(e['path']).suffix!='.log']
    assert len(batch_logs)<=1,'unexpected extra batch logs'
    return dict(Counter(Path(e['path']).parts[0] for e in files if e not in batch_logs))


def command_counts(transcript):
    catalogue=load(ROOT / 'fixtures/fake-worker/apple/command-catalogue.json')['forms']
    counts=Counter()
    for event in transcript:
        if event['event']!='exec':
            continue
        command=base64.b64decode(event['detail']['base64'],validate=True).decode()
        matches=[f for f in catalogue if f['reachability']!='deferred' and glob(f['wire_pattern'],command)]
        assert matches, 'uncatalogued command: '+command[:180]
        if ' --id ' in command:
            matches=[f for f in matches if f['id']=='xcresult-id']
        if command.startswith('/usr/bin/rsync --server --sender '):
            matches=[f for f in matches if f['id']=='rsync-receive']
        assert matches, 'wrong command form: '+command[:180]
        counts[matches[0]['id']]+=1
    return dict(counts)


def glob(pattern, value):
    # Same deliberately small '*'/'?' matcher as 0.4a. Brackets are literal,
    # unlike fnmatch (important for nm's quoted character-class pipeline).
    regex=''.join('.*' if c=='*' else '.' if c=='?' else re.escape(c) for c in pattern)
    return re.fullmatch(regex,value,flags=re.DOTALL) is not None


def redact_parser_transfer(work):
    """D20: a JVM recording must not keep the third-party library that its rsync pushes to the worker."""
    import sys
    sys.path.insert(0, str(ROOT / 'tools/differential'))
    import vendor_transport
    path = work / 'transcript.jsonl'
    path.write_bytes(vendor_transport.redact_parser_transfer(path.read_bytes()))


def collect(work, run, spec):
    redact_parser_transfer(work)
    report = run / 'in/report'
    raw_results = load(report / 'test_result/raw.json')
    details = [load(p) for p in report.glob('test_result/*/*/*.json')]
    results = []
    for row in raw_results:
        detail = next(d for d in details if (d['test']['pkg'], d['test']['clazz'], d['test']['method']) == (row['package'], row['class'], row['method']))
        attachments = []
        for att in detail.get('attachments', []):
            p = Path(att['file'])
            rel = p.relative_to(report)
            assert p.resolve().is_relative_to(report.resolve()), 'attachment escaped report'
            assert p.is_file() and p.stat().st_size, f'missing attachment {rel}'
            attachments.append({'type': att['type'], 'sha256': sha(p.read_bytes()), 'path': str(rel)})
        results.append({'package': row['package'], 'class': row['class'], 'method': row['method'], 'status': row['status'],
                        'device': 'worker-1', 'failure': detail.get('stacktrace'), 'attachments': attachments})
    save(work / 'results.json', sorted(results, key=lambda r: (r['package'], r['class'], r['method'])))
    manifest = []
    for p in sorted(report.rglob('*')):
        entry = {'path': str(p.relative_to(report)), 'kind': 'dir' if p.is_dir() else 'file'}
        if p.is_file():
            entry.update(size=p.stat().st_size, sha256=sha(p.read_bytes()))
        manifest.append(entry)
    save(work / 'report-manifest.json', manifest)
    shutil.copy2(run / 'manifest.json', work / 'oracle-manifest.json')
    transcript = rows(work / 'transcript.jsonl')
    pushed = []
    for row in transcript:
        if row['event'] == 'exec':
            command = base64.b64decode(row['detail']['base64']).decode()
            match = re.fullmatch(r"sh -c 'umask 077 && echo ([A-Za-z0-9+/=]+) \| base64 --decode > (\S+\.xctestrun)\.marathon-tmp .*'", command)
            if match:
                data = base64.b64decode(match[1])
                pushed.append({'path': match[2], 'sha256': sha(data), 'base64': match[1]})
    sessions=load(work / 'state.json')['sessions']
    captured=[{'path':s['xctestrun'],'base64':s['plist_base64'],'sha256':sha(base64.b64decode(s['plist_base64']))} for s in sessions]
    if spec['platform']=='ios':
        assert pushed == captured, 'wire plist differs from consumed plist'
    save(work / 'pushed-xctestrun.json', captured)
    media = []
    media_dir = work / 'media'
    for p in sorted(report.rglob('*')):
        if p.is_file() and (p.suffix in ('.gif', '.mp4') or 'device-logs' in p.relative_to(report).parts):
            media_dir.mkdir(exist_ok=True)
            data = p.read_bytes()
            entry = {'path': str(p.relative_to(report)), 'size': len(data), 'sha256': sha(data)}
            if p.suffix == '.gif':
                entry.update(gif_metadata(data))
            elif p.suffix == '.mp4':
                assert data == (ROOT / 'fixtures/fake-worker/apple/media/transcoded.mp4').read_bytes(), 'wrong transcoded video'
                entry.update(format='mp4', codec='h264', width=568, height=852)
            else:
                assert data == b'2000-01-01 00:00:00 AppleWorld scripted device log\n', 'wrong device log'
                entry.update(format='log')
            (media_dir / str(len(media))).write_bytes(data)
            media.append(entry)
    save(work / 'media.json', media)
    # Content hashes of all reports remain in the raw manifest. Compare semantic
    # report content and static file counts; timestamps/UUIDs in JVM reports are
    # the later WP 7.1 byte/field comparison, not silently rewritten here.
    junit = [ET.parse(p).getroot() for p in report.glob('tests/*/*.xml')]
    totals = Counter()
    for suite in junit:
        for k in ['tests', 'failures', 'skipped', 'errors']:
            totals[k] += int(suite.attrib.get(k, 0))
    stable_results = [{k: v for k, v in row.items() if k != 'attachments'} | {'attachment_types': sorted(a['type'] for a in row['attachments'])} for row in results]
    families = report_families(manifest)
    state = load(work / 'state.json')
    stable_content=sorted([{'name':Path(e['path']).name,'sha256':e['sha256'],'size':e['size']} for e in manifest if e['kind']=='file' and ('.xcresult/' in e['path'] or Path(e['path']).suffix in ('.js','.css'))],key=lambda e:(e['name'],e['sha256']))
    semantic = {'artifact_content':stable_content,'command_counts':command_counts(transcript),'tests': sorted(stable_results, key=lambda r: (r['package'], r['class'], r['method'])), 'junit': dict(totals),
                'report_families': dict(families), 'media': sorted([{k: v for k, v in m.items() if k != 'path'} for m in media], key=lambda m: (m['format'], m['sha256'])),
                'sessions': [{'discovery': s['discovery'], 'state': s['state'], 'selected': sorted(s['selected'])} for s in state['sessions']],
                'devices': state['devices'], 'tcc_writes': len(state['permissions'])}
    save(work / 'semantic.json', semantic)


def validate(work, spec):
    manifest = load(work / 'manifest.json')
    assert manifest['schema'] == 2 and manifest['suite'] == spec['name'], 'wrong suite'
    assert manifest['terminal_outcome'] == 'completed', 'incomplete/failed evidence'
    assert manifest['required_artifacts'] == REQUIRED, 'wrong artifact inventory'
    assert sorted(manifest['artifact_hashes'])==sorted(REQUIRED),'missing evidence digest'
    assert manifest['exit_code'] == 1 and manifest['worker_exit'] == 0 and not manifest['timed_out'], 'wrong exits'
    for name in REQUIRED:
        assert (work / name).is_file(), f'missing required artifact {name}'
        if name != 'stderr':
            assert (work / name).stat().st_size, f'empty artifact {name}'
    transcript = rows(work / 'transcript.jsonl')
    assert transcript[-1]['event'] == 'cleanup' and transcript[-1]['detail']['outcome'] == 'completed', 'missing cleanup'
    assert transcript[-1]['detail']['active_processes'] == [], 'leaked virtual jobs'
    assert [x['seq'] for x in transcript] == list(range(len(transcript))), 'truncated transcript'
    for row in transcript:
        assert row['event'] not in ('warning', 'route_error'), 'unmatched/invalid command'
        if row['event'] in ('exec', 'output', 'stdin'):
            data = base64.b64decode(row['detail']['base64'], validate=True)
            assert len(data) == row['detail']['size'] and sha(data) == row['detail']['sha256'], 'corrupt stream'
        if row['event'] == 'route':
            assert not row['detail'].get('warning'), 'unmatched command'
        if row['event']=='background_end':
            assert row['detail']['error'] is None, 'background job failed'
        if row['event']=='exec' and base64.b64decode(row['detail']['base64']).endswith(b'/libxctest-parser.dylib'):
            parser=(row['connection'],row['channel'])
            assert any(r['event']=='stdin' and (r['connection'],r['channel'])==parser for r in transcript),'parser transfer vanished'
            for chunk in (r for r in transcript if r['event']=='stdin' and (r['connection'],r['channel'])==parser):
                bytes_of=base64.b64decode(chunk['detail']['base64'],validate=True)
                assert chunk['detail'].get('redacted')=='D20' and bytes_of==bytes(len(bytes_of)),'third-party library bytes in transcript'
    assert (work / 'stderr').read_bytes().rstrip().endswith(b'Marathon execution failed'), 'wrong final CLI message'
    assert b'Marathon execution crashed' not in (work / 'stderr').read_bytes(), 'CLI crashed'
    host=rows(work/'host-argv.jsonl')
    assert {r['program'] for r in host}=={'ssh','rsync'},'unexpected host shim'
    client=[r['argv'] for r in host if r['program']=='rsync']
    assert sum(a==['--version','/tmp','/tmp'] for a in client)==1,'missing rsync version form'
    assert any(a[-1].startswith('127.0.0.1:/tmp/marathon/') for a in client),'missing host send'
    assert any(a[-2].startswith('127.0.0.1:/tmp/marathon/') for a in client),'missing host receive'
    state = load(work / 'state.json')
    assert not state['deferred'], 'deferred command reached'
    for expectation in spec['expectations']:
        assert expectation['min'] <= state['calls'].get(expectation['name'], 0) <= expectation['max'], 'unconsumed expectation'
    assert all(s['state'] == 'finished' for s in state['sessions']), 'cancelled session'
    assert sum(not s['discovery'] for s in state['sessions']) == 1, 'missing/extra batch'
    if spec['platform'] == 'ios':
        assert all(d['state'] == 'Shutdown' and not d['keyboard'] for d in state['devices'].values()), 'device not disposed'
    else:
        assert len(state['permissions']) == 4, 'both discovery and batch must grant fake system/user TCC'
        permission_counts=Counter()
        prefix='replace into access (service,client,client_type,auth_value,auth_reason,auth_version,indirect_object_identifier,flags,last_modified) values ("kTCCServiceAccessibility","org.marathon.AppleWorld",0,2,1,1,"UNUSED",0,'
        for path,query in state['permissions']:
            assert query.startswith(prefix) and query.endswith(');') and query[len(prefix):-2].isdigit(),'wrong TCC grant'
            permission_counts[path]+=1
        assert permission_counts=={'/Library/Application Support/com.apple.TCC/TCC.db':2,'/Users/worker/Library/Application Support/com.apple.TCC/TCC.db':2},'wrong TCC database'
    result = load(work / 'results.json')
    expected = sorted((spec['target'], t['class'], t['method'], STATUS[t['status']]) for t in spec['tests'])
    actual = sorted((t['package'], t['class'], t['method'], t['status']) for t in result)
    assert actual == expected, 'wrong test outcomes'
    assert all(any(a['type']=='LOG' for a in t['attachments']) for t in result),'missing per-test log attachment'
    assert all(t['failure'] == 'scripted enriched failure\nfile:///fixture/SmokeTests:1' for t in result if t['status'] == 'FAILURE'), 'missing enrichment'
    pushed = load(work / 'pushed-xctestrun.json')
    assert len(pushed) == (2 if spec['platform'] == 'macos' else 1), 'missing pushed xctestrun'
    import plistlib
    for entry in pushed:
        data = base64.b64decode(entry['base64'], validate=True)
        assert sha(data) == entry['sha256'], 'corrupt pushed plist'
        assert plistlib.loads(data)['TestConfigurations'], 'invalid pushed plist'
    report = load(work / 'report-manifest.json')
    assert any(e['path'].endswith('.xcresult/SCRIPTED') for e in report), 'xcresult not pulled'
    assert not any(e['path'].endswith('.xcresult') for e in transcript[-1]['detail']['files']), 'remote result not cleaned'
    media = load(work / 'media.json')
    for i, entry in enumerate(media):
        data = (work / 'media' / str(i)).read_bytes()
        assert len(data) == entry['size'] and sha(data) == entry['sha256'], 'missing/corrupt media'
    if spec['media'] == 'video':
        assert Counter(m['format'] for m in media) == {'mp4': 1, 'log': 1}, 'missing video/log'
        originals=[f for f in final_files(work) if f['path'].endswith('-temp.mp4')]
        assert len(originals)==1 and originals[0]['sha256']==sha((ROOT/'fixtures/fake-worker/apple/media/recording.mp4').read_bytes()),'missing retained original recording'
        assert any(a['type'] == 'VIDEO' for t in result for a in t['attachments']), 'missing video attachment'
    elif spec['media'] == 'screenshot':
        assert len(media) == 1 and media[0]['format'] == 'gif' and (media[0]['width'], media[0]['height']) == (64, 96), 'missing screenshot'
        assert media[0]['frames']==1 and media[0]['delay_centiseconds']==10 and media[0]['loop']==0,'wrong GIF metadata'
        assert any(a['type'] == 'SCREENSHOT_GIF' for t in result for a in t['attachments']), 'missing screenshot attachment'
    else:
        assert not media, 'unexpected media'
    semantic = load(work / 'semantic.json')
    assert semantic['command_counts']==command_counts(transcript),'changed command inventory'
    assert semantic['report_families']==report_families(report),'changed report families'
    assert semantic['junit']['tests'] == len(expected) and semantic['junit']['failures'] == 1, 'wrong JUnit totals'
    assert semantic['junit']['skipped'] == sum(t['status'] == 'skipped' for t in spec['tests']), 'wrong skipped count'
    for name, recorded in manifest['artifact_hashes'].items():
        assert sha((work / name).read_bytes()) == recorded, f'evidence changed: {name}'


def seal(work):
    manifest = load(work / 'manifest.json')
    manifest['required_artifacts'] = REQUIRED
    manifest['terminal_outcome'] = 'completed'
    manifest['artifact_hashes'] = {name: sha((work / name).read_bytes()) for name in REQUIRED}
    save(work / 'manifest.json', manifest)


def curate(work, dest):
    assert not dest.exists(), 'evidence destination already exists'
    dest.mkdir(parents=True)
    for name in REQUIRED + ['manifest.json', 'ready.json', 'worker.stderr']:
        shutil.copy2(work / name, dest / name)
    if (work / 'media').exists():
        shutil.copytree(work / 'media', dest / 'media')


def normalize(raw, dest):
    dest.mkdir(parents=True)
    shutil.copy2(raw / 'semantic.json', dest / 'semantic.json')
    ready=load(raw / 'ready.json')
    ready['endpoint']='127.0.0.1:@PORT@'
    ready['host_key_fingerprint']='@HOST_KEY@'
    save(dest / 'ready.json',ready)
    # Large opaque artifacts live only in raw/. Semantic projection removes
    # timing fields instead of editing raw reports or transcript payloads.
