"""Versioned differential evidence. Harness serialization only; product bytes stay opaque."""
import base64
import hashlib
import gzip
import zlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import stat
import tempfile
import xml.etree.ElementTree as ET

FORMAT = 'marathon-differential/evidence@1'
COMPRESSED_FORMAT = 'marathon-differential/evidence@2'
SOURCE = 'e47352f25cbc50333900d5d040a5544ff1224501'


class Invalid(ValueError):
    pass


def require(condition, message):
    if not condition:
        raise Invalid(message)


def sha(data):
    return hashlib.sha256(data).hexdigest()


def encoded(value):
    return (json.dumps(value, ensure_ascii=False, indent=2, sort_keys=True) + '\n').encode()


def unique(pairs):
    out = {}
    for key, value in pairs:
        require(key not in out, f'duplicate JSON key: {key}')
        out[key] = value
    return out


def loads(data):
    return json.loads(data, object_pairs_hook=unique,
                      parse_constant=lambda x: (_ for _ in ()).throw(Invalid(f'nonfinite JSON: {x}')))


def load(path):
    return loads(Path(path).read_bytes())


def path_glob(path, pattern):
    """A single star matches one path component; only ** spans directories."""
    import fnmatch
    parts, patterns = path.split('/'), pattern.split('/')
    def match(i, j):
        if j == len(patterns):
            return i == len(parts)
        if patterns[j] == '**':
            return match(i, j + 1) or (i < len(parts) and match(i + 1, j))
        return i < len(parts) and fnmatch.fnmatchcase(parts[i], patterns[j]) and match(i + 1, j + 1)
    return match(0, 0)


def relative(path):
    require(isinstance(path, str) and path and '\\' not in path and '\x00' not in path,
            f'invalid path: {path!r}')
    p = PurePosixPath(path)
    require(not p.is_absolute() and '..' not in p.parts and p.as_posix() == path and path != '.',
            f'noncanonical path: {path}')
    return path


def snapshot(root, prefix):
    """lstat traversal, including the root and empty directories; never follow links."""
    root = Path(root)
    entries, payloads = [], {}

    def visit(path, name):
        mode = path.lstat().st_mode
        row = dict(path=relative(name), mode=stat.S_IMODE(mode))
        if stat.S_ISLNK(mode):
            row.update(kind='link', target=os.readlink(path))
        elif stat.S_ISDIR(mode):
            row.update(kind='directory')
        elif stat.S_ISREG(mode):
            data = path.read_bytes()
            digest = sha(data)
            row.update(kind='file', size=len(data), sha256=digest)
            payloads[digest] = data
        else:
            raise Invalid(f'unsupported file kind: {path}')
        entries.append(row)
        if row['kind'] == 'directory':
            for child in sorted(path.iterdir()):
                visit(child, name + '/' + child.name)
    visit(root, prefix)
    return entries, payloads


def payload(directory, row, record):
    data = (Path(directory) / 'blobs' / row['sha256']).read_bytes()
    if record['format'] == COMPRESSED_FORMAT:
        try:
            data = gzip.decompress(data)
        except (OSError, EOFError, zlib.error) as error:
            raise Invalid('corrupt compressed payload: ' + row['path']) from error
    return data


def save_payload(directory, digest, data, record):
    if record['format'] == COMPRESSED_FORMAT:
        data = gzip.compress(data, mtime=0)
    (Path(directory) / 'blobs' / digest).write_bytes(data)


def write_record(destination, contract, implementation, trees, artifacts, outcome, provenance, identities=None, compressed=False):
    """Destination must be new. Failed staging is intentionally retained by the caller."""
    destination = Path(destination)
    destination.mkdir()
    blobs = destination / 'blobs'
    blobs.mkdir()
    entries, payloads = [], {}
    for name, root in trees.items():
        rows, content = snapshot(root, name)
        entries.extend(rows)
        payloads.update(content)
    for name, data in artifacts.items():
        entries.append(dict(path=relative(name), kind='file', mode=0o644, size=len(data), sha256=sha(data)))
        payloads[sha(data)] = data
    record = dict(format=FORMAT, contract=sha(encoded(contract)), implementation=implementation,
                  source=provenance.get("source_sha", SOURCE), provenance=provenance, identities=identities or {},
                  outcome=outcome, entries=sorted(entries, key=lambda e: e['path']))
    if compressed:
        record.update(format=COMPRESSED_FORMAT, storage='gzip@1')
    for digest, data in payloads.items():
        save_payload(destination,digest,data,record)
    (destination / 'evidence.json').write_bytes(encoded(record))
    validate(destination, contract)
    return destination


