"""Typed vendor report locations; leave all surrounding product bytes untouched."""
import html
import json
import re
import evidence as e


def json_tokens(data, identities, family):
    text, spans = e.json_spans(data)
    edits = []
    for pointer, (start, end, value) in spans.items():
        if not isinstance(value, str):
            continue
        field = pointer.rsplit('/', 1)[-1]
        replacement = value
        # These are schema-owned device/path fields, never test names, messages,
        # metadata, statuses or arbitrary string values.
        selected = (family == 'report' and re.fullmatch(r'/(?:device/)?serialNumber|/\d+/deviceSerial',pointer)) or (
            family == 'html' and re.fullmatch(r'/(?:deviceId|device_id)|/(?:pools/\d+/)?(?:tests/\d+/deviceId|devices/\d+/serial)|/measures/\d+/measure',pointer))
        if selected:
            for role in [key for key in identities if key.endswith('serial')]:
                if value == identities[role]:
                    replacement = '{' + role + '}'
        elif family == 'allure' and re.fullmatch(r'/labels/\d+/value', pointer):
            parent = pointer.rsplit('/',1)[0]
            if spans.get(parent + '/name', (None,None,None))[2] == 'host':
                roles = [key for key in identities if key.endswith('serial') and identities[key] == value]
                e.require(len(roles) == 1,'unexpected Allure host')
                replacement = '{' + roles[0] + '}'
        elif re.fullmatch(r'/attachments/\d+/(?:file|source)', pointer):
            prefix = identities['sandbox'] + '/in/report/'
            e.require(value.startswith(prefix), 'attachment escaped report tree')
            replacement = '{sandbox}/in/report/' + value[len(prefix):].replace(identities['safe_serial'], '{safe_serial}')
        elif family == 'html' and (field in ('log_path','log_file','screenshot') or re.fullmatch(r'/videos/\d+',pointer)):
            replacement = value.replace('/' + identities['safe_serial'] + '/', '/{safe_serial}/')
        if replacement != value:
            edits.append((start, end, json.dumps(replacement,ensure_ascii=False)))
    for start,end,replacement in reversed(sorted(edits)):
        text = text[:start] + replacement + text[end:]
    return text.encode()


def omit_delegate(data, pointer, implementation):
    """D3 only: remove this member at a declared DeviceInfo object location."""
    text = data.decode()
    # The DeviceInfo member is emitted once in each selected file, with this
    # exact key. Balanced raw_decode finds the value without rewriting siblings.
    marker = '"safeSerialNumber$delegate":'
    _,spans = e.json_spans(data,containers=True)
    e.require(pointer in spans,'missing DeviceInfo object')
    obj_start,obj_end,obj = spans[pointer]
    hits = list(re.finditer(re.escape(marker), text[obj_start:obj_end]))
    if implementation == 'native':
        e.require('safeSerialNumber$delegate' not in obj,'native D3 delegate unexpectedly present')
        return data
    e.require('safeSerialNumber$delegate' in obj and len(hits) == 1,'missing/ambiguous JVM DeviceInfo delegate')
    hit_start = obj_start + hits[0].start()
    hit_end = obj_start + hits[0].end()
    _, end = json.JSONDecoder().raw_decode(text, hit_end)
    start = hit_start
    if start and text[start-1] == ',':
        start -= 1
    elif end < len(text) and text[end] == ',':
        end += 1
    return (text[:start] + text[end:]).encode()


def environment(data, identities, implementation, expected):
    text = data.decode()
    allowed_objects = {'analyticsConfiguration','pooling','sharding','sorting','flakiness','retry','executionStrategy','batching','ciConfiguration'}
    def object_identity(value):
        cls,suffix = value.rsplit('@',1)
        if implementation == 'jvm':
            e.require(value in expected,'taped configuration identity differs')
        else:
            code = 0
            for i in range(0,len(cls.encode('utf-16-be')),2):
                code = (31*code + int.from_bytes(cls.encode('utf-16-be')[i:i+2],'big')) & 0xffffffff
            e.require(suffix == format(code,'x'),'native configuration identity violates D12/F29')
        return cls+'@{class-identity}'
    def parameter(match):
        key, value = match[1], html.unescape(match[2])
        if key == 'outputDir':
            e.require(value == identities['sandbox'] + '/in/report', 'unexpected outputDir')
            value = '{sandbox}/in/report'
        elif key == 'vendorConfiguration':
            # File-valued constructor slots only. This exact config text is
            # otherwise compared, including nulls, booleans, order and quoting.
            value = value.replace(identities['sandbox'] + '/in/', '{sandbox}/in/')
            value = value.replace(identities['sandbox'] + '/client-key', '{sandbox}/client-key')
            value = value.replace(identities['sandbox'] + '/known_hosts', '{sandbox}/known_hosts')
            value = value.replace('port=' + identities['port'] + ')', 'port={port})')
            value = value.replace('port=' + identities['port'] + ',', 'port={port},')
            for cls in ['com.malinskiy.marathon.config.vendor.android.TestParserConfiguration$LocalTestParserConfiguration',
                        'com.malinskiy.marathon.config.vendor.apple.TestParserConfiguration$XCTestParserConfiguration']:
                value = re.sub(r'(?<=testParserConfiguration=)' + re.escape(cls) + r'@[0-9a-f]+(?=, )', lambda m:object_identity(m[0]), value)
        elif key in allowed_objects and value.startswith('com.malinskiy.marathon.config.'):
            e.require(re.fullmatch(r'com\.malinskiy\.marathon\.config\.[A-Za-z0-9_.$]+@[0-9a-f]+',value), 'unknown configuration object')
            value = object_identity(value)
        return '<parameter><key>' + key + '</key><value>' + html.escape(value, quote=False) + '</value></parameter>'
    text = re.sub(r'<parameter><key>([^<]+)</key><value>(.*?)</value></parameter>',parameter,text,flags=re.S)
    return text.encode()


def normalize(path, data, identities, implementation, contract):
    relative = path.removeprefix('reports/')
    if relative == 'allure-results/environment.xml':
        return environment(data, identities, implementation, contract['jvm_config_identities'])
    if relative.startswith('devices/') and relative.endswith('.json'):
        data = omit_delegate(data, '', implementation)
    elif (relative.startswith('bill/') or (relative.startswith('test_result/') and relative != 'test_result/raw.json')) and relative.endswith('.json'):
        data = omit_delegate(data, '/device', implementation)
    if relative.endswith('.json') and relative.split('/')[0] in ('devices','bill','test_result','allure-results'):
        return json_tokens(data,identities,'allure' if relative.startswith('allure-results/') else 'report')
    if relative.startswith('html/') and relative.endswith('.html'):
        text = data.decode()
        pattern = re.compile(r'((?:window\.(?:mainData|pool|test|logs)|var dataset) = )(.*?)(;?\s*)$',re.M)
        def embedded(match):
            body = match[2].removesuffix(';')
            suffix = ';' if match[2].endswith(';') else ''
            return match[1] + json_tokens(body.encode(),identities,'html').decode() + suffix + match[3]
        return pattern.sub(embedded,text).encode()
    return data
