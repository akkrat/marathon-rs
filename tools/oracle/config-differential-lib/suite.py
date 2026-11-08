"""WP 2.6 driver. Oracle semantics belong to ConfigDump.kt and the Rust binary."""
import argparse
import collections
import concurrent.futures
import copy
import hashlib
import gzip
import json
import os
from pathlib import Path
import platform
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from isolation import tree as isolated_tree

ROOT = Path(__file__).resolve().parents[3]
CORPUS = ROOT / 'fixtures/oracle/config-differential'
NOW = '1970-01-12T13:46:40Z'
JDK = os.environ.get('JAVA_HOME', '/Applications/Android Studio.app/Contents/jbr/Contents/Home')

class Number(str):
    """Keep Java's JSON number spelling, including the exponent and decimal point."""

def loads(text):
    def pairs(items):
        out = {}
        for k, v in items:
            if k in out:
                raise ValueError('duplicate JSON key: ' + k)
            out[k] = v
        return out
    return json.loads(text, parse_int=Number, parse_float=Number, object_pairs_hook=pairs,
                      parse_constant=lambda x: (_ for _ in ()).throw(ValueError(x)))

def read(path):
    return json.loads(path.read_text())

def write(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + '\n')

def command(args, cwd, env, timeout, stdout, stderr, allow_stderr=False):
    with stdout.open('wb') as out, stderr.open('wb') as err:
        child = subprocess.Popen([str(x) for x in args], cwd=cwd, env=env,
                                 stdin=subprocess.DEVNULL, stdout=out, stderr=err, start_new_session=True)
        try:
            rc = child.wait(timeout=timeout)
        except BaseException:
            os.killpg(child.pid, signal.SIGKILL)
            child.wait()
            raise
    if rc or (stderr.stat().st_size and not allow_stderr):
        raise ValueError(f'command rc={rc}, stderr={stderr.read_text()[:2000]!r}: {args[0]} (evidence {stdout.parent})')

def under(root, rel):
    p = Path(rel)
    if p.is_absolute() or '..' in p.parts:
        raise ValueError('uncontained setup path: ' + rel)
    return root / p

def expand(s, root):
    return s.replace('@ROOT@', str(root))

def setup(case, root):
    root.mkdir(parents=True)
    for name in ['config', 'cwd', 'home', 'tmp']:
        (root / name).mkdir()
    profile = case.get('setup', 'fixture')
    if profile == 'fixture':
        shutil.copytree(ROOT / 'fixtures/configuration/fixture/config', root / 'config', dirs_exist_ok=True)
        for p in (ROOT / 'fixtures/oracle/config-dump-inputs').glob('*.yaml'):
            shutil.copyfile(p, root / 'config' / p.name)
    elif profile == 'validation':
        shutil.copytree(ROOT / 'fixtures/configuration/fixture/config/ios/derivedDataDir', root / 'config/derivedDataDir')
        (root / 'config/app.apk').write_text('')
        (root / 'config/apkdir').mkdir()
        (root / 'config/known_hosts').write_text('')
        (root / 'config/loop').symlink_to(root / 'config/loop')
    elif profile != 'empty':
        raise ValueError('unknown setup ' + profile)
    for rel, content in case.get('files', {}).items():
        p = under(root, rel)
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(expand(content, root))
    for rel, target in case.get('links', {}).items():
        p = under(root, rel)
        p.parent.mkdir(parents=True, exist_ok=True)
        # All new archive witnesses are contained. Validation's old self-loop is bounded too.
        if Path(target).is_absolute() or '..' in Path(target).parts:
            raise ValueError('uncontained symlink target ' + target)
        p.symlink_to(target)
    for spec in case.get('archives', []):
        import zipfile
        p = under(root, spec['path'])
        p.parent.mkdir(parents=True, exist_ok=True)
        with zipfile.ZipFile(p, 'w', compression=zipfile.ZIP_STORED) as z:
            for name, data in spec['entries'].items():
                under(root, name)
                info = zipfile.ZipInfo(name, (2020, 1, 1, 0, 0, 0))
                z.writestr(info, data)
    file = under(root / 'config', case.get('input', 'Marathonfile'))
    if 'text' in case:
        file.parent.mkdir(parents=True, exist_ok=True)
        file.write_text(expand(case['text'], root))
    env = {k: expand(v, root) for k, v in case.get('env', {'ANDROID_HOME': '/android-sdk'}).items()}
    env_file = root / 'env'
    env_file.write_text(''.join(k + '=' + v + '\n' for k, v in sorted(env.items())))
    return file, env_file

