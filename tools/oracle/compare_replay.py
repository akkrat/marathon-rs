#!/usr/bin/env python3
"""Strict repeated-run comparison; explicitly scoped injected-value comparison for the unpatched reference."""
import collections
import difflib
import hashlib
import json
import re
from pathlib import Path

UUID = re.compile(r'[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}')
# Only typed configuration slots rendered with Object.toString may lose an identity suffix.
CONFIG_IDENTITIES = {
    'analyticsConfiguration': ('AnalyticsConfiguration$DisabledAnalytics',),
    'pooling': ('strategy.PoolingStrategyConfiguration$OmniPoolingStrategyConfiguration',
                'strategy.PoolingStrategyConfiguration$ModelPoolingStrategyConfiguration'),
    'sharding': ('strategy.ShardingStrategyConfiguration$ParallelShardingStrategyConfiguration',),
    'sorting': ('strategy.SortingStrategyConfiguration$NoSortingStrategyConfiguration',
                'strategy.SortingStrategyConfiguration$RandomOrderStrategyConfiguration'),
    'batching': ('strategy.BatchingStrategyConfiguration$IsolateBatchingStrategyConfiguration',),
    'flakiness': ('strategy.FlakinessStrategyConfiguration$IgnoreFlakinessStrategyConfiguration',),
    'retry': ('strategy.RetryStrategyConfiguration$NoRetryStrategyConfiguration',),
    'vendorConfiguration': ('vendor.VendorConfiguration$StubVendorConfiguration',),
    'ciConfiguration': ('CIConfiguration$None',),
}



def files(root):
    return {str(p.relative_to(root)): p.read_bytes() for p in sorted(root.rglob('*')) if p.is_file()}


def report_json(name):
    return (name == 'test_result/raw.json' or
            (name.endswith('.json') and name.split('/')[0] in ('test_result', 'devices', 'bill')) or
            (name.startswith('allure-results/') and name.endswith('-result.json')))


def exact(a, b):
    aa, bb = files(a), files(b)
    errors = []
    for name in sorted(aa.keys() | bb.keys()):
        if aa.get(name) != bb.get(name):
            errors.append(name)
    if errors:
        raise AssertionError('byte mismatch: ' + ', '.join(errors))
    return {name: hashlib.sha256(data).hexdigest() for name, data in aa.items()}