def bindings(record, contract):
    values = record['identities']
    require(set(values) == set(contract['roles']), 'identity role inventory differs')
    seen = set()
    for role, kind in contract['roles'].items():
        value = values[role]
        require(isinstance(value, str) and value and '{' not in value and '}' not in value, f'invalid role: {role}')
        require((kind, value) not in seen, f'identity collision: {role}')
        seen.add((kind, value))
        if kind in ('uuid', 'batch'):
            require(re.fullmatch(r'[0-9a-fA-F]{8}(-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}', value), f'invalid UUID: {role}')
        else:
            require(kind in ('path', 'device', 'worker', 'endpoint', 'channel', 'pid', 'identity'), f'unknown role type: {kind}')
    return values


def expand(template, values):
    return re.sub(r'\{([^{}]+)\}', lambda m: values[m[1]], template)


def canonical_paths(record, contract):
    values = bindings(record, contract)
    mapping = {}
    for template in contract['paths']:
        raw = expand(template, values)
        require(raw not in mapping, 'path normalization collision')
        mapping[raw] = template
    actual = {row['path'] for row in record['entries']}
    require(set(mapping) <= actual, f'missing declared paths: {sorted(set(mapping) - actual)}')
    if contract.get('scenario') == 'vendor':
        import vendors
        for name in actual:
            mapping.setdefault(name, vendors.canonical(name, values))
    normalized = [mapping.get(row['path'], row['path']) for row in record['entries']]
    require(len(set(normalized)) == len(normalized), 'normalized path collision')
    return mapping


def validate_jvm_provenance(facts):
    for field in ('jars', 'patches'):
        values = facts.get(field)
        require(isinstance(values, dict) and values, f'missing JVM {field} provenance')
        require(all(isinstance(k, str) and k and isinstance(v, str) and re.fullmatch('[0-9a-f]{64}', v)
                    for k, v in values.items()), f'invalid JVM {field} provenance')
    require(isinstance(facts.get('jdk'), str) and facts['jdk'].strip(), 'missing JVM jdk provenance')
    process = facts.get('process')
    require(isinstance(process, dict), 'missing JVM process provenance')
    require(isinstance(process.get('argv'), list) and process['argv']
            and all(isinstance(x, str) and x for x in process['argv'])
            and type(process.get('exit')) is int and type(process.get('watchdog')) is bool,
            'invalid JVM process provenance')


def observed_outcome(contract, observation, process=None, transcript=None):
    """Current adapters have no separate CLI/fake process and join before export.

    In-process native tests export only after the operation returns. Spawned
    captures additionally have a waited process result, including watchdog state.
    """
    require(process is None or (type(process.get('exit')) is int
            and type(process.get('watchdog')) is bool), 'missing process outcome')
    completed = (observation.get('outcome', {}).get('completed') is True
                 if contract['scenario'] == 'parse' else type(observation.get('exit')) is int)
    unused, unmatched = [], []
    if transcript is not None:
        require(isinstance(transcript, dict) and isinstance(transcript.get('errors'), list)
                and isinstance(transcript.get('expectations'), list)
                and all(isinstance(x, dict) and type(x.get('ok')) is bool for x in transcript['expectations'])
                and isinstance(transcript.get('outcome'), dict), 'malformed JVM transcript')
        require(not transcript['errors'] and all(x['ok'] for x in transcript['expectations']),
                'failed JVM transcript')
        require(transcript['outcome'] == observation['outcome'], 'inconsistent JVM outcome')
        report = transcript.get('hooksReport')
        require(not contract.get('uses_tapes', False) or
                (isinstance(report, dict) and report.get('active') is True), 'missing tape accounting')
        if report is not None:
            require(isinstance(report.get('unused'), list) and isinstance(report.get('failures'), list),
                    'invalid tape accounting')
            unused, unmatched = report['unused'], report['failures']
    else:
        require(not contract.get('uses_tapes', False), 'missing tape accounting')
    return dict(cli=process['exit'] if process else 0, fake=None,
                harness=process['exit'] if process else 0,
                watchdog=process['watchdog'] if process else False,
                joined=completed and (process is None or not process['watchdog']),
                unused=unused, unmatched=unmatched)