def snapshot(root, case=None, run_root=None):
    out = {}
    templates = dict(case.get('files',{})) if case else {}
    if case and 'text' in case:
        templates['config/' + case.get('input','Marathonfile')] = case['text']
    for entry in isolated_tree(root, root):
        rel, kind = entry['path'], entry['kind']
        size, digest = entry.get('size'), entry.get('sha256')
        if kind == 'symlink':
            digest = entry['target']
        template = templates.get('config/' + rel)
        if kind == 'file' and template and '@ROOT@' in template:
            if (root/rel).read_bytes() == expand(template,run_root).encode():
                data = template.replace('@ROOT@','<RUN>').encode()
                size, digest = len(data), hashlib.sha256(data).hexdigest()
        out[rel] = {'kind':kind,'size':size,'sha256':digest}
    return dict(sorted(out.items(), key=lambda kv:kv[0].encode('utf-16-be')))

PATH_KEYS = {'outputDir', 'androidSdk', 'applicationApk', 'testApplicationApk', 'extraApplications',
             'pbtxt', 'file', 'source', 'devicesFile', 'privateKey', 'knownHostsPath', 'app', 'testApp',
             'xctest', 'derivedDataDir', 'xctestrun', 'testApplication', 'application', 'keyFile', 'applicationOutput', 'testApplicationOutput',
             'extraApplicationsOutput', 'extraApplicationsApk', 'splitApks', 'importMedia', 'path', 'key', 'mediaFiles'}
SET_PATHS = {('config', 'vendorConfiguration', 'fileSyncConfiguration', 'pull'),
             ('config', 'vendorConfiguration', 'fileSyncConfiguration', 'push'),
             ('config', 'vendorConfiguration', 'permissions', 'grant'),
             ('config', 'vendorConfiguration', 'lifecycleConfiguration', 'onPrepare'),
             ('config', 'vendorConfiguration', 'lifecycleConfiguration', 'onDispose')}

def sandbox_spellings(root):
    # Recorded macOS paths must replay on Linux after the sandbox is gone.
    # Resolving against the replay host loses /tmp and /var's /private aliases.
    spelling = str(root).replace('/System/Volumes/Data/', '/')
    spellings = {spelling}
    for prefix in ['/tmp', '/var']:
        if spelling == prefix or spelling.startswith(prefix + '/'):
            spellings.add('/private' + spelling)
        private = '/private' + prefix
        if spelling == private or spelling.startswith(private + '/'):
            spellings.add(spelling[len('/private'):])
    spellings.update('/System/Volumes/Data' + s for s in list(spellings) if s.startswith('/'))
    return sorted(spellings, key=lambda s: (-len(s), s))

def normalize(value, root, path=()):
    if isinstance(value, dict):
        return {k: normalize(v, root, path + (k,)) for k, v in value.items()}
    if isinstance(value, list):
        return [normalize(v, root, path + (i,)) for i, v in enumerate(value)]
    if isinstance(value, str) and not isinstance(value, Number):
        is_path = (any(k in PATH_KEYS for k in path) and path[:1] == ('config',)
                   and not any(k in {'instrumentationArgs', 'xcodebuildTestArgs', 'xctestrunEnv'} for k in path))
        is_error = path[:1] == ('error',)
        is_link = path[-1:] == ('sha256',) and not re.fullmatch('[0-9a-f]{64}', value)
        if is_path or is_error or is_link:
            for spelling in sandbox_spellings(root):
                value = value.replace(spelling, '<RUN>')
            value = value.replace('/System/Volumes/Data/', '/')
        return value
    return value

def document(text):
    d = loads(text)
    if not isinstance(d, dict) or set(d) != {'stage', 'error', 'config', 'fsEffects'}:
        raise ValueError('invalid dump fields')
    if d['stage'] not in ['parse', 'logical', 'ok']:
        raise ValueError('invalid stage')
    if (d['stage'] == 'parse') != (d['config'] is None):
        raise ValueError('config/stage mismatch')
    if (d['stage'] == 'ok') != (d['error'] is None):
        raise ValueError('error/stage mismatch')
    if d['config'] is not None and not isinstance(d['config'], dict):
        raise ValueError('malformed canonical config')
    if d['error'] is not None:
        e = d['error']
        if set(e) != {'class', 'message', 'causes'} or not isinstance(e['causes'], list):
            raise ValueError('malformed error')
        if (type(e['class']) is not str or (e['message'] is not None and type(e['message']) is not str)
            or not all(type(c) is str for c in e['causes'])):
            raise ValueError('malformed cause')
    if not isinstance(d['fsEffects'], list):
        raise ValueError('malformed effects')
    paths = []
    for e in d['fsEffects']:
        if set(e) != {'op', 'path', 'kind', 'size', 'sha256'} or e['op'] not in ['created', 'deleted', 'modified']:
            raise ValueError('malformed effect')
        if e['kind'] not in ['dir', 'file', 'symlink']:
            raise ValueError('malformed entry kind')
        if type(e['path']) is not str or not e['path'] or e['path'].startswith('/') or '..' in Path(e['path']).parts:
            raise ValueError('malformed effect path')
        paths.append(e['path'])
        if e['kind'] == 'file':
            if not isinstance(e['size'], Number) or not re.fullmatch(r'[0-9]+',e['size']):
                raise ValueError('malformed effect size')
            if type(e['sha256']) is not str or not re.fullmatch(r'[0-9a-f]{64}',e['sha256']):
                raise ValueError('malformed effect hash')
        elif e['size'] is not None or (e['kind']=='dir' and e['sha256'] is not None) or (e['kind']=='symlink' and type(e['sha256']) is not str):
            raise ValueError('malformed non-file effect')
    if paths != sorted(set(paths), key=lambda p:p.encode('utf-16-be')):
        raise ValueError('duplicate or unsorted effects')
    return d