def reference_snapshot(output, transcript=None, cli=False):
    """Core replay: preserve test times, assignments, ordering, counts, and every non-injected field."""
    blobs = files(output)
    ids = {}
    roles = {}
    if cli:
        raw = json.loads(blobs['test_result/raw.json'])
        runners = {r['deviceSerial'] for r in raw}
        assert len(runners) == 1, runners
        runner = next(iter(runners))
        serials = [json.loads(data)['serialNumber'] for name, data in blobs.items() if name.startswith('devices/')]
        for serial in serials:
            role = 'runner' if serial == runner else 'idle'
            roles[serial] = role
            roles[serial.replace(':', '-')] = role
        for name, data in blobs.items():
            if name.startswith('test_result/') and name != 'test_result/raw.json':
                batch = json.loads(data)['testBatchId']
                ids.setdefault(batch, 'batch-%04d' % (len(ids)+1))
    if transcript is not None:
        for event in transcript['log']:
            if 'batch' in event:
                ids.setdefault(event['batch'], 'batch-%04d' % (len(ids) + 1))
    allure = []
    for name, data in blobs.items():
        if name.startswith('allure-results/') and name.endswith('-result.json'):
            value = json.loads(data)
            uuid = value.pop('uuid')
            allure.append((json.dumps(value, sort_keys=True), uuid))
    for i, (_, uuid) in enumerate(sorted(allure), 1):
        ids[uuid] = 'allure-%04d' % i
    identities = {}
    def filename(name):
        parts = name.split('/')
        if name.startswith('allure-results/') and name.endswith('-result.json'):
            parts[-1] = ids.get(parts[-1][:-12], parts[-1][:-12])+'-result.json'
        # Only the device directory/basename, never a test/pool/user-selected filename.
        if cli:
            if parts[0] in ('devices', 'bill') and len(parts) == 3:
                parts[2] = roles.get(parts[2][:-5], parts[2][:-5])+'.json'
            elif parts[0] in ('test_result', 'logs', 'screenshot') and len(parts) >= 4:
                parts[2] = roles.get(parts[2], parts[2])
            elif parts[:2] == ['html', 'pools'] and len(parts) >= 5:
                parts[3] = roles.get(parts[3], parts[3])
        return '/'.join(parts)
    def typed_values(name, value, embedded=None):
        def role(row, field):
            if field in row: row[field] = roles.get(row[field], row[field])
        def device(row):
            role(row, 'serialNumber')
            delegate = row.get('safeSerialNumber$delegate', {})
            if isinstance(delegate.get('_value'), str): role(delegate, '_value')
        if name == 'test_result/raw.json':
            if cli:
                for row in value: role(row, 'deviceSerial')
        elif name.startswith('test_result/') and name.endswith('.json'):
            if 'testBatchId' in value: value['testBatchId'] = ids.get(value['testBatchId'], value['testBatchId'])
            if cli and isinstance(value.get('device'), dict): device(value['device'])
        elif name.startswith('allure-results/') and name.endswith('-result.json'):
            value['uuid'] = ids.get(value['uuid'], value['uuid'])
            if cli:
                for label in value.get('labels', []):
                    if label['name'] == 'host': role(label, 'value')
        elif cli and name.startswith('devices/') and name.endswith('.json'): device(value)
        elif cli and name.startswith('bill/') and name.endswith('.json') and isinstance(value.get('device'), dict): device(value['device'])
        if cli:
            def pool(row):
                for test in row['tests']: role(test, 'deviceId')
                for dev in row['devices']: role(dev, 'serial')
            if embedded == 'window.mainData =':
                for row in value['pools']: pool(row)
            elif embedded == 'window.pool =': pool(value)
            elif embedded == 'window.test =': role(value, 'deviceId')
            elif embedded == 'window.logs =': role(value, 'device_id')
            elif embedded == 'var dataset =':
                for row in value['measures']: role(row, 'measure')
        return value
    def identity(text):
        identities.setdefault(text, len(identities)+1)
        return text.rsplit('@', 1)[0]+'@<identity-%04d>' % identities[text]
    def report_identities(name, text):
        if name == 'allure-results/environment.xml':
            import xml.etree.ElementTree as ET
            root = ET.fromstring(text)
            for parameter in root.findall('parameter'):
                key, value = parameter.findtext('key'), parameter.find('value')
                if value is None or value.text is None: continue
                for cls in CONFIG_IDENTITIES.get(key, ()):
                    if re.fullmatch(re.escape('com.malinskiy.marathon.config.'+cls)+r'@[0-9a-f]{1,8}', value.text):
                        value.text = identity(value.text)
                if key == 'vendorConfiguration' and value.text.startswith('AndroidConfiguration('):
                    # The final typed parser slot, not arbitrary strings in Android's earlier args/maps.
                    prefix, separator, tail = value.text.rpartition(', testParserConfiguration=')
                    cls = 'com.malinskiy.marathon.config.vendor.android.TestParserConfiguration$LocalTestParserConfiguration'
                    tail = re.sub(r'^'+re.escape(cls)+r'@[0-9a-f]{1,8}(?=, testAccessConfiguration=TestAccessConfiguration\()',
                                  lambda m: identity(m[0]), tail)
                    value.text = prefix+separator+tail
            return ET.tostring(root, encoding='unicode')
        if cli and name == 'test_result/raw.json':
            rows = json.loads(text)
            # These typed array members are proven by the pinned CLI APK, not inferred from string shape.
            fixture_tests = {('com.example', 'ClassIgnoredTest', 'testAlwaysIgnored'),
                             ('com.example', 'MainActivityTest', 'testText'),
                             ('com.example', 'MainActivityAllureTest', 'testFailure')}
            for row in rows:
                if (row.get('package'), row.get('class'), row.get('method')) not in fixture_tests: continue
                for meta in row.get('metaProperties', []):
                    if meta['name'] != 'kotlin.Metadata': continue
                    for key in ('bv', 'd1', 'd2', 'mv'):
                        value = meta['values'].get(key)
                        if isinstance(value, str) and re.fullmatch(r'\[Lcom\.linkedin\.dex\.parser\.DecodedValue;@[0-9a-f]{1,8}', value):
                            meta['values'][key] = identity(value)
            return json.dumps(rows, separators=(',', ':'), ensure_ascii=False)
        return text
    returned_times = {}
    if not cli and 'test_result/raw.json' in blobs:
        for row in json.loads(blobs['test_result/raw.json']):
            if row['timestamp'] >= 1_000_000_000_000:
                assert row['status'] == 'INCOMPLETE' and row['duration'] == 1, row
                index = len(returned_times)//2+1
                returned_times[row['timestamp']] = f'<returned-{index}-start>'
                returned_times[row['timestamp']+1] = f'<returned-{index}-end>'
    out = {}
    rules = collections.Counter()
    for name, data in blobs.items():
        key = filename(name)
        assert key not in out, ('normalized filename collision', name, key)
        try:
            text = data.decode('utf-8')
        except UnicodeDecodeError:
            out[key] = data
            continue
        normalized = report_identities(name, text)
        if report_json(name):
            normalized = json.dumps(typed_values(name, json.loads(normalized)), separators=(',', ':'), ensure_ascii=False)
        elif name.startswith('html/') and name.endswith('.html'):
            def typed_embedded(match):
                value = typed_values(name, json.loads(match[2]), match[1].strip())
                return match[1]+json.dumps(value, separators=(',', ':'), ensure_ascii=False)
            normalized = re.sub(r'^([ \t]*(?:window\.(?:mainData|pool|test|logs)|var dataset) = )(\{[^\n]+\})$', typed_embedded, normalized, flags=re.M)
        if normalized != text:
            rules['typed report IDs / roles / identities and JSON encoding'] += 1
        if returned_times:
            if report_json(name):
                value = json.loads(normalized)
                returned_fields(name, value, returned_times)
                normalized = json.dumps(value, separators=(',', ':'), ensure_ascii=False)
            elif name.startswith('html/') and name.endswith('.html'):
                def returned_embedded(match):
                    value = json.loads(match[2])
                    returned_fields(name, value, returned_times, match[1].strip())
                    return match[1]+json.dumps(value, separators=(',', ':'), ensure_ascii=False)
                normalized = re.sub(r'^([ \t]*(?:window\.(?:mainData|pool|test|logs)|var dataset) = )(\{[^\n]+\})$', returned_embedded, normalized, flags=re.M)
            rules['QueueActor returned-batch timestamps (duration remains 1)'] += 1
        if name.startswith('bill/') and name.endswith('.json'):
            value = json.loads(normalized)
            for field in ('start', 'end', 'duration'):
                clock_fields(value, (field,), '<wall-clock>')
            normalized = json.dumps(value, separators=(',', ':'), ensure_ascii=False)
            rules['billing start/end/duration'] += 1
        if name.startswith('html/') and name.endswith('.html'):
            normalized, count = re.subn(r'^(    <div class="copy content">Generated with&nbsp;❤️&nbsp;&nbsp;by Juno at )[^<]+(</div>)$', r'\1<wall-clock>\2', normalized, flags=re.M)
            rules['HTML generation date'] += count
        if cli:
            if report_json(name):
                normalized = json.dumps(cli_times(name, json.loads(normalized)), separators=(',', ':'), ensure_ascii=False)
            elif name.startswith('html/') and name.endswith('.html'):
                def embedded(match):
                    return match[1]+json.dumps(cli_times(name, json.loads(match[2]), match[1].strip()), separators=(',', ':'), ensure_ascii=False)
                normalized = re.sub(r'^([ \t]*(?:window\.(?:mainData|pool|test|logs)|var dataset) = )(\{[^\n]+\})$', embedded, normalized, flags=re.M)
            elif name.endswith('.xml') and name.startswith('tests/'):
                import xml.etree.ElementTree as ET
                root = ET.fromstring(normalized)
                for element in root.iter():
                    for field in ('time', 'timestamp') if element.tag == 'testsuite' else ('time',) if element.tag == 'testcase' else ():
                        assert field in element.attrib, (name, element.tag, field)
                        if field == 'time': float(element.attrib[field])
                        else: assert re.fullmatch(r'\d{4}-\d\d-\d\dT.*', element.attrib[field])
                        element.set(field, '<clock-derived>')
                normalized = ET.tostring(root, encoding='unicode')
            rules['CLI clock fields and device roles'] += 1
        out[key] = normalized.encode()
    if transcript is not None:
        value = dict(transcript)
        value.pop('hooks'); value.pop('hooksReport')
        # A hooked synchronous entry delegates to the same runAsync core as the reference driver.
        value['log'] = [dict(e) for e in value['log'] if e != {'t': 0, 'runBlocking': 'core.Marathon.run'}]
        for event in value['log']:
            if 'batch' in event: event['batch'] = ids.get(event['batch'], event['batch'])
        out['@transcript'] = json.dumps(value, sort_keys=True).encode()
    return out, dict(rules)


