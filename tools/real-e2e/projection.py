"""Decision 9's semantic fields; original files remain in the evidence envelope."""
from collections import Counter
import json
from pathlib import PurePosixPath
import re
import struct
from urllib.parse import unquote, urlsplit, parse_qs
import xml.etree.ElementTree as ET
import zlib

from common import SCENARIOS, e, require

Invalid = e.Invalid

RAW = 'reports/test_result/raw.json'


MEDIA_SUFFIXES = ('.mp4', '.mov', '.gif', '.webm')

# Decision 7 of WP 7.5. Kotlin attaches a test's log only when the test window caught a log line
# (LogListener.kt:36-43) and the native side does the same (artifacts.rs), so for a test that ran for a few
# milliseconds the presence of its log is ambient. Absence on a side that recorded less than this is informational.
INSTANT_LOG_MS = 10
LOG_KINDS = ('attachment:LOG', 'allure-attachment:text/plain')


def instant_without_log(attempts):
    """True for a single-attempt test that recorded no log and a duration of 0 <= ms < INSTANT_LOG_MS.

    `attempts` holds one (endTime - startTime or None, has a LOG attachment) pair per test-result row. A retried test
    is never waived, nor is a test whose duration is missing, negative or unknown.
    """
    if len(attempts) != 1:
        return False
    duration, has_log = attempts[0]
    return not has_log and type(duration) is int and 0 <= duration < INSTANT_LOG_MS


def waived_tests(log_attempts, kinds_by_test, executions):
    """The tests of one projected side whose missing log is informational; the side must hold no other log fact.

    Kotlin's detail reporter writes only a test's final result, so a retried test has one detail row and
    `log_attempts` alone cannot tell. `executions` holds [raw attempts, Allure results] per test; the waiver needs both to be
    exactly one.
    """
    return sorted(name for name, attempts in log_attempts.items()
                  if instant_without_log(attempts) and executions.get(name) == [1, 1]
                  and not set(kinds_by_test.get(name, ())) & set(LOG_KINDS))


def empty_recording(path, item):
    # The CLI keeps the file it pulled even when the recorder had written nothing yet: an empty .mp4, or the
    # lone byte of a GIF with no frames. Whether such a file exists varied between two Kotlin runs (an empty
    # file, or no file with only the report's reference left), so it is not media and is not projected.
    return item['kind'] == 'file' and PurePosixPath(path).suffix.lower() in MEDIA_SUFFIXES and len(item['data']) <= 1


PNG_SIGNATURE = b'\x89PNG\r\n\x1a\n'
# colour type -> (samples per pixel, permitted bit depths)
PNG_COLOUR = {0: (1, (1, 2, 4, 8, 16)), 2: (3, (8, 16)), 3: (1, (1, 2, 4, 8)), 4: (2, (8, 16)), 6: (4, (8, 16))}
PNG_CRITICAL = (b'IHDR', b'PLTE', b'IDAT', b'IEND')
ADAM7 = ((0, 0, 8, 8), (4, 0, 8, 8), (0, 4, 4, 8), (2, 0, 4, 4), (0, 2, 2, 4), (1, 0, 2, 2), (0, 1, 1, 2))