def diagnostic(s):
    # PLAN 3.10 permits generated parser/binding diagnostic wording only. Preserve
    # class, count and order; custom ConfigurationException messages stay exact.
    c = s.partition(':')[0]
    if c.startswith('com.fasterxml.jackson.') or c in {
        'org.yaml.snakeyaml.scanner.ScannerException', 'org.yaml.snakeyaml.parser.ParserException',
        'org.yaml.snakeyaml.reader.ReaderException', 'org.yaml.snakeyaml.error.YAMLException',
    }:
        return c
    return s

def f29(value):
    classes = r'com\.malinskiy\.marathon\.config\.(?:strategy\.[A-Za-z]+StrategyConfiguration\$[A-Za-z]+|CIConfiguration\$[A-Za-z]+)'
    return re.sub(r'(?<![\w.$])(' + classes + r')@[0-9a-f]{1,8}(?![\w])', r'\1@<hash>', value)

def compare(got, want, path=(), native_strings=False):
    label = '$' + ''.join('[' + str(p) + ']' if isinstance(p, int) else '.' + p for p in path)
    if type(got) is not type(want):
        return f'{label}: type/value {got!r} != {want!r}'
    if isinstance(want, dict):
        # D55 changes only the UTF-16/scalar ordering of these canonical map keys.
        if native_strings and path == ('config', 'vendorConfiguration', 'instrumentationArgs'):
            assert list(want) == sorted(want, key=lambda s: s.encode('utf-16-be'))
            want = dict(sorted(want.items()))
        if list(got) != list(want):
            for key in want:
                if key not in got:
                    return f'{label}.{key}: missing expected value {want[key]!r}'
            for key in got:
                if key not in want:
                    return f'{label}.{key}: unexpected value {got[key]!r}'
            index = next(i for i, (g, w) in enumerate(zip(got, want)) if g != w)
            return f'{label}: key order at {index}: {list(got)[index]!r} != {list(want)[index]!r}'
        for k in want:
            d = compare(got[k], want[k], path + (k,), native_strings)
            if d:
                return d
    elif isinstance(want, list):
        if len(got) != len(want):
            return f'{label}: count {len(got)} != {len(want)}'
        is_set = path in SET_PATHS or (path[:4] == ('config', 'vendorConfiguration', 'permissions', 'grant') and len(path) == 5)
        if is_set:
            remaining = list(want)
            for item in got:
                index = next((i for i, v in enumerate(remaining) if compare(item, v) is None), None)
                if index is None:
                    return f'{label}: unexpected set member {item!r}'
                remaining.pop(index)
        else:
            for i, (g, w) in enumerate(zip(got, want)):
                if path == ('error', 'causes'):
                    g, w = diagnostic(g), diagnostic(w)
                d = compare(g, w, path + (i,), native_strings)
                if d:
                    return d
    elif path[:1] == ('toMap',) and len(path) == 2 and path[1] in {
        'pooling', 'sharding', 'sorting', 'batching', 'flakiness', 'retry', 'ciConfiguration',
    } and isinstance(got,str) and isinstance(want,str):
        if f29(got) != f29(want):
            return f'{label}: {got!r} != {want!r}'
    elif got != want:
        return f'{label}: {got!r} != {want!r}'
    return None

def environment(root):
    return {'HOME': str(root / 'home'), 'TMPDIR': str(root / 'tmp') + '/',
            'PATH': '/usr/bin:/bin:/usr/sbin:/sbin', 'JAVA_HOME': JDK}

def run_rust(case, dest, binary):
    file, env_file = setup(case, dest)
    command([binary, 'config-dump', file, '--env-file', env_file, '--now', NOW],
            dest / 'cwd', environment(dest), 20, dest / 'stdout.json', dest / 'stderr')
    doc = document((dest / 'stdout.json').read_text())
    tree = normalize(snapshot(dest / 'config', case, dest), dest)
    write(dest / 'tree.json', tree)
    return normalize(doc, dest), loads(json.dumps(tree))