def returned_fields(name, value, returned_times, embedded=None):
    def replace(row, fields):
        for field in fields:
            if field in row:
                assert type(row[field]) in (int, float), (name, field, row[field])
                if row[field] in returned_times: row[field] = returned_times[row[field]]
    if name == 'test_result/raw.json':
        for row in value: replace(row, ('timestamp',))
    elif name.startswith('test_result/') and name.endswith('.json'): replace(value, ('startTime', 'endTime'))
    elif name.startswith('allure-results/') and name.endswith('-result.json'): replace(value, ('start', 'stop'))
    elif embedded == 'var dataset =':
        for measure in value['measures']:
            for row in measure['data']: replace(row, ('startDate', 'endDate'))


def clock_fields(value, fields, replacement='<clock-derived>'):
    assert isinstance(value, dict), value
    for field in fields:
        assert field in value and type(value[field]) in (int, float), (field, value)
        value[field] = replacement


def cli_times(name, value, embedded=None):
    """Only report-schema paths may lose clock values; metadata is opaque user data."""
    if name == 'test_result/raw.json':
        assert isinstance(value, list)
        for row in value: clock_fields(row, ('timestamp', 'duration'))
    elif name.startswith('test_result/') and name.endswith('.json'):
        clock_fields(value, ('startTime', 'endTime'))
    elif name.startswith('allure-results/') and name.endswith('-result.json'):
        clock_fields(value, ('start', 'stop'))
    elif embedded == 'window.mainData =':
        clock_fields(value, ('total_duration_millis', 'average_duration_millis', 'max_duration_millis', 'min_duration_millis'))
        assert isinstance(value['pools'], list)
        for pool in value['pools']: html_pool(pool)
    elif embedded == 'window.pool =': html_pool(value)
    elif embedded == 'window.test =': clock_fields(value, ('duration_millis',))
    elif embedded == 'var dataset =':
        clock_fields(value['executionStats'], ('idleTimeMillis', 'averageTestExecutionTimeMillis'))
        assert isinstance(value['measures'], list)
        for measure in value['measures']:
            assert isinstance(measure['measure'], str)
            clock_fields(measure['stats'], ('idleTimeMillis', 'averageTestExecutionTimeMillis'))
            assert isinstance(measure['data'], list)
            for row in measure['data']:
                assert isinstance(row['metricType'], str) and isinstance(row['testName'], str)
                clock_fields(row, ('startDate', 'endDate'))
            measure['data'].sort(key=lambda row: (row['metricType'], row['testName']))
        value['measures'].sort(key=lambda row: row['measure'])
    return value