def validate(directory, contract):
    directory = Path(directory)
    require(contract.get('format') == 'marathon-differential/contract@1', 'unknown contract version')
    require(contract.get('scenario') in ('local', 'parse', 'vendor', 'artifact'), 'missing/unknown scenario adapter')
    require(set(p.name for p in directory.iterdir()) == {'evidence.json', 'blobs'}, 'unexpected evidence inventory')
    require(not (directory / 'evidence.json').is_symlink() and not (directory / 'blobs').is_symlink(), 'linked evidence storage')
    record = load(directory / 'evidence.json')
    fields = {'format', 'contract', 'implementation', 'source', 'provenance', 'identities', 'outcome', 'entries'}
    if record.get('format') == COMPRESSED_FORMAT:
        fields.add('storage')
        require(record.get('storage') == 'gzip@1','unknown payload storage')
    require(set(record) == fields, 'evidence schema')
    require(record['format'] in (FORMAT,COMPRESSED_FORMAT) and record['source'] in (SOURCE, 'c95b212882c7ba7b1ad91b7981916ed175f1530e'), 'unknown evidence/source version')
    require(record['contract'] == sha(encoded(contract)), 'contract digest differs')
    require(record['implementation'] in ('jvm', 'native'), 'unknown implementation')
    require(isinstance(record['provenance'], dict) and record['provenance'], 'missing provenance')
    if record['source'] != SOURCE:
        require(record['provenance'].get('source_sha') == record['source'], 'release evidence lacks explicit source provenance')
    if 'source_sha' in record['provenance']:
        require(record['source'] == record['provenance']['source_sha'], 'record/provenance source differs')
    if record['implementation'] == 'jvm':
        validate_jvm_provenance(record['provenance'])
    require(encoded(record['outcome']) == encoded(contract['outcome']), 'incomplete or unexpected terminal outcome')
    require(record['outcome']['harness'] == 0 and record['outcome']['watchdog'] is False
            and record['outcome']['joined'] is True and record['outcome']['unused'] == []
            and record['outcome']['unmatched'] == [], 'failed cleanup/tape/command completion')
    require(isinstance(record['entries'], list) and record['entries'], 'empty manifest')
    paths = canonical_paths(record, contract)
    names, digests = set(), set()
    for row in record['entries']:
        name = relative(row['path'])
        require(name not in names, f'duplicate path: {name}')
        names.add(name)
        require(type(row['mode']) is int and 0 <= row['mode'] <= 0o7777, f'invalid mode: {name}')
        fields = {'path', 'kind', 'mode'}
        if row['kind'] == 'file':
            fields |= {'size', 'sha256'}
            digest = row['sha256']
            require(isinstance(digest, str) and re.fullmatch('[0-9a-f]{64}', digest), 'invalid digest')
            p = directory / 'blobs' / digest
            require(p.is_file() and not p.is_symlink(), f'missing payload: {name}')
            data = payload(directory,row,record)
            require(type(row['size']) is int and len(data) == row['size'] and sha(data) == digest, f'corrupt payload: {name}')
            canonical = paths.get(name, name)
            spec = contract['normalization'].get(canonical, {'mode':'bytes', 'rules':[]})
            structured = spec['mode'] in ('allure-json', 'causal-streams') or any(
                r['kind'] == 'json-string' for r in spec['rules'])
            if structured and not any(path_glob(canonical, p) for p in contract.get('opaque_json', [])):
                if name.endswith('.json'):
                    loads(data)
                elif name.endswith('.jsonl'):
                    require(data.endswith(b'\n') and data != b'\n', f'truncated/empty JSONL: {name}')
                    for line in data[:-1].split(b'\n'):
                        require(isinstance(loads(line), dict), f'JSONL row is not an object: {name}')
            digests.add(digest)
        elif row['kind'] == 'link':
            fields.add('target')
            require(isinstance(row['target'], str) and row['target'] and '\x00' not in row['target'], 'invalid link')
        else:
            require(row['kind'] == 'directory', f'unknown kind: {name}')
        require(set(row) == fields, f'entry schema: {name}')
    require(set(p.name for p in (directory / 'blobs').iterdir()) == digests, 'blob inventory differs')
    for row in record['entries']:
        parent = str(PurePosixPath(row['path']).parent)
        if parent != '.':
            require(any(r['path'] == parent and r['kind'] == 'directory' for r in record['entries']), f'missing parent: {parent}')
    if contract.get('scenario') in ('local', 'parse'):
        row = next((r for r in record['entries'] if r['path'] == 'observation.json'), None)
        require(row is not None and row['kind'] == 'file', 'missing observation')
        observation = loads(payload(directory,row,record))
        facts = record['provenance']
        if contract['scenario'] == 'parse' and record['implementation'] == 'jvm':
            require(isinstance(facts.get('driver_transcript'), dict), 'missing JVM driver transcript')
        outcome = observed_outcome(contract, observation, facts.get('process'), facts.get('driver_transcript'))
        require(encoded(record['outcome']) == encoded(outcome), 'outcome differs from observations')
    else:
        facts = record['provenance']
        observed = facts.get('terminal_observation')
        require(isinstance(observed, dict) and encoded(observed) == encoded(record['outcome']),
                'outcome differs from terminal observations')
        if contract['scenario'] == 'vendor':
            import vendors
            vendors.validate_record(directory, contract, record)
    return record