def cases():
    manifest = read(CORPUS / 'manifest.json')
    inputs = gzip.decompress((CORPUS / 'cases.jsonl.gz').read_bytes())
    rows = [json.loads(s) for s in inputs.decode().splitlines()]
    if len(rows) != manifest['cases'] or len({r['id'] for r in rows}) != len(rows):
        raise ValueError('missing/duplicate case evidence')
    digest = hashlib.sha256(inputs).hexdigest()
    if digest != manifest['inputsSha256']:
        raise ValueError('input evidence changed; regenerate manifest')
    ids = {r['id'] for r in rows}
    for row in manifest['coverage']:
        if set(row['operators']) != set(manifest['operators']):
            raise ValueError('incomplete key/operator ledger: ' + row['field'])
        for op, witness in row['operators'].items():
            if 'case' in witness and witness['case'] not in ids:
                raise ValueError(f'missing coverage witness {row["field"]}/{op}')
            if not ('case' in witness or witness.get('inapplicable')):
                raise ValueError('missing coverage reason')
    return manifest, rows

_PACKED = None

def packed():
    global _PACKED
    if _PACKED is None:
        path = CORPUS / 'outcomes.jsonl.gz'
        _PACKED = {}
        if path.exists():
            with gzip.open(path, 'rt', encoding='utf-8') as stream:
                for line in stream:
                    row = json.loads(line)
                    key = row['evidenceId']
                    if key in _PACKED:
                        raise ValueError('duplicate evidence id ' + key)
                    _PACKED[key] = row
    return _PACKED

def evidence_text(data, field):
    return data[field + 'Text'] if field + 'Text' in data else (CORPUS / data[field]).read_text()

def pack():
    global _PACKED
    manifest, rows = cases()
    output = []
    old = packed()
    for case in rows:
        for suffix in ['', '.linux'] if case.get('linux') else ['']:
            key = case['id'] + suffix
            path = CORPUS / 'evidence' / (key + '.json')
            if path.exists():
                data = read(path)
                data['rawText'] = evidence_text(data, 'raw')
                data['normalizedText'] = normalized_text(data['rawText'], Path(data['sandbox']))
                data.pop('document', None)
            elif key in old:
                data = old[key]
            else:
                raise ValueError('missing evidence ' + key)
            data['tree'] = dict(sorted(data['tree'].items(), key=lambda kv:kv[0].encode('utf-16-be')))
            if 'rawTree' in data:
                data['rawTree'] = dict(sorted(data['rawTree'].items(), key=lambda kv:kv[0].encode('utf-16-be')))
            data['evidenceId'] = key
            data['buildIdentity'] = 'build-identity-linux.json' if suffix else 'build-identity.json'
            output.append(data)
    raw = ''.join(json.dumps(d, ensure_ascii=False, separators=(',',':')) + '\n' for d in output).encode()
    (CORPUS / 'outcomes.jsonl.gz').write_bytes(gzip.compress(raw, mtime=0))
    manifest['evidenceSha256'] = hashlib.sha256((CORPUS / 'outcomes.jsonl.gz').read_bytes()).hexdigest()
    manifest['outcomes'] = len(output)
    manifest['counts'] = {
        'vendor': dict(sorted(collections.Counter(r['vendor'] for r in rows).items())),
        'mutation': dict(sorted(collections.Counter(r['operator'] for r in rows).items())),
        'jvmStage': dict(sorted(collections.Counter(document(r['rawText'])['stage'] for r in output if not r['evidenceId'].endswith('.linux')).items())),
        'rustStage': dict(sorted(collections.Counter(
            read(CORPUS/'alternatives.json').get(r['id'],{}).get('document',{}).get('stage',document(r['rawText'])['stage'])
            for r in output if not r['evidenceId'].endswith('.linux')).items())),
        'approvedAlternatives':len(read(CORPUS/'alternatives.json')),
        'linuxVariants':sum(r['evidenceId'].endswith('.linux') for r in output),
    }
    manifest['retainedEvidence'] = [
        {'path':'fixtures/oracle/config-dump','rows':30,'test':'crates/marathon-cli/tests/config_dump_goldens.rs'},
        {'path':'fixtures/oracle/config-validation-corpus.jsonl','rows':87,'test':'crates/marathon-config/tests/validation_corpus.rs','view':'toMap / typed F29 identity normalization and serialization'},
        {'path':'fixtures/oracle/config-validation-corpus-linux.jsonl','rows':3,'replaces':True},
        {'path':'fixtures/oracle/bundle-probe.jsonl','test':'crates/marathon-config/tests/bundle.rs',
         'deviations':['F21 containment rejection','F24 dangerous-stem rejection','F25 unlink without following symlinks',
                       'F26 directory-creation errors','F27 CRC rejection','F28 partial duplicate extraction keeps the last entry'],
         'reason':'Existing isolated bundle profiles retain their explicit approved Rust outcomes; unsafe extraction is not rerun by this config-dump suite.'},
    ]
    write(CORPUS / 'manifest.json', manifest)
    for name in ['evidence', 'raw', 'normalized']:
        shutil.rmtree(CORPUS / name, ignore_errors=True)
    # A live run must replay the evidence it just recorded, including new cases.
    _PACKED = None
    print(f'packed {len(output)} complete JVM outcomes')