def html_pool(pool):
    clock_fields(pool, ('duration_millis',))
    assert isinstance(pool['tests'], list) and isinstance(pool['devices'], list)
    for row in pool['tests']: clock_fields(row, ('duration_millis',))
    for row in pool['devices']: assert isinstance(row['serial'], str) and isinstance(row['modelName'], str)
    pool['devices'].sort(key=lambda row: row['serial'])


def compare_reference(a, b, destination, cli=False):
    aa, ar = reference_snapshot(a/'output', None if cli else json.loads((a/'transcript.json').read_text()), cli)
    bb, br = reference_snapshot(b/'output', None if cli else json.loads((b/'transcript.json').read_text()), cli)
    errors = []
    for name in sorted(aa.keys() | bb.keys()):
        if aa.get(name) != bb.get(name):
            errors.append(name)
            left, right = aa.get(name, b'<missing>'), bb.get(name, b'<missing>')
            destination.mkdir(parents=True, exist_ok=True)
            (destination/(name.replace('/', '_')+'.diff')).write_text(''.join(difflib.unified_diff(
                left.decode(errors='replace').splitlines(True), right.decode(errors='replace').splitlines(True),
                fromfile='patched', tofile='reference')))
    if errors:
        raise AssertionError('reference mismatch: ' + ', '.join(errors))
    return {'patched_normalizations': ar, 'reference_normalizations': br, 'files': len(aa)}