def json_spans(data, containers=False):
    """Locate scalar tokens by JSON pointer without reserializing Gson bytes."""
    text = data.decode('utf-8')
    decoder = json.JSONDecoder(object_pairs_hook=unique)
    spans = {}

    def space(i):
        while i < len(text) and text[i] in ' \t\r\n':
            i += 1
        return i

    def value(i, pointer):
        i = space(i)
        start = i
        if text[i] == '{':
            i = space(i + 1)
            keys = set()
            if text[i] != '}':
                while True:
                    key, end = decoder.raw_decode(text, i)
                    require(isinstance(key, str) and key not in keys, 'duplicate/invalid JSON key')
                    keys.add(key)
                    i = space(end)
                    require(text[i] == ':', 'missing JSON colon')
                    i = space(value(i + 1, pointer + '/' + key.replace('~', '~0').replace('/', '~1')))
                    if text[i] != ',':
                        break
                    i = space(i + 1)
            require(text[i] == '}', 'incomplete JSON object')
            if containers:
                spans[pointer] = (start, i+1, loads(text[start:i+1]))
            return i + 1
        if text[i] == '[':
            i = space(i + 1)
            n = 0
            if text[i] != ']':
                while True:
                    i = space(value(i, pointer + '/' + str(n)))
                    n += 1
                    if text[i] != ',':
                        break
                    i = space(i + 1)
            require(text[i] == ']', 'incomplete JSON array')
            if containers:
                spans[pointer] = (start, i+1, loads(text[start:i+1]))
            return i + 1
        result, end = decoder.raw_decode(text, i)
        spans[pointer] = (start, end, result)
        return end
    require(space(value(0, '')) == len(text), 'trailing JSON bytes')
    return text, spans


def normalize(data, rules, values):
    text = data.decode('utf-8') if rules else None
    edits = []
    for rule in rules:
        expected = expand(rule['value'], values)
        replacement = rule['value']
        if rule['kind'] == 'json-string':
            _, spans = json_spans(data)
            require(rule['pointer'] in spans, f'missing JSON location: {rule["pointer"]}')
            start, end, value = spans[rule['pointer']]
            require(value == expected, f'inconsistent reference: {rule["pointer"]}')
            replacement = json.dumps(replacement, ensure_ascii=False)
        elif rule['kind'] == 'text':
            # Literal anchors belong to the reviewed contract, never to a recording.
            prefix, suffix = rule['prefix'], rule['suffix']
            require(prefix and suffix and text.count(prefix) == 1, 'ambiguous text anchor')
            start = text.index(prefix) + len(prefix)
            end = text.find(suffix, start)
            require(end >= start and text[start:end] == expected, 'inconsistent anchored reference')
        else:
            raise Invalid(f'unknown normalization: {rule["kind"]}')
        edits.append((start, end, replacement))
    edits.sort()
    require(all(a[1] <= b[0] for a, b in zip(edits, edits[1:])), 'overlapping normalization')
    for start, end, value in reversed(edits):
        text = text[:start] + value + text[end:]
    return text.encode() if rules else data


def xml_value(data):
    def node(e):
        return (e.tag, sorted(e.attrib.items()), e.text or '', e.tail or '', [node(c) for c in e])
    require(b'<!DOCTYPE' not in data and b'<!ENTITY' not in data, 'XML entities are not supported')
    return node(ET.fromstring(data))