def expected(case, host):
    path = CORPUS / 'evidence' / (case['id'] + ('.linux' if host == 'Linux' and case.get('linux') else '') + '.json')
    data = packed()[path.stem]
    if data['id'] != case['id'] or data['inputSha256'] != hashlib.sha256(json.dumps(case, sort_keys=True).encode()).hexdigest():
        raise ValueError('stale/mismatched evidence: ' + str(path))
    return data

def verify_evidence(manifest, rows, evidence, compressed):
    if hashlib.sha256(compressed).hexdigest() != manifest['evidenceSha256']:
        raise ValueError('truncated/changed packed evidence')
    required = {r['id'] + suffix for r in rows for suffix in (['','.linux'] if r.get('linux') else [''])}
    if set(evidence) != required or len(evidence) != manifest['outcomes']:
        raise ValueError('missing or unexpected packed JVM outcomes')
    inputs = {r['id']: r for r in rows}
    for row in evidence.values():
        case = inputs[row['id']]
        if hashlib.sha256(json.dumps(case, sort_keys=True).encode()).hexdigest() != row['inputSha256']:
            raise ValueError('stale input identity: ' + row['id'])
        raw = evidence_text(row, 'raw')
        if hashlib.sha256(raw.encode()).hexdigest() != row['rawSha256']:
            raise ValueError('truncated/changed raw evidence: ' + row['id'])
        if compare(normalize(document(raw), Path(row['sandbox'])), document(evidence_text(row,'normalized'))):
            raise ValueError('raw/normalised evidence mismatch: ' + row['id'])

def release_overlay(manifest, rows):
    """Source-specific observations replace complete records, never selected output fields."""
    release = ROOT / 'fixtures/oracle/coverage-c95b2128'
    metadata = read(release / 'config-differential-metadata.json')
    blob = (release / 'config-differential.jsonl.gz').read_bytes()
    if metadata['source_sha'] != 'c95b212882c7ba7b1ad91b7981916ed175f1530e' or metadata['baseline_outcomes_sha256'] != manifest['evidenceSha256'] or hashlib.sha256(blob).hexdigest() != metadata['fixture_sha256']:
        raise ValueError('release configuration evidence identity differs')
    captured = [json.loads(line) for line in gzip.decompress(blob).decode().splitlines()]
    overlay = {r['id']:r for r in captured}
    required = {case['id'] for case in rows if case.get('setup','fixture') == 'fixture' or
                ((document(evidence_text(packed()[case['id']], 'raw')).get('config') or {}).get('vendorConfiguration') or {}).get('bundle') is not None}
    if set(overlay) != required or len(captured) != metadata['cases'] or len(captured) != len(required):
        raise ValueError('release configuration overlay selection differs')
    inputs = {case['id']:case for case in rows}
    for row in captured:
        if row['source_sha'] != metadata['source_sha'] or row['inputSha256'] != hashlib.sha256(json.dumps(inputs[row['id']],sort_keys=True).encode()).hexdigest():
            raise ValueError('stale release configuration input')
    return overlay

def release_expected(case, host, release):
    if host == 'Linux' and case.get('linux'):
        record = expected(case, host)
        # These existing Linux failures return no configuration: the new bundle field is unobservable.
        if document(evidence_text(record, 'raw'))['config'] is not None or case.get('setup') != 'validation':
            raise ValueError('host-specific release evidence required: ' + case['id'])
        return record
    return release.get(case['id']) or expected(case, host)

def release_profile_self_test():
    manifest, rows = cases()
    release = release_overlay(manifest, rows)
    for name in ('validation-a-devices-symlink-loop', 'effects-parse'):
        case = next(case for case in rows if case['id'] == name)
        linux = release_expected(case, 'Linux', release)
        macos = release_expected(case, 'Darwin', release)
        assert linux == packed()[name + '.linux']
        assert document(evidence_text(linux, 'raw'))['stage'] == 'parse'
        assert macos == release[name]
        assert 'xctestrun' in document(evidence_text(macos, 'raw'))['config']['vendorConfiguration']['bundle']