def console_snapshot(root, cli=False):
    """No-tape console equivalence keeps messages/counts, ignoring instrumented JVM frames and live ordering."""
    output = root/'output'
    text = (root/'stdout').read_text()
    ids = {}
    transcript = root/'run/transcript.json'
    if transcript.exists():
        output = root/'run/output'
        for event in json.loads(transcript.read_text())['log']:
            if 'batch' in event: ids.setdefault(event['batch'], 'batch-%04d' % (len(ids)+1))
    roles = {}
    if cli and not transcript.exists():
        for path in sorted(output.glob('test_result/*/*/*.json')):
            batch=json.loads(path.read_text())['testBatchId']; ids.setdefault(batch,'batch-%04d' % (len(ids)+1))
        raw=json.loads((output/'test_result/raw.json').read_text())
        runner=raw[0]['deviceSerial']
        for path in output.glob('devices/*/*.json'):
            serial=json.loads(path.read_text())['serialNumber']; role='runner' if serial==runner else 'idle'
            roles[serial] = role
    rows=[]
    ordered=[]
    for line in text.splitlines():
        if re.fullmatch(r'\s+at [\w.$/]+\([^()]*\)', line) or re.fullmatch(r'\s*\.\.\. \d+ common frames omitted', line):
            # Only JVM frame syntax, never a message that merely begins with "at ".
            continue
        # Generated log envelopes own a clock; clocks embedded in messages are user data.
        line = re.sub(r'^(?:([DIWE] )?)(\d{2}:\d{2}:\d{2}\.\d{3})(?= \[)',
                      lambda m: (m[1] or '')+'<log-clock>', line)
        match = re.fullmatch(r'(<log-clock> \[main\] INFO  com\.malinskiy\.marathon\.Marathon - java\.class\.path: )(.*)', line)
        if match:
            jars=sorted(Path(p).name for p in match[2].split(':') if not p.endswith('/oracle-hooks.jar'))
            line=match[1]+','.join(jars)
        # The UUID is a typed batch reference in this specific actor message.
        line = re.sub(r'^(<log-clock> \[main\] DEBUG DevicePool\[[^]\n]+\]_DeviceActor\[[^]\n]+\] - Returning batch )('+UUID.pattern+r')(\. Reason:)',
                      lambda m: m[1]+ids.get(m[2], m[2])+m[3], line)
        for serial, role in roles.items():
            line = re.sub(r'^(\d{1,3}% \| \[[^]\n]+\]-\[)'+re.escape(serial)+r'(\] )',
                          lambda m: m[1]+role+m[2], line)
        timing = re.fullmatch(r'((?:[DIWE] )?<log-clock> \[[^]\n]+\] (?:INFO  Timings - |<Timings> ))(.*)', line)
        if timing and re.fullmatch(r'(?:Device provider initialized in \d+ ms|Parsed \d+ tests in \d+ ms|Prepared the shard of \d+ tests and the analytics history it uses in \d+ ms|Scheduling \d+ tests) \(\+\d+ ms since start\)', timing[2]):
            line = timing[1]+re.sub(r'\d+ ms', '<duration> ms', timing[2])
        line = re.sub(r'^(<log-clock> \[main\] DEBUG com\.malinskiy\.marathon\.Marathon - Parsing took )\d+( ms)$',
                      r'\1<duration>\2', line)
        if re.fullmatch(r'Total time: (?:\d+H \d+m \d+s|\d+s)', line): line='Total time: <duration>'
        # Live logger records may interleave; plain output and exception continuations retain their order.
        if re.match(r'^(?:[DIWE] )?<log-clock> \[', line): rows.append(line)
        else: ordered.append(line)
    return (collections.Counter(rows), ordered), (root/'stderr').read_bytes()