def decode_png(data):
    """Decision 16: a screenshot must decode completely, not merely start like a PNG.

    Every chunk CRC is checked, no unknown critical chunk may appear, IHDR must be well formed, the concatenated IDAT must inflate to exactly the size
    IHDR implies (so a truncated or padded stream fails), and IEND must be last with nothing after it. Pixels are
    not compared: two Kotlin runs differ in the status-bar clock and animation state.
    """
    require(data[:8] == PNG_SIGNATURE, 'screenshot is not a PNG')
    position, chunks = 8, []
    while True:
        require(position + 12 <= len(data), 'PNG is truncated before IEND')
        length, kind = struct.unpack('>I4s', data[position:position + 8])
        require(length < 2 ** 31 and position + 12 + length <= len(data), 'PNG chunk is truncated')
        body = data[position + 8:position + 8 + length]
        (crc,) = struct.unpack('>I', data[position + 8 + length:position + 12 + length])
        require(zlib.crc32(kind + body) == crc, 'PNG chunk CRC differs')
        require(re.fullmatch(rb'[A-Za-z]{4}', kind) is not None, 'invalid PNG chunk type')
        chunks.append((kind, body))
        position += 12 + length
        if kind == b'IEND':
            break
    require(position == len(data), 'PNG has trailing bytes after IEND')
    require(chunks[0][0] == b'IHDR' and len(chunks[0][1]) == 13 and chunks[-1][1] == b'', 'PNG IHDR/IEND malformed')
    width, height, depth, colour, compression, filtering, interlace = struct.unpack('>IIBBBBB', chunks[0][1])
    require(0 < width < 2 ** 31 and 0 < height < 2 ** 31, 'PNG dimensions are invalid')
    require(colour in PNG_COLOUR and depth in PNG_COLOUR[colour][1], 'PNG colour type/bit depth is invalid')
    require(compression == 0 and filtering == 0 and interlace in (0, 1), 'PNG compression/filter/interlace is invalid')
    kinds = [kind for kind, _ in chunks]
    # An unknown critical chunk (upper-case first letter) is fatal to a decoder such as libpng, however valid its CRC.
    require(all(kind in PNG_CRITICAL for kind in kinds if kind[0] & 0x20 == 0), 'PNG has an unknown critical chunk')
    require(b'PLTE' not in kinds or colour in (2, 3, 6), 'PNG palette in a greyscale image')
    require(kinds.count(b'IHDR') == 1 and kinds.count(b'IEND') == 1, 'PNG repeats IHDR/IEND')
    idat = [i for i, kind in enumerate(kinds) if kind == b'IDAT']
    require(idat and idat == list(range(idat[0], idat[-1] + 1)), 'PNG IDAT chunks are missing or not contiguous')
    if colour == 3:
        palette = [body for kind, body in chunks if kind == b'PLTE']
        require(len(palette) == 1 and len(palette[0]) % 3 == 0 and 0 < len(palette[0]) // 3 <= 2 ** depth,
                'PNG palette is invalid')
        require(kinds.index(b'PLTE') < idat[0], 'PNG palette follows the image data')
    bits = PNG_COLOUR[colour][0] * depth
    if interlace:
        passes = [((width - x + dx - 1) // dx, (height - y + dy - 1) // dy) for x, y, dx, dy in ADAM7]
    else:
        passes = [(width, height)]
    passes = [(w, h) for w, h in passes if w and h]
    # (scanline count, bytes per scanline including its filter-type byte) for each pass
    layout = [(h, 1 + (w * bits + 7) // 8) for w, h in passes]
    expected = sum(h * stride for h, stride in layout)
    inflater = zlib.decompressobj()
    try:
        # The bound stops an inflation bomb; one byte more than expected is enough to prove a mismatch.
        raw = inflater.decompress(b''.join(body for kind, body in chunks if kind == b'IDAT'), expected + 1)
    except zlib.error as error:
        raise Invalid('PNG IDAT does not inflate: ' + str(error)) from error
    require(len(raw) == expected, 'PNG IDAT inflates to %s bytes, header implies %d' % (
        len(raw) if len(raw) <= expected else 'more', expected))
    require(inflater.eof and not inflater.unconsumed_tail and not inflater.unused_data, 'PNG IDAT is not a complete zlib stream')
    # Every scanline starts with its filter type, and libpng rejects anything above 4 ("bad adaptive filter value").
    offset = 0
    for count, stride in layout:
        require(all(raw[offset + row * stride] <= 4 for row in range(count)), 'PNG scanline has an invalid filter type')
        offset += count * stride
    return dict(width=width, height=height)


def screenshot_fact(vendor, data):
    # An emulator screen carries a clock and other live pixels, so two Kotlin runs never produce the same
    # PNG bytes; for Android the projected fact is the decoded size (the pixels are the human witness's).
    # Apple simulators render deterministically and keep the exact hash.
    if vendor != 'Android':
        if data[:8] == PNG_SIGNATURE:
            decode_png(data)
        return dict(sha256=e.sha(data))
    return decode_png(data)


UUID = re.compile(r'[0-9a-fA-F]{8}(?:-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}')


def parse_jacoco(data):
    """Decision 16: parse a whole JaCoCo exec stream (ExecutionDataWriter/Reader), failing on anything else.

    Layout: 0x01 0xC0C0 0x1007 header; then any number of 0x10 session-info blocks (UTF id, long start, long dump)
    and 0x11 execution-data blocks (long id, UTF class name, boolean array as a varint length and packed bits).
    Returns (sessions, classes) where classes is a list of (name, probe count, hit probes).
    """
    position = 0
    def take(n):
        nonlocal position
        require(position + n <= len(data), 'JaCoCo exec is truncated')
        chunk = data[position:position + n]
        position += n
        return chunk
    def utf():
        (n,) = struct.unpack('>H', take(2))
        try:
            return take(n).decode('utf-8')
        except UnicodeDecodeError as error:
            raise Invalid('JaCoCo exec has an invalid UTF string') from error
    require(take(5) == b'\x01\xc0\xc0\x10\x07', 'not a JaCoCo exec file of format 0x1007')
    sessions, classes = 0, []
    while position < len(data):
        (block,) = take(1)
        if block == 0x10:
            utf(); take(16)
            sessions += 1
        elif block == 0x11:
            take(8)
            name = utf()
            count, shift = 0, 0
            while True:
                (byte,) = take(1)
                count |= (byte & 0x7f) << shift
                shift += 7
                require(shift <= 35, 'JaCoCo exec has an oversized probe count')
                if not byte & 0x80:
                    break
            packed = take((count + 7) // 8)
            hits = sum(bin(byte).count('1') for byte in packed)
            require(count % 8 == 0 or packed[-1] >> (count % 8) == 0, 'JaCoCo probe padding bits are set')
            classes.append((name, count, hits))
        else:
            raise Invalid('JaCoCo exec has an unknown block type 0x%02x' % block)
    return sessions, classes


ANDROIDX = 'androidx/'


def coverage_fact(role, data):
    # Gate on which classes the run recorded and how many probes each has; which probes were hit follows the
    # test timing and is informational (outcomes.coverage_hits), not projected. Decision 17: whether an `androidx/`
    # class is present does not gate (two Kotlin runs differ in the font/emoji classes they load lazily), so those
    # sit apart in `androidx` and outcomes.split_androidx compares only the ones present on both sides.
    _, classes = parse_jacoco(data)
    # A multiset, not a set: a second execution-data block for a class with another probe length must show.
    found = sorted((name, count) for name, count, _ in classes)
    return dict(role=role, classes=[list(c) for c in found if not c[0].startswith(ANDROIDX)],
                androidx=[list(c) for c in found if c[0].startswith(ANDROIDX)])


def file_sync_fact(vendor, path, data):
    parts = PurePosixPath(path).parts
    if vendor != 'Android':
        # File-sync outputs are deterministic payloads under pool/device.
        return dict(role='/'.join(parts[4:]), sha256=e.sha(data))
    # Android pulls device-files/<folder>/...; the instrumentation also leaves files named by, and holding, the
    # run's UUIDs and clock. Two Kotlin runs differ there in name and bytes only, so those are projected by
    # content that is stable: the result rows of the device-side Allure files and the class set of a coverage
    # file (decision 16; the whole stream must parse, so a truncated file fails).
    role = UUID.sub('<uuid>', '/'.join(parts[2:]))
    if role.startswith('allure-results/') and role.endswith('-result.json'):
        row = e.loads(data)
        return dict(role='allure-results/<uuid>-result.json', name=row.get('name'), full_name=row.get('fullName'),
                    status=row.get('status'), message=(row.get('statusDetails') or {}).get('message'))
    if role.endswith('.ec'):
        return coverage_fact(role, data)
    return dict(role=role, sha256=e.sha(data))


def ordered(rows):
    # Sorting a list, rather than making a set, preserves duplicate results. Test identity leads the key so a
    # difference inside one row (a missing attachment) cannot reorder rows and show up as a cascade of positional ones.
    def key(row):
        owner = row.get('test') or row.get('tests') or row.get('owners') if isinstance(row, dict) else None
        return e.encoded(owner), e.encoded(row)
    return sorted(rows, key=key)


def identity(package, clazz, method):
    return (package + '.' if package else '') + clazz + '#' + method


def location(url):
    parsed = urlsplit(url)
    fragment = parse_qs(parsed.fragment)
    path = unquote(parsed.path)
    # The suffix is the source filename, not a checkout or machine identity.
    return dict(path=PurePosixPath(path).name,
                line=int(fragment['StartingLineNumber'][0]) if 'StartingLineNumber' in fragment else None,
                end_line=int(fragment['EndingLineNumber'][0]) if 'EndingLineNumber' in fragment else None)


def failure(trace, failure_class=None, message=None):
    trace = trace or ''
    locations = [location(m[0]) for m in re.finditer(r'file://[^\s]+', trace)]
    text = re.sub(r'(?:\r?\n)?file://[^\s]+', '', trace)
    first = text.splitlines()[0] if text else ''
    exception = re.match(r'^([\w.$]+(?:Exception|Error|Failure))(?:: ?(.*))?$', first)
    if exception:
        failure_class = failure_class or exception[1]
        inferred = (exception[2] or '') + text[len(first):]
    else:
        inferred = text
    # Java source frames are stable suffix/line evidence too.
    for match in re.finditer(r'\(([^():/]+\.(?:java|kt|swift|m|mm)):(\d+)\)', trace):
        locations.append(dict(path=match[1], line=int(match[2]), end_line=None))
    return dict(failure_class=failure_class, message=message if message is not None else inferred,
                locations=locations)


def xcvalue(value):
    if isinstance(value, list):
        return [xcvalue(v) for v in value]
    if not isinstance(value, dict):
        return value
    if '_value' in value:
        return value['_value']
    if value.get('_type', {}).get('_name') == 'Array':
        return [xcvalue(v) for v in value.get('_values', [])]
    return {k: xcvalue(v) for k, v in value.items() if k != '_type'}


def xc_projection(data):
    root = xcvalue(data['root'])
    fields = ('testsCount', 'testsFailedCount', 'testsSkippedCount')
    def counts(metrics):
        # xcresulttool omits a zero count: Xcode 26.6 results carry no testsSkippedCount when nothing was skipped.
        require('testsCount' in metrics, 'missing xcresult test count')
        return {k: int(metrics.get(k, 0)) for k in fields}
    def issues(value):
        return ordered([dict(kind=kind, test=issue.get('testCaseName'), message=issue['message'],
                             location=location(issue['documentLocationInCreatingWorkspace']['url'])
                             if issue.get('documentLocationInCreatingWorkspace') else None)
                        for kind, rows in value.items()
                        if kind in ('testFailureSummaries', 'errorSummaries') for issue in rows])
    tests = []
    def visit(node):
        if isinstance(node, list):
            for item in node:
                visit(item)
        elif isinstance(node, dict):
            if 'testStatus' in node:
                tests.append(dict(name=node['identifier'], status=node['testStatus']))
            for value in node.values():
                visit(value)
    visit(xcvalue(data['tests']))
    return dict(counts=counts(root['metrics']), issues=issues(root.get('issues', {})),
                actions=ordered([dict(counts=counts(a['actionResult']['metrics']),
                                      issues=issues(a['actionResult'].get('issues', {})))
                                 for a in root['actions']]), tests=ordered(tests))


def report_path(capture, ref):
    # Resolve the recorded report root, never search by basename: two tests
    # may legitimately attach distinct files with the same basename.
    # The root is the one the CLI saw at capture time: the evidence may have been moved
    # to the controller from the worker that wrote these absolute Allure sources.
    prefix = str(PurePosixPath(capture['capture']['process']['cwd']) / 'report') + '/'
    path = 'reports/' + ref[len(prefix):] if ref.startswith(prefix) else ref
    return path if path.startswith('reports/') else 'reports/' + path


def attachment_owners(capture):
    """Decision 16: path -> the (test, attempt, role, source) records that own it, for media and screenshot keys.

    Both reports vouch for a file's owner and both gate: `test-result` rows carry the attempt (a test's rows in
    end-time order, the order `tests` projects) and the Marathon attachment type; `allure` results carry the Allure
    type and no attempt. Gating only one of them would let the other be rewired without notice.
    """
    files = capture['files']
    results, owners = {}, {}
    for path, item in sorted(files.items()):
        if item['kind'] != 'file':
            continue
        if path.startswith('reports/test_result/') and path.endswith('.json') and path != RAW:
            row = e.loads(item['data']); test = row['test']
            results.setdefault(identity(test['pkg'], test['clazz'], test['method']), []).append(
                (row['endTime'], [(att['file'], att['type']) for att in row.get('attachments', [])]))
        elif path.startswith('reports/allure-results/') and path.endswith('-result.json'):
            row = e.loads(item['data'])
            labels = {r['name']: r['value'] for r in row.get('labels', [])}
            name = identity('', labels['testClass'], labels['testMethod'])
            def collect(node):
                for att in node.get('attachments', []):
                    owners.setdefault(report_path(capture, att['source']), set()).add((name, None, att['type'], 'allure'))
                for step in node.get('steps', []):
                    collect(step)
            collect(row)
    for name, rows in results.items():
        for attempt, (_, attachments) in enumerate(sorted(rows, key=lambda r: r[0])):
            for ref, role in attachments:
                owners.setdefault(report_path(capture, ref), set()).add((name, attempt, role, 'test-result'))
    return {path: [dict(test=t, attempt=a, role=r, source=src)
                   for t, a, r, src in sorted(found, key=lambda o: (o[0], o[3], -1 if o[1] is None else o[1], o[2]))]
            for path, found in owners.items()}


def semantic_projection(capture, policy, inspect_artifact, summary):
    files = capture['files']
    options = SCENARIOS[capture['capture']['scenario']]['projection']
    selected = set(capture['capture']['selected_tests'])
    result = dict(cli_exit=capture['record']['outcome']['cli'], console={}, tests={}, junit=[], allure=[],
                  html=[], xcresult=[], kinds=[], per_test_kinds={}, media=[], screenshots=[], file_sync=[], junit_suites=[])
    observed = {r['id']: [] for r in policy['rules']}
    kinds = set()
    log_attempts = {}
    allure_results = Counter()
    owned = attachment_owners(capture)
    per_test = {name: set() for name in selected}
    def kind(name, test=None):
        kinds.add(name)
        if test is not None:
            require(test in per_test, 'report has unselected test: ' + test)
            per_test[test].add(name)
    def attachment(ref, media=False):
        path = report_path(capture, ref)
        present = path in files and files[path]['kind'] == 'file'
        # Kotlin references a test's recording even when screenrecord left nothing to pull (an ignored or
        # very short test); whether the file exists is projected through `media`, so a dangling video
        # reference is not an error by itself.
        require(present or media, 'missing attachment: ' + path)
        return path
    for stream in ('stdout', 'stderr'):
        block = summary(files[stream]['data']).decode()
        pools, pool, verdicts = {}, None, []
        for line in block.splitlines():
            if line.startswith('Device pool '):
                pool = line[len('Device pool '):].removesuffix(':')
                require(pool not in pools, 'duplicate summary pool')
                pools[pool] = []
            elif re.search(r'\b\d+ (?:passed|failed|ignored|incomplete)', line) and 'time(s)' not in line:
                require(pool is not None, 'summary counts without a pool')
                pools[pool].append(dict(raw=line.strip().startswith('Raw:'),
                                        counts={status: int(n) for n, status in re.findall(
                                            r'(\d+) (passed|failed|ignored|incomplete)', line)}))
            elif line.startswith('Marathon execution '):
                verdicts.append(line)
        result['console'][stream] = dict(pools=pools, verdicts=verdicts)
    if options['complete_tests']:
        require(result['console']['stdout']['pools'] and all(result['console']['stdout']['pools'].values()),
                'missing stdout test counts')
        require(any(v['verdicts'] for v in result['console'].values()), 'missing execution verdict')
    raw = e.loads(files[RAW]['data']) if RAW in files else []
    attempts = {}
    for row in raw:
        name = identity(row['package'], row['class'], row['method'])
        require(row['status'] in ('PASSED', 'FAILURE', 'IGNORED', 'ASSUMPTION_FAILURE', 'INCOMPLETE'), 'unknown status')
        attempts.setdefault(name, []).append({k: row[k] for k in ('status', 'ignored', 'success')})
        kind('raw', name)
    if options['complete_tests']:
        require(set(attempts) == selected, 'selected test inventory differs')
    details, batches = {}, {}
    for path, item in sorted(files.items()):
        if item['kind'] != 'file':
            continue
        data = item['data']
        if path.startswith('reports/'):
            kind('report:' + PurePosixPath(path).parts[1])
        if path.startswith('reports/test_result/') and path.endswith('.json') and path != RAW:
            row = e.loads(data); test = row['test']
            name = identity(test['pkg'], test['clazz'], test['method'])
            detail = dict(status=row['status'], **failure(row.get('stacktrace')))
            details.setdefault(name, []).append((row['endTime'], detail))
            batches.setdefault(row['testBatchId'], set()).add(name)
            kind('test-result', name)
            start, end = row.get('startTime'), row.get('endTime')
            log_attempts.setdefault(name, []).append(
                [end - start if type(start) is int and type(end) is int else None,
                 any(att['type'] == 'LOG' for att in row.get('attachments', []))])
            for att in row.get('attachments', []):
                attachment(att['file'], att['type'] == 'VIDEO')
                kind('attachment:' + att['type'], name)
        elif path.endswith('.xml') and ('/tests/' in path or 'junit' in PurePosixPath(path).name):
            tree = ET.fromstring(data)
            kind('junit')
            for suite in tree.iter('testsuite'):
                result['junit_suites'].append({k: v for k, v in suite.attrib.items()
                                               if k in ('name', 'tests', 'failures', 'errors', 'skipped')})
            for case in tree.iter('testcase'):
                name = identity('', case.attrib.get('classname', ''), case.attrib['name'])
                kind('junit', name)
                elements = []
                for child in case:
                    if child.tag in ('failure', 'skipped', 'error'):
                        elements.append(dict(kind=child.tag, attributes={k: v for k, v in child.attrib.items()
                                                                       if k not in ('time', 'timestamp')},
                                             **failure(''.join(child.itertext()), child.get('type'))))
                result['junit'].append(dict(test=name, elements=elements))
        elif path.startswith('reports/allure-results/') and path.endswith('-result.json'):
            row = e.loads(data)
            labels = {r['name']: r['value'] for r in row.get('labels', [])}
            name = identity('', labels['testClass'], labels['testMethod'])
            kind('allure', name)
            allure_results[name] += 1
            attachments = Counter()
            def allure_attachments(node):
                for att in node.get('attachments', []):
                    attachment(att['source'], att['type'].startswith('video/')); attachments[att['type']] += 1
                    kind('allure-attachment:' + att['type'], name)
                for step in node.get('steps', []):
                    allure_attachments(step)
            allure_attachments(row)
            result['allure'].append(dict(test=name, name=row['name'], full_name=row['fullName'], status=row['status'],
                                        message=row.get('statusDetails', {}).get('message'),
                                        attachments=dict(attachments)))
        elif path.startswith('reports/html/') and path.endswith('.html'):
            kind('html')
            match = re.search(r'^\s*window\.test\s*=\s*(?=\{)', data.decode(), re.M)
            if match:
                row, _ = json.JSONDecoder().raw_decode(data.decode()[match.end():])
                name = identity(row['package_name'], row['class_name'], row['name'])
                kind('html', name)
                result['html'].append(dict(test=name, status=row['status'], **failure(row.get('stacktrace'))))
        elif path.startswith('reports/bill/'):
            kind('billing')
        elif path.startswith('reports/devices/'):
            kind('devices')
        elif path.startswith('reports/logs/'):
            kind('logs')
    for name, rows in attempts.items():
        detail_rows = [d for _, d in sorted(details.get(name, []), key=lambda item: item[0])]
        result['tests'][name] = dict(final_status=detail_rows[-1]['status'] if detail_rows else rows[-1]['status'],
                                     attempt_count=len(rows), attempts=rows, failures=detail_rows)
    for path, item in sorted(files.items()):
        if item['kind'] == 'directory' and path.endswith('.xcresult'):
            data, _ = inspect_artifact(files, dict(kind='xcresult', path=path))
            xc = xc_projection(e.loads(data)); result['xcresult'].append(xc)
            kind('xcresult')
            batch = PurePosixPath(path).stem
            require(batch in batches, 'xcresult has no test batch association')
            for name in batches[batch]:
                kind('xcresult', name)
        if item['kind'] != 'file' or '.xcresult/' in path or empty_recording(path, item):
            continue
        suffix = PurePosixPath(path).suffix.lower()
        owners = owned.get(path, [])
        names = sorted({o['test'] for o in owners})
        if suffix in MEDIA_SUFFIXES:
            rules = [r for r in policy['rules'] if e.path_glob(path, r['path'])]
            require(len(rules) == 1, 'media needs exactly one reviewed P3 rule: ' + path)
            rule = rules[0]
            data, facts = inspect_artifact(files, dict(rule, path=path))
            observed[rule['id']].append(facts)
            result['media'].append(dict(role=rule['id'], owners=owners, facts=e.loads(data)))
            kind('media')
            for name in names:
                kind('media', name)
            # Animated screenshots retain their artifact role while P3 governs
            # their changing pixels, timing and frame count.
            if suffix == '.gif' and path.startswith('reports/screenshot/'):
                kind('screenshot')
                for name in names:
                    kind('screenshot', name)
        elif suffix in ('.png', '.jpg', '.jpeg'):
            kind('screenshot')
            for name in names:
                kind('screenshot', name)
            result['screenshots'].append(dict(owners=owners, **screenshot_fact(SCENARIOS[capture['capture']['scenario']]['vendor'], item['data'])))
        elif path.startswith('reports/device-files/'):
            result['file_sync'].append(file_sync_fact(SCENARIOS[capture['capture']['scenario']]['vendor'], path, item['data']))
            kind('file-sync')
    require(set(options['required_kinds']) <= kinds, 'missing report/artifact kinds: ' +
            ', '.join(sorted(set(options['required_kinds']) - kinds)))
    executions = {name: [len(attempts.get(name, [])), allure_results[name]] for name in log_attempts}
    waived = set(waived_tests(log_attempts, per_test, executions))
    for name, values in per_test.items():
        if options['complete_tests']:
            # Decision 7: a near-instant test may have no log; whether it does is compared in outcomes.split_log_presence.
            needed = set(options['per_test_kinds']) - ({'attachment:LOG'} if name in waived else set())
            require(needed <= values, 'missing per-test artifact kinds: ' + name + ': ' + ', '.join(sorted(needed - values)))
    require(all(observed.values()), 'unused P3 media rule')
    result['kinds'] = sorted(kinds)
    result['per_test_kinds'] = {k: sorted(v) for k, v in per_test.items()}
    result['log_attempts'] = {k: log_attempts[k] for k in sorted(log_attempts)}
    result['log_executions'] = {k: executions[k] for k in sorted(executions)}
    for field in ('junit', 'junit_suites', 'allure', 'html', 'xcresult', 'media', 'screenshots', 'file_sync'):
        result[field] = ordered(result[field])
    return result, observed