def replay(binary, run, selected=None):
    manifest, rows = cases()
    verify_evidence(manifest, rows, packed(), (CORPUS / 'outcomes.jsonl.gz').read_bytes())
    release = release_overlay(manifest, rows)
    if selected:
        rows = [r for r in rows if selected in r['id']]
    if not rows:
        raise ValueError('selection matches no cases')
    alternatives = read(CORPUS / 'alternatives.json')
    failures = []
    stages = collections.Counter()
    def one(case):
        oracle = release_expected(case, platform.system(), release)
        raw = evidence_text(oracle, 'raw')
        if hashlib.sha256(raw.encode()).hexdigest() != oracle['rawSha256']:
            raise ValueError('truncated/changed raw evidence: ' + case['id'])
        want = document(evidence_text(oracle, 'normalized'))
        if compare(normalize(document(raw), Path(oracle['sandbox'])), want):
            raise ValueError('raw/normalised evidence mismatch: ' + case['id'])
        got, tree = run_rust(case, run / case['id'] / 'rust', binary)
        (run / case['id'] / 'oracle.json').write_text(raw)
        alternative = alternatives.get(case['id'])
        if alternative:
            if not alternative.get('approval'):
                raise ValueError('unapproved alternative: ' + case['id'])
            if 'document' in alternative:
                want = document(json.dumps(alternative['document'], ensure_ascii=False))
            else:
                want = copy.deepcopy(want)
                for patch in alternative['patches']:
                    parent = want
                    for part in patch['path'][:-1]: parent = parent[part]
                    key = patch['path'][-1]
                    if compare(parent[key], patch['jvm']):
                        raise ValueError('alternative no longer matches JVM row: ' + case['id'])
                    parent[key] = patch['rust']
        difference = compare(got, want, native_strings=True) or compare(tree, loads(json.dumps(oracle['tree'])), ('filesystem',))
        return case['id'], got['stage'], difference
    with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
        for i, (id, stage, diff) in enumerate(pool.map(one, rows), 1):
            stages[stage] += 1
            if diff:
                failures.append(id + ': ' + diff)
            if i % 500 == 0:
                print(f'replay {i}/{len(rows)}', flush=True)
    print(f'config differential: {len(rows)-len(failures)}/{len(rows)}; stages={dict(stages)}; evidence={run}')
    if failures:
        (run / 'failures.txt').write_text('\n'.join(failures) + '\n')
        raise ValueError('\n'.join(failures[:30]))

def record(run, selected=None):
    manifest, rows = cases()
    previous = packed()
    stable = 0
    if selected:
        rows = [r for r in rows if selected in r['id']]
    if not rows:
        raise ValueError('selection matches no cases')
    def one(case):
        dest = run / case['id'] / 'jvm'
        file, env_file = setup(case, dest)
        command([ROOT / 'tools/oracle/config-dump', file, '--env-file', env_file, '--now', NOW],
                dest / 'cwd', environment(dest), 30, dest / 'stdout.json', dest / 'stderr')
        text = (dest / 'stdout.json').read_text()
        document(text)
        norm = normalized_text(text, dest)
        tree = normalize(snapshot(dest / 'config', case, dest), dest)
        write(dest / 'tree.json', tree)
        stem = case['id'] + ('.linux' if platform.system() == 'Linux' and case.get('linux') else '')
        old = previous.get(stem)
        unchanged = old and old['inputSha256'] == hashlib.sha256(json.dumps(case,sort_keys=True).encode()).hexdigest()
        if unchanged:
            difference = (compare(document(norm), document(evidence_text(old,'normalized')))
                          or compare(loads(json.dumps(tree)), loads(json.dumps(old['tree'])), ('filesystem',)))
            if difference:
                raise ValueError('unstable repeated JVM outcome ' + stem + ': ' + difference)
        rawpath = 'raw/' + stem + '.json'
        normpath = 'normalized/' + stem + '.json'
        (CORPUS / rawpath).parent.mkdir(exist_ok=True)
        (CORPUS / normpath).parent.mkdir(exist_ok=True)
        (CORPUS / rawpath).write_text(text)
        (CORPUS / normpath).write_text(norm)
        data = {'id': case['id'], 'inputSha256': hashlib.sha256(json.dumps(case, sort_keys=True).encode()).hexdigest(),
                'raw': rawpath, 'rawSha256': hashlib.sha256(text.encode()).hexdigest(),
                'normalized': normpath, 'sandbox': str(dest),
                'tree': tree,
                'rawTree': snapshot(dest / 'config'),
                'host': {'os': platform.system(), 'release': platform.release(), 'arch': platform.machine()},
                'cwd': str(dest / 'cwd'), 'env': case.get('env',{'ANDROID_HOME':'/android-sdk'}), 'now': NOW}
        write(CORPUS / 'evidence' / (stem + '.json'), data)
        return bool(unchanged)
    # Each process gets its own cwd/HOME/TMPDIR; System.getenv and java.io.File's
    # cached user.dir cannot safely be changed between cases in one JVM.
    with concurrent.futures.ThreadPoolExecutor(max_workers=6) as pool:
        for i, unchanged in enumerate(pool.map(one, rows),1):
            stable += unchanged
            if i % 100 == 0:
                print(f'recorded {i}/{len(rows)}',flush=True)
    print(f'repeated JVM outcomes stable: {stable}/{len(rows)}',flush=True)
    java = subprocess.run([JDK + '/bin/java', '-version'], capture_output=True, text=True, check=True)
    libs = ROOT / 'tools/oracle/build/marathon-src/oracle-build/install/oracle/lib'
    identity = {'source': 'e47352f25cbc50333900d5d040a5544ff1224501', 'jdk': java.stderr.strip(),
                'jars': {p.name: hashlib.sha256(p.read_bytes()).hexdigest() for p in sorted(libs.glob('*.jar'))},
                'oracleSources': {str(p.relative_to(ROOT)): hashlib.sha256(p.read_bytes()).hexdigest()
                                  for p in sorted((ROOT / 'tools/oracle/module').rglob('*.kt'))}}
    write(CORPUS / ('build-identity-linux.json' if platform.system() == 'Linux' else 'build-identity.json'), identity)