def normalized(directory, contract):
    record = validate(directory, contract)
    paths = canonical_paths(record, contract)
    files = {}
    rules = contract['normalization']
    used = set()
    for row in record['entries']:
        name = paths.get(row['path'], row['path'])
        entry = {k: v for k, v in row.items() if k not in ('path', 'size', 'sha256')}
        if row['kind'] == 'link' and contract.get('scenario') == 'vendor':
            import vendors
            entry['mode'] = vendors.link_mode(name, row['mode'], record, contract)
        if row['kind'] == 'file':
            data = payload(directory,row,record)
            if contract.get('scenario') == 'vendor' and name.startswith('reports/'):
                import vendor_reports
                data = vendor_reports.normalize(name, data, record['identities'], record['implementation'], contract)
            spec = rules.get(name, {'rules': [], 'mode': 'bytes'})
            if name in rules:
                used.add(name)
            data = normalize(data, spec['rules'], record['identities'])
            mode = spec['mode']
            if contract.get('scenario') == 'vendor' and name in ('stdout','stderr','transcript.jsonl','host-argv.jsonl'):
                import vendor_console, vendor_transport
                if name in ('stdout','stderr'):
                    content = base64.b64encode(vendor_console.normalize(data,record['identities'],record['implementation'],name)).decode()
                elif name == 'host-argv.jsonl':
                    content = vendor_transport.host_argv(data,record['identities'])
                else:
                    content = vendor_transport.ssh(data,contract['vendor']) if contract['transport'] == 'ssh' else vendor_transport.adb(data)
            elif mode == 'allure-json':
                require(name.startswith('reports/allure-results/') and name.endswith('.json'), 'Allure scope')
                # Numeric lexemes remain exact, arrays ordered, null distinct from omission.
                content = json.loads(data, object_pairs_hook=unique, parse_int=lambda x: ('number', x),
                                     parse_float=lambda x: ('number', x))
            elif mode == 'causal-streams':
                content = causal_streams(data)
            elif mode == 'allure-environment':
                require(name == 'reports/allure-results/environment.xml', 'environment scope')
                content = xml_value(data)
            elif mode == 'gif':
                require(name.startswith('reports/screenshot/') and name.endswith('.gif'), 'GIF scope')
                import gif_frames
                content = gif_frames.decode(data)
            else:
                require(mode == 'bytes', f'unknown comparison mode: {mode}')
                content = base64.b64encode(data).decode()
            entry['content'] = content
        elif row['kind'] == 'link':
            # A link is opaque unless its complete target is explicitly declared.
            if name in contract.get('links', {}):
                template = contract['links'][name]
                require(row['target'] == expand(template, record['identities']), f'inconsistent link: {name}')
                entry['target'] = template
        files[name] = entry
    require(used == set(rules), 'unused normalization rules')
    return files


def first_difference(left, right, pointer=''):
    if isinstance(left, dict) and isinstance(right, dict):
        for key in sorted(set(left) | set(right), key=lambda key: (key != 'wire', key)):
            if left.get(key) != right.get(key):
                return first_difference(left.get(key), right.get(key), pointer + '/' + str(key))
    elif isinstance(left, list) and isinstance(right, list):
        for i in range(max(len(left), len(right))):
            a, b = left[i] if i < len(left) else None, right[i] if i < len(right) else None
            if a != b:
                return first_difference(a, b, pointer + '/' + str(i))
    return pointer + ': ' + repr(left)[:180] + ' -> ' + repr(right)[:180]


def compare(expected, actual, contract, mismatch=None):
    left = right = None
    try:
        left, right = normalized(expected, contract), normalized(actual, contract)
        differences = [name for name in sorted(set(left) | set(right)) if left.get(name) != right.get(name)]
        details = []
        for name in differences[:20]:
            a, b = left.get(name), right.get(name)
            fields = [key for key in sorted(set(a or {}) | set(b or {})) if (a or {}).get(key) != (b or {}).get(key)]
            metadata = [key + ': ' + repr((a or {}).get(key)) + ' -> ' + repr((b or {}).get(key))
                        for key in fields if key != 'content']
            if 'content' in fields:
                metadata.append('content differs at ' + first_difference((a or {}).get('content'), (b or {}).get('content')))
            details.append(name + ' [' + ', '.join(metadata or ['entry missing']) + ']')
        require(not differences, 'observable differences: ' + ', '.join(details))
    except (Invalid, OSError, ValueError, KeyError, IndexError, TypeError) as error:
        if mismatch is not None:
            mismatch = Path(mismatch)
            mismatch.mkdir(parents=True, exist_ok=False)
            (mismatch / 'error.txt').write_text(str(error) + '\n')
            if left is not None and right is not None:
                (mismatch / 'expected.normalized.json').write_bytes(encoded(left))
                (mismatch / 'actual.normalized.json').write_bytes(encoded(right))
                (mismatch / 'differences.json').write_bytes(encoded(differences))
            # Even malformed evidence is retained without following linked storage.
            for label, source in [('expected', expected), ('actual', actual)]:
                if Path(source).exists():
                    shutil.copytree(source, mismatch / label, symlinks=True)
        raise Invalid(str(error)) from error