def compare_console(a, b, destination, cli=False):
    aa, ae=console_snapshot(a,cli); bb, be=console_snapshot(b,cli)
    if aa != bb or ae != be:
        destination.mkdir(parents=True,exist_ok=True)
        (destination/'console.diff').write_text(json.dumps({'patched_only':list((aa[0]-bb[0]).elements()),
            'reference_only':list((bb[0]-aa[0]).elements()),'patched_ordered':aa[1],
            'reference_ordered':bb[1],'stderr_equal':ae==be},indent=2)+'\n')
        raise AssertionError('reference console mismatch: '+str(destination/'console.diff'))


def adb_snapshot(path, roles=None):
    """Keep per-device request/sync-operation order and every record within each connection.

    Only socket ids and interleaving between independent devices/completion records disappear.
    Payload sizes and SHA-256 digests are exact; reject unobserved stdin/sync transfers.
    """
    connections = {}
    global_events = []
    requests = []
    for line in path.read_text().splitlines():
        row = json.loads(line)
        conn = row.pop('conn', None)
        assert row.get('type') != 'warning', row
        if row.get('stdin_bytes'):
            assert re.fullmatch(r'[0-9a-f]{64}', row.get('stdin_sha256', '')), 'payload bytes must be captured before comparing streamed installs'
        if row.get('op') in ('SEND', 'SND2'):
            assert re.fullmatch(r'[0-9a-f]{64}', row.get('payload_sha256', '')), 'payload bytes must be captured before comparing sync writes'
        if conn is None: global_events.append(row)
        else:
            connections.setdefault(conn, []).append(row)
            if row.get('type') == 'request' or (row.get('type') == 'sync' and row.get('op') != 'QUIT'):
                requests.append((conn, row))
    devices = collections.defaultdict(list)
    connection_devices = {}
    for conn, rows in connections.items():
        serials = {r['serial'] for r in rows if r.get('serial') is not None}
        for r in rows:
            service = r.get('service', '')
            if service.startswith('host:transport:'): serials.add(service[len('host:transport:'):])
            if service.startswith('host-serial:'): serials.add(service[len('host-serial:'):].rsplit(':', 1)[0])
        assert len(serials) <= 1, rows
        serial = next(iter(serials), '@host')
        if roles:
            # Role mapping is restricted to transport selectors and the serial field, never shell arguments/payload.
            for r in rows:
                if r.get('serial') in roles: r['serial'] = roles[r['serial']]
                if r.get('service') == 'host:transport:'+serial: r['service'] = 'host:transport:'+roles.get(serial, serial)
                if r.get('service', '').startswith('host-serial:'+serial+':'):
                    r['service'] = 'host-serial:'+roles.get(serial, serial)+':'+r['service'].rsplit(':', 1)[1]
        device = roles.get(serial, serial) if roles else serial
        connection_devices[conn] = device
        devices[device].append(rows)
    # Socket setup order and request/sync order are separate: connections can be opened in advance.
    request_order = collections.defaultdict(list)
    for conn, row in requests:
        device = connection_devices[conn]
        request_order[device].append(row)
    return {'devices': dict(devices), 'requests': dict(request_order), 'events': global_events}


def compare_adb(a, b, role_mapping=False):
    def roles(root):
        runner = json.loads((root/'output/test_result/raw.json').read_text())[0]['deviceSerial'].rsplit(':', 1)[1]
        serials = [json.loads(p.read_text())['serialNumber'].rsplit(':', 1)[1] for p in (root/'output/devices').rglob('*.json')]
        return {s: 'runner' if s == runner else 'idle' for s in serials}
    aa = adb_snapshot(a/'adb.jsonl', roles(a) if role_mapping else None)
    bb = adb_snapshot(b/'adb.jsonl', roles(b) if role_mapping else None)
    if aa != bb:
        (b/'adb-logical.diff').write_text(''.join(difflib.unified_diff(
            json.dumps(aa, indent=2, sort_keys=True).splitlines(True),
            json.dumps(bb, indent=2, sort_keys=True).splitlines(True))))
        raise AssertionError('logical ADB mismatch: '+str(a)+' vs '+str(b))