def normalized_text(text, root):
    # Replace only JSON string tokens that normalize() identifies as paths/errors;
    # leave every JSON number token in the original JVM spelling.
    raw = document(text)
    normalized = normalize(raw, root)
    def encode(v, depth=0):
        if isinstance(v, Number): return str(v)
        if isinstance(v, dict):
            return '{' + ','.join(json.dumps(k, ensure_ascii=False) + ':' + encode(x, depth+1) for k, x in v.items()) + '}'
        if isinstance(v, list): return '[' + ','.join(encode(x, depth+1) for x in v) + ']'
        return json.dumps(v, ensure_ascii=False)
    return encode(normalized) + '\n'

def self_test():
    a = document('{"stage":"ok","error":null,"config":{"ordered":[1,2],"vendorConfiguration":{"permissions":{"grant":["a","b"]}}},"fsEffects":[]}')
    for mutate in [lambda x: x.update(stage='logical'), lambda x: x['config'].update(ordered=[Number('2'), Number('1')]),
                   lambda x: x['config'].update(extra=True),
                   lambda x: x['config']['vendorConfiguration']['permissions'].update(grant=['a', 'c'])]:
        b = copy.deepcopy(a); mutate(b)
        assert compare(a, b)
    b = copy.deepcopy(a); b['config']['vendorConfiguration']['permissions']['grant'].reverse()
    assert compare(a, b) is None
    assert compare(Number('1.0'), Number('1'))
    e = {'class':'ConfigurationException','message':'custom','causes':['com.fasterxml.jackson.X: words','org.custom.X: custom']}
    for field, value in [('message','changed'),('class','Other'),('causes',e['causes'][:1]),
                         ('causes',['com.fasterxml.jackson.Y: words',e['causes'][1]]),
                         ('causes',[e['causes'][0],'org.custom.X: changed'])]:
        b = copy.deepcopy(e); b[field] = value
        assert compare(e,b,('error',))
    b = copy.deepcopy(e); b['causes'][0] = 'com.fasterxml.jackson.X: different'
    assert compare(e,b,('error',)) is None
    assert compare({'sha256':'aa'}, {'sha256':'ab'}, ('fsEffects',0))
    assert '$.filesystem.missing' in compare({}, {'missing':{'kind':'file'}}, ('filesystem',))
    for text in ['{}', '{', '{"stage":"ok","stage":"ok"}']:
        try: document(text)
        except (ValueError, TypeError): pass
        else: raise AssertionError('malformed evidence accepted')
    # Canonical user strings and ordered lists are never globally normalised.
    assert normalize({'config':{'name':'/tmp/run/a@deadbeef','outputDir':'/tmp/run/out'}}, Path('/tmp/run')) == {
        'config':{'name':'/tmp/run/a@deadbeef','outputDir':'<RUN>/out'}}
    # These paths need not exist, and no host filesystem lookup may affect them.
    for root in ['/tmp/recorded-run', '/private/tmp/recorded-run',
                 '/var/recorded-run', '/private/var/recorded-run']:
        for spelling in sandbox_spellings(Path(root)):
            value = {'config': {'name': spelling + '/opaque', 'outputDir': spelling + '/out'},
                     'error': {'message': spelling + '/config/Marathonfile'}}
            assert normalize(value, Path(root)) == {
                'config': {'name': spelling + '/opaque', 'outputDir': '<RUN>/out'},
                'error': {'message': '<RUN>/config/Marathonfile'}}
    assert normalize({'config': {'outputDir': '/private/tmp/other/out'}}, Path('/tmp/recorded-run')) == {
        'config': {'outputDir': '/private/tmp/other/out'}}
    identity = 'com.malinskiy.marathon.config.strategy.PoolingStrategyConfiguration$OmniPoolingStrategyConfiguration@'
    assert compare(identity+'a1', identity+'b2', ('toMap','pooling')) is None
    assert compare(identity+'a1', identity+'b2', ('config','name'))
    # Exercise evidence deletion/truncation, even if a tamperer updates the
    # outer checksum. The row inventory and raw-document checksum still fail.
    fixture={'id':'tiny','text':'x'}
    raw='{"stage":"ok","error":null,"config":{},"fsEffects":[]}'
    row={'id':'tiny','inputSha256':hashlib.sha256(json.dumps(fixture,sort_keys=True).encode()).hexdigest(),
         'rawText':raw,'normalizedText':raw,'rawSha256':hashlib.sha256(raw.encode()).hexdigest(),'sandbox':'/tiny'}
    blob=b'evidence'
    manifest={'evidenceSha256':hashlib.sha256(blob).hexdigest(),'outcomes':1}
    verify_evidence(manifest,[fixture],{'tiny':row},blob)
    corrupt=copy.deepcopy(row);corrupt['rawText']=raw[:-1]
    for entries,content in [({},blob),({'tiny':row},blob[:-1]),({'tiny':corrupt},blob)]:
        try: verify_evidence(manifest,[fixture],entries,content)
        except ValueError: pass
        else: raise AssertionError('corrupt evidence accepted')
    path = ('config', 'vendorConfiguration', 'instrumentationArgs')
    assert compare({'ｚ': 'bmp', '😀': 'supplementary'}, {'😀': 'supplementary', 'ｚ': 'bmp'}, path, native_strings=True) is None
    assert compare({'b': 2, 'a': 1}, {'a': 1, 'b': 2}, path, native_strings=True)
    assert compare({'ｚ': 'changed', '😀': 'supplementary'}, {'😀': 'supplementary', 'ｚ': 'bmp'}, path, native_strings=True)
    print('comparator sensitivity: pass')