def publish(staging, destination, contract):
    """New immutable generation only: a rename publishes the entire validated JVM recording."""
    staging, destination = Path(staging), Path(destination)
    require(validate(staging, contract)['implementation'] == 'jvm', 'goldens must originate on the JVM')
    normalized(staging, contract)
    require(not destination.exists(), 'golden generation already exists; choose a new generation')
    destination.parent.mkdir(parents=True, exist_ok=True)
    require(staging.stat().st_dev == destination.parent.stat().st_dev, 'publication requires the same filesystem')
    os.rename(staging, destination)


def causal_streams(data):
    """Session lanes are explicit scenario identities; no inferred global ordering.

    Chunk boundaries are transport diagnostics. Aggregate each stream between the
    command and its terminal result, preserving every byte, barrier and outcome.
    """
    rows = loads(data)
    require(isinstance(rows, list) and rows, 'empty causal transcript')
    lanes = {}
    for row in rows:
        require(isinstance(row, dict) and isinstance(row.get('lane'), str) and row['lane'], 'missing causal lane')
        lane = lanes.setdefault(row['lane'], {'next':0, 'command':None, 'stdout':b'', 'stderr':b'', 'end':None})
        require(type(row['seq']) is int and row['seq'] == lane['next'], 'truncated/reordered lane')
        lane['next'] += 1
        require(lane['end'] is None, 'event after terminal outcome')
        common = {'lane','seq','event'}
        if row['event'] == 'command':
            require(set(row) == common | {'argv','stdin','timeout_ms','idle_ms','barriers'}, 'command schema')
            require(lane['command'] is None and row['seq'] == 0, 'duplicate/late command')
            require(isinstance(row['argv'], list) and row['argv'] and all(isinstance(x,str) for x in row['argv']), 'argv schema')
            require(all(type(row[k]) is int for k in ('timeout_ms','idle_ms')), 'timeout schema')
            require(isinstance(row['barriers'], list) and all(isinstance(x,str) for x in row['barriers']), 'barrier schema')
            base64.b64decode(row['stdin'], validate=True)
            lane['command'] = {k:v for k,v in row.items() if k not in common}
        elif row['event'] == 'chunk':
            require(set(row) == common | {'stream','base64'} and lane['command'] is not None, 'chunk schema')
            require(row['stream'] in ('stdout','stderr'), 'unknown stream')
            lane[row['stream']] += base64.b64decode(row['base64'], validate=True)
        else:
            require(row['event'] == 'end' and set(row) == common | {'exit','outcome'} and lane['command'] is not None, 'terminal schema')
            require(row['outcome'] in ('completed','timeout','cancelled','disconnected'), 'unknown terminal outcome')
            require(row['exit'] is None or type(row['exit']) is int, 'exit schema')
            lane['end'] = {k:row[k] for k in ('exit','outcome')}
    for lane in lanes.values():
        require(lane['command'] is not None and lane['end'] is not None, 'unfinished session')
        del lane['next']
        for stream in ('stdout','stderr'):
            lane[stream] = base64.b64encode(lane[stream]).decode()
    return lanes


def compare_variants(expected, actual, contract, mismatch=None):
    """P4: a finite set of complete JVM witnesses, never optional file families."""
    require(isinstance(expected, list) and expected, 'no witnessed variants')
    for directory in expected:
        require(validate(directory, contract)['implementation'] == 'jvm', 'variant is not JVM evidence')
        normalized(directory, contract)
    normalized(actual, contract)
    for directory in expected:
        try:
            compare(directory, actual, contract)
            return
        except Invalid:
            pass
    compare(expected[0], actual, contract, mismatch)