def build_rust(run):
    env = dict(os.environ)
    env['PATH'] = '/opt/homebrew/opt/rustup/bin:' + str(Path.home()/'.cargo/bin') + ':' + env.get('PATH','')
    cargo = shutil.which('cargo', path=env['PATH'])
    if not cargo:
        raise ValueError('cargo is required for live comparison')
    command([cargo,'build','--locked','-p','marathon-cli','--bin','marathon','--message-format=json'],
            ROOT,env,300,run/'rust-build.jsonl',run/'rust-build.stderr',allow_stderr=True)
    executables = [r['executable'] for r in [json.loads(s) for s in (run/'rust-build.jsonl').read_text().splitlines()]
                   if r.get('reason')=='compiler-artifact' and r.get('executable') and r['target']['name']=='marathon']
    if len(executables)!=1:
        raise ValueError('cargo did not select exactly one marathon executable')
    binary = Path(executables[0]).resolve()
    write(run/'rust-build-identity.json',{'executable':str(binary),'sha256':hashlib.sha256(binary.read_bytes()).hexdigest()})
    return binary

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('mode', choices=['record','replay','live','self-test','pack'])
    parser.add_argument('--binary', type=Path)
    parser.add_argument('--run-dir', type=Path)
    parser.add_argument('--select')
    parser.add_argument('--prepared-oracle',action='store_true',help='Linux recording only: verify the copied reference distribution instead of rebuilding it')
    args = parser.parse_args()
    self_test()
    release_profile_self_test()
    if args.mode == 'self-test': return
    if args.mode == 'pack':
        pack(); return
    run = args.run_dir or Path(tempfile.mkdtemp(prefix='mrs26-'))
    run = run.absolute()
    run.mkdir(parents=True, exist_ok=True)
    if any(run.iterdir()):
        raise ValueError('run directory must be empty: ' + str(run))
    if args.mode == 'live':
        binary = build_rust(run)
        if args.binary and args.binary.resolve() != binary:
            parser.error('--binary differs from the executable selected by the fresh cargo build')
        args.binary = binary
    if args.mode in ['record','live']:
        if args.prepared_oracle:
            libs = ROOT/'tools/oracle/build/marathon-src/oracle-build/install/oracle/lib'
            identity = read(CORPUS/'build-identity.json')
            if {p.name:hashlib.sha256(p.read_bytes()).hexdigest() for p in libs.glob('*.jar')} != identity['jars']:
                raise ValueError('prepared oracle differs from the recorded reference build')
        else:
            command([ROOT/'tools/oracle/build.sh','reference'], ROOT,dict(os.environ),300,
                    run/'oracle-build.stdout',run/'oracle-build.stderr',allow_stderr=True)
        record(run, args.select)
        pack()
    if args.mode in ['replay','live']:
        if not args.binary or not args.binary.is_file():
            parser.error('--binary must select the freshly built marathon executable')
        replay(args.binary.resolve(), run, args.select)

if __name__ == '__main__':
    try: main()
    except (ValueError, OSError, subprocess.SubprocessError) as e:
        print('config differential FAILED: ' + str(e), file=sys.stderr)
        sys.exit(1)
