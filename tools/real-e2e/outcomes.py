"""Outcome projection over 7.1 evidence; rules are qualified by two JVM captures."""
import copy
from pathlib import Path
import re
import subprocess
import tempfile
from common import HERE, ROOT, SCENARIOS, build_source, capture_source, e, require, save, source_kind
from projection import (LOG_KINDS, MEDIA_SUFFIXES, UUID, attachment_owners, empty_recording, parse_jacoco, semantic_projection,
                        waived_tests)

FORMAT = 'marathon-real-e2e/outcomes@2'
RAW = 'reports/test_result/raw.json'


def legacy_digests():
    """Evidence digests of the retained WP 7.2 captures: the only ones that may lack the WP 7.5 source fields."""
    value = e.load(HERE / 'legacy-captures.json')
    require(value.get('format') == 'marathon-real-e2e/legacy-captures@1' and value.get('count') == len(value['evidence_sha256']),
            'invalid legacy capture list')
    return frozenset(value['evidence_sha256'])


def retained_build(raw, capture):
    """The build.json the capture ran with, proven to be the one its hashed input inventory lists."""
    path = raw / 'in/build.json'
    require(path.is_file() and not path.is_symlink(), 'the capture retains no build record')
    rows = [r for r in capture['input_tree'] if isinstance(r, dict) and r.get('path') == 'tree/build.json']
    require(len(rows) == 1 and rows[0].get('kind') == 'file' and rows[0].get('sha256') == e.sha(path.read_bytes()),
            'the retained build record differs from the input inventory')
    return e.load(path)


def load_capture(pair, side):
    pair = Path(pair).resolve()
    raw = pair / side
    contract = e.load(pair / 'contract.json')
    record = e.validate(raw / 'evidence', contract)
    capture = e.load(raw / 'capture.json')
    require(record['implementation'] == side, 'capture implementation differs')
    # WP 7.5: capture, contract, evidence, retained build record and JVM reference must name one Kotlin source.
    source = capture_source(capture)
    require(record['source'] == source and contract.get('source', e.SOURCE) == source,
            'capture, contract and evidence name different Kotlin sources')
    build = retained_build(raw, capture)
    require(build_source(build) == source, 'the retained build record is bound to another Kotlin source than the capture')
    if 'source' in capture:
        # Captures made since WP 7.5, frozen or release, always carry and prove the whole binding.
        require('source_kind' in build and capture.get('source_kind') == source_kind(source)
                and capture.get('jvm_reference_source') == source, 'JVM reference was not bound to the capture source')
    else:
        # Field absence proves nothing (a fresh capture can drop its fields and refresh every digest it carries): only an
        # audited retained 7.2 capture, named by its evidence digest, may be read without the binding.
        require(e.sha((raw / 'evidence/evidence.json').read_bytes()) in legacy_digests(),
                'a capture without a source must be one of the retained 7.2 captures')
        require(source == e.SOURCE and 'source_kind' not in build and 'source_kind' not in capture
                and 'jvm_reference_source' not in capture, 'a capture without a source must be a complete 7.2 capture')
    require(record['provenance']['capture_sha256'] == e.sha((raw / 'capture.json').read_bytes()), 'capture digest differs')
    require(capture['completed'] and capture['cleanup']['complete'], 'incomplete capture/cleanup')
    process = capture['process']
    require(process['exit'] == record['outcome']['cli'] and not process['watchdog']
            and process['joined'] and not process['interrupted'], 'inconsistent process outcome')
    files = {}
    for row in record['entries']:
        item = {k: v for k, v in row.items() if k not in ('path', 'sha256', 'size')}
        if row['kind'] == 'file':
            item['data'] = e.payload(raw / 'evidence', row, record)
        files[row['path']] = item
    return dict(pair=pair, side=side, capture=capture, record=record, files=files,
                digest=e.sha((raw / 'evidence/evidence.json').read_bytes()))


def same_inputs(a, b):
    require(capture_source(a['capture']) == capture_source(b['capture']), 'capture context differs: Kotlin source')
    for field in ('scenario', 'profile', 'worker_id', 'input_tree', 'selected_tests'):
        require(a['capture'][field] == b['capture'][field], 'capture context differs: ' + field)
    for field in ('xcode', 'sdk', 'image', 'image_hashes', 'adb', 'emulator', 'os'):
        require(a['capture']['worker_before']['before'].get(field) == b['capture']['worker_before']['before'].get(field),
                'capture toolchain differs: ' + field)


def summary(data):
    # StdoutReporter.kt and ApplicationView.kt: diagnostic logger lines are not
    # part of decision 7's console surface. Keep the complete summary block.
    lines = data.decode().splitlines(keepends=True)
    result = []
    inside = False
    for line in lines:
        if line.rstrip('\r\n') == 'Marathon run finished:':
            require(not inside, 'nested console summary')
            inside = True
        if inside:
            result.append(line)
            if line.startswith('Total time: '):
                inside = False
        elif line.startswith('Marathon execution '):
            result.append(line)
    require(not inside, 'truncated console summary')
    return ''.join(result).encode()


def command(argv):
    result = subprocess.run(argv, capture_output=True, timeout=180)
    require(result.returncode == 0, 'artifact inspection failed: ' + str(argv[0]) + ': ' + result.stderr.decode(errors='replace')[:300])
    return result.stdout


def inspect_artifact(files, rule):
    """Read only retained bytes, never a worker/device; temporary tree is ours."""
    path = rule['path']
    require(path in files, 'missing artifact: ' + path)
    with tempfile.TemporaryDirectory(prefix='mrs-outcome-') as directory:
        root = Path(directory)
        if rule['kind'] == 'media':
            require(files[path]['kind'] == 'file', 'media is not a file')
            media = root / Path(path).name
            media.write_bytes(files[path]['data'])
            info = e.loads(command(['ffprobe', '-v', 'error', '-count_frames', '-show_streams', '-show_format', '-of', 'json', str(media)]))
            command(['ffmpeg', '-v', 'error', '-xerror', '-i', str(media), '-f', 'null', '-'])
            video = [s for s in info['streams'] if s.get('codec_type') == 'video']
            require(len(video) == 1, 'missing/ambiguous video stream')
            stream = video[0]
            facts = {k: stream[k] for k in ('codec_name', 'width', 'height')}
            duration = float(info['format']['duration'])
            require(facts == rule.get('expected', facts) and facts['width'] > 0 and facts['height'] > 0, 'media codec/dimensions differ')
            low, high = rule.get('duration_seconds', [1e-9, float('inf')])
            require(0 < low <= duration <= high, 'media duration outside JVM-reviewed bounds')
            frames = int(stream['nb_read_frames'])
            low_frames, high_frames = rule.get('frame_count', [1, float('inf')])
            require(0 < low_frames <= frames <= high_frames, 'media frame count outside JVM-reviewed bounds')
            if path.endswith('.gif') and not rule.get('exact'):
                import gif_frames
                gif = gif_frames.decode(files[path]['data'])
                require(gif['loop'] == rule['gif_loop'] and gif['extensions'] == rule['gif_extensions'], 'GIF loop/extensions differ')
                require(all(f['disposal'] in rule['gif_disposals'] for f in gif['frames']), 'GIF disposal differs')
                low_delay, high_delay = rule['gif_delay_centiseconds']
                require(all(low_delay <= f['delay'] <= high_delay for f in gif['frames']), 'GIF frame timing differs')
            # Landmarks are a human P3 witness, bound to these exact raw bytes.
            return e.encoded(facts), [e.sha(files[path]['data']), duration, frames]
        require(rule['kind'] == 'xcresult' and files[path]['kind'] == 'directory', 'xcresult is not a directory')
        members = {p: item for p, item in files.items() if p.startswith(path + '/')}
        require(members, 'empty xcresult')
        bundle = root / 'result.xcresult'; bundle.mkdir()
        for name, item in sorted(members.items()):
            dest = bundle / e.relative(name[len(path) + 1:])
            require(item['kind'] in ('directory', 'file'), 'linked xcresult member')
            if item['kind'] == 'directory':
                dest.mkdir(parents=True, exist_ok=True)
            else:
                dest.parent.mkdir(parents=True, exist_ok=True); dest.write_bytes(item['data'])
        data = command(['xcrun', 'xcresulttool', 'get', '--legacy', '--format', 'json', '--path', str(bundle)])
        root_data = e.loads(data)
        tests = []
        for action in root_data.get('actions', {}).get('_values', []):
            ref = action.get('actionResult', {}).get('testsRef')
            if ref:
                tests.append(e.loads(command(['xcrun', 'xcresulttool', 'get', '--legacy', '--format', 'json',
                                              '--path', str(bundle), '--id', ref['id']['_value']])))
        return e.encoded(dict(root=root_data, tests=tests)), [e.sha(e.encoded([(p, e.sha(i['data']) if i['kind'] == 'file' else i['kind']) for p, i in sorted(members.items())]))]


def project(capture, policy):
    """One extraction path for JVM qualification and native release comparison."""
    return semantic_projection(capture, policy, inspect_artifact, summary)


def differences(a, b, path=''):
    if type(a) is not type(b):
        return [dict(field=path, expected=a, actual=b)]
    if isinstance(a, dict):
        result = []
        for key in sorted(set(a) | set(b)):
            child = path + '/' + key.replace('~', '~0').replace('/', '~1')
            if key not in a or key not in b:
                result.append(dict(field=child, expected=a.get(key), actual=b.get(key)))
            else:
                result.extend(differences(a[key], b[key], child))
        return result
    if isinstance(a, list) and len(a) == len(b):
        return [d for i, (left, right) in enumerate(zip(a, b))
                for d in differences(left, right, path + '/' + str(i))]
    return [] if a == b else [dict(field=path, expected=a, actual=b)]


def compare_projected(a, b, policy, mismatch, final=True):
    # Raw manifests include hashes, modes, links and every original filename.
    # They are informational even when extraction itself fails.
    raw_a = {r['path']: r for r in a['record']['entries']}
    raw_b = {r['path']: r for r in b['record']['entries']}
    report = dict(format=FORMAT, raw_differences_informational=differences(raw_a, raw_b),
                  evidence=dict(expected=a['digest'], actual=b['digest']))
    try:
        left, av = project(a, policy)
        right, bv = project(b, policy)
        left_gating, right_gating, log_presence = split_log_presence(left, right)
        gating, informational = split_androidx(left_gating, right_gating)
        report.update(expected=left, actual=right, differences=gating)
        if informational:
            report['coverage_androidx_presence_informational'] = informational
        if log_presence:
            report['log_attachment_presence_informational'] = log_presence
        require(not report['differences'], 'semantic projection differences: ' +
                ', '.join(d['field'] for d in report['differences']))
        # Decision 16: a caller with further checks (release) finalises the status itself, so a report never
        # says "passed" for a pair those checks rejected.
        report['status'] = 'passed' if final else 'pending'
        return av, bv
    except (e.Invalid, ValueError, KeyError, TypeError, OSError, AttributeError, SyntaxError, IndexError, subprocess.TimeoutExpired) as error:
        report.update(status='failed', error=str(error))
        raise e.Invalid(str(error)) from error
    finally:
        if mismatch is not None:
            destination = Path(mismatch)
            destination.mkdir(parents=True, exist_ok=False)
            save(destination / 'comparison.json', report)


def split_log_presence(left, right):
    """Decision 7: the presence of a near-instant test's log is informational on the side that lacks it.

    A test that recorded no log and ran for under 10 ms on a side (one raw attempt, one Allure result, one detail row,
    0 <= endTime - startTime < 10) has
    its log facts (the LOG attachment, the Allure `text/plain` attachment, and the matching per-test and global kinds)
    set aside on both sides. Returns the two projections without them and the presence differences, which comparison.json
    lists. A log that is present on both sides is untouched, and every other test, field and duration still gates.
    The HTML log link is not projected at all, so it never gated.
    """
    sides = []
    for projected in (left, right):
        projected = copy.deepcopy(projected)
        attempts, executions = projected.pop('log_attempts'), projected.pop('log_executions')
        sides.append((projected, attempts, set(waived_tests(attempts, projected['per_test_kinds'], executions))))
    (a, attempts_a, waived_a), (b, attempts_b, waived_b) = sides
    informational = []
    for name in sorted(waived_a | waived_b):
        kinds_a, kinds_b = a['per_test_kinds'].get(name), b['per_test_kinds'].get(name)
        if kinds_a is None or kinds_b is None:
            continue   # a test missing on one side is a gating difference of its own
        logs_a, logs_b = [k for k in LOG_KINDS if k in kinds_a], [k for k in LOG_KINDS if k in kinds_b]
        if any(logs not in ([], list(LOG_KINDS)) for logs in (logs_a, logs_b)):
            continue   # a side whose own reports disagree about the log (one kind without the other) still gates
        for side in (a, b):
            side['per_test_kinds'][name] = [k for k in side['per_test_kinds'][name] if k not in LOG_KINDS]
            for row in side['allure']:
                if row['test'] == name:
                    row['attachments'].pop('text/plain', None)
        if logs_a != logs_b:
            informational.append(dict(test=name, expected=dict(log_kinds=logs_a, attempts=attempts_a[name]),
                                      actual=dict(log_kinds=logs_b, attempts=attempts_b[name]),
                                      reason='decision 7: a test under 10 ms records a log only if a device line fell in its window'))
    if informational or waived_a or waived_b:
        for side in (a, b):
            held = {k for kinds in side['per_test_kinds'].values() for k in kinds}
            side['kinds'] = sorted((set(side['kinds']) - set(LOG_KINDS)) | (held & set(LOG_KINDS)))
    return a, b, informational


def split_androidx(left, right):
    """Decision 17: `androidx/` coverage classes gate only on probe length, and only where both sides have the class.

    Returns the gating differences (the projections with those lists set aside, plus any probe-length change on a
    shared class) and the informational presence differences per coverage file.
    """
    sides = []
    for projected in (left, right):
        projected = copy.deepcopy(projected)
        sides.append((projected, [fact.pop('androidx', None) for fact in projected['file_sync']]))
    (a, lists_a), (b, lists_b) = sides
    result = differences(a, b)
    informational = []
    for i, (la, lb) in enumerate(zip(lists_a, lists_b)):
        if la is None or lb is None:
            continue
        ca, cb = {}, {}
        for table, rows in ((ca, la), (cb, lb)):
            for name, count in rows:
                table.setdefault(name, []).append(count)  # every probe length of a class, duplicates included
        for name in sorted(ca.keys() & cb.keys()):
            if sorted(ca[name]) != sorted(cb[name]):
                result.append(dict(field='/file_sync/%d/androidx/%s' % (i, name), expected=sorted(ca[name]), actual=sorted(cb[name])))
        only_a, only_b = sorted(ca.keys() - cb.keys()), sorted(cb.keys() - ca.keys())
        if only_a or only_b:
            informational.append(dict(role=left['file_sync'][i]['role'], only_expected=only_a, only_actual=only_b))
    return result, informational


def policy_shape(policy):
    require(policy['format'] == FORMAT and policy.get('review') and not policy['review'].startswith('SET_'),
            'reviewed decision-9 outcome policy required')
    require(set(policy) <= {'format', 'review', 'rules', 'deviations', 'jvm_runs', 'projection_sha256'},
            'unknown policy option; scenario expectations belong in scenarios.json')
    plan = (ROOT / 'PLAN.md').read_text()
    for deviation in policy.get('deviations', []):
        require(set(deviation) == {'number', 'reason'} and deviation['reason'], 'invalid deviation annotation')
        number = deviation['number']
        require(re.fullmatch(r'D(?:[1-9]|1[0-6])', number) and
                re.search(r'^\| ' + number + r' \|', plan, re.M), 'deviation has no PLAN §3.11 row')
    ids = set()
    for rule in policy['rules']:
        require(re.fullmatch(r'[a-z][a-z0-9-]*', rule['id']) and rule['id'] not in ids, 'invalid/duplicate rule id')
        ids.add(rule['id'])
        require(rule['kind'] == 'media' and rule.get('reason'), 'only reviewed P3 media rules supplement decision 9')
        require(all(k in rule for k in ('path', 'expected', 'duration_seconds', 'frame_count')), 'incomplete P3 bounds')
        require(set(rule) <= {'id', 'kind', 'path', 'reason', 'expected', 'duration_seconds', 'frame_count',
                             'gif_loop', 'gif_extensions', 'gif_disposals', 'gif_delay_centiseconds', 'evidence'},
                'unknown P3 option')


def projection_digest(capture, policy):
    return e.sha(e.encoded(dict(
        implementation={name: e.sha((ROOT / 'tools/real-e2e' / name).read_bytes())
                        for name in ('outcomes.py', 'projection.py')},
        scenario=SCENARIOS[capture['capture']['scenario']],
        rules=[{k: v for k, v in r.items() if k != 'evidence'} for r in policy['rules']],
        deviations=policy.get('deviations', []))))


def qualify(reference, rerun, draft, output, mismatch):
    policy = e.load(draft); policy_shape(policy)
    require(not Path(output).exists(), 'policy output exists')
    a, b = load_capture(reference, 'jvm'), load_capture(rerun, 'jvm')
    require(a['digest'] != b['digest'] and a['capture']['run_id'] != b['capture']['run_id'], 'two independent JVM runs required')
    same_inputs(a, b)
    for field in ('jars', 'patches', 'jdk'):
        require(a['record']['provenance'][field] == b['record']['provenance'][field], 'JVM provenance changed: ' + field)
    av, bv = compare_projected(a, b, policy, mismatch)
    for rule in policy['rules']:
        rule['evidence'] = dict(jvm_a=a['digest'], jvm_b=b['digest'],
                                values_a=av[rule['id']], values_b=bv[rule['id']])
    policy['jvm_runs'] = [dict(pair=str(c['pair']), evidence_sha256=c['digest']) for c in (a, b)]
    policy['projection_sha256'] = projection_digest(a, policy)
    save(output, policy)


def release(pair, policy_path, mismatch):
    policy = e.load(policy_path); policy_shape(policy)
    witnesses = [load_capture(r['pair'], 'jvm') for r in policy['jvm_runs']]
    require(len(witnesses) == 2 and witnesses[0]['capture']['run_id'] != witnesses[1]['capture']['run_id'], 'two JVM witnesses required')
    for witness, ref in zip(witnesses, policy['jvm_runs']):
        require(witness['digest'] == ref['evidence_sha256'], 'JVM witness digest differs')
    a, b = load_capture(pair, 'jvm'), load_capture(pair, 'native')
    require(policy['projection_sha256'] == projection_digest(a, policy), 'projection changed since qualification')
    same_inputs(a, b)
    for witness in witnesses:
        same_inputs(a, witness)
        for field in ('jars', 'patches', 'jdk'):
            require(a['record']['provenance'][field] == witness['record']['provenance'][field], 'JVM provenance changed: ' + field)
    av, bv = compare_projected(*witnesses, policy, None)
    for rule in policy['rules']:
        expected = dict(jvm_a=witnesses[0]['digest'], jvm_b=witnesses[1]['digest'],
                        values_a=av[rule['id']], values_b=bv[rule['id']])
        require(rule.get('evidence') == expected, 'unjustified media rule: ' + rule['id'])
    compare_projected(a, b, policy, mismatch, final=False)
    try:
        reviews = [review_witnesses(capture, policy) for capture in (a, b)]
        require(reviews[0][0] == reviews[1][0], 'ordered media landmarks differ')
    except Exception as error:
        finish_report(mismatch, status='failed', error=str(error), witness_error=type(error).__name__)
        raise
    # Decision 15: GIF landmark order measures sampling timing, so it is reported but never gates.
    # Coverage hits (decision 16) follow test timing too and are reported the same way.
    informational = dict(
        gif_landmarks_informational=dict(expected=reviews[0][1], actual=reviews[1][1],
                                         differences=differences(reviews[0][1], reviews[1][1])),
        coverage_hits_informational=coverage_hit_report(coverage_hits(a), coverage_hits(b)))
    finish_report(mismatch, status='passed', **informational)
    return dict(format=FORMAT, status='passed', scope='decision-9 semantic pair; matrix coverage remains separate',
                policy_sha256=e.sha(Path(policy_path).read_bytes()), jvm=a['digest'], native=b['digest'])


def finish_report(mismatch, **fields):
    """The release verdict, written after every release check (decision 16)."""
    if mismatch is None:
        return
    path = Path(mismatch) / 'comparison.json'
    report = e.load(path)
    report.update(fields)
    save(path, report)


def coverage_hits(capture):
    """Which JaCoCo probes fired follows test timing; reported per file, never gating (decision 16)."""
    result = {}
    for path, item in sorted(capture['files'].items()):
        if item['kind'] == 'file' and path.startswith('reports/device-files/') and path.endswith('.ec'):
            _, classes = parse_jacoco(item['data'])
            result[UUID.sub('<uuid>', '/'.join(path.split('/')[2:]))] = {name: hits for name, _, hits in classes}
    return result


def coverage_hit_report(expected, actual):
    return {role: dict(expected_total=sum(expected[role].values()), actual_total=sum(actual[role].values()),
                       differing_classes={name: [expected[role][name], actual[role][name]]
                                          for name in sorted(expected[role].keys() & actual[role].keys())
                                          if expected[role][name] != actual[role][name]})
            for role in sorted(expected.keys() & actual.keys())}


def review_witnesses(capture, policy):
    path = capture['pair'] / capture['side'] / 'witnesses.json'
    review = e.load(path)
    require(review.get('review') and review['evidence_sha256'] == capture['digest'], 'missing/stale artifact witness review')
    scenario = SCENARIOS[capture['capture']['scenario']]
    require(set(review['witnesses']) == set(scenario['witnesses']), 'missing scenario witness')
    for name, witness in review['witnesses'].items():
        require(witness.get('observation') and witness.get('paths'), 'empty witness: ' + name)
        for path in witness['paths']:
            require(path in capture['files'], 'missing witness artifact: ' + path)
    # Any P3 byte exemption requires exact-payload-bound human landmark review;
    # ffprobe and full decode are checked mechanically by project().
    media = [r for r in policy['rules'] if r['kind'] == 'media']
    landmarks = review.get('media_landmarks', {})
    paths = {p: [r['id'] for r in media if e.path_glob(p, r['path'])]
             for p, item in capture['files'].items()
             if item['kind'] == 'file' and Path(p).suffix.lower() in MEDIA_SUFFIXES and not empty_recording(p, item)}
    require(set(landmarks) == set(paths) and all(len(ids) == 1 for ids in paths.values()),
            'missing ordered media landmark review')
    require(all(isinstance(states, list) and all(isinstance(s, str) and s.strip() for s in states)
                and (states or p.lower().endswith('.gif')) for p, states in landmarks.items()), 'invalid ordered landmarks')
    # Decision 16: a landmark list belongs to the test, attempt and role that own the recording, not just to the
    # policy rule that matched its path; otherwise two recordings (and their annotations) could trade places.
    owned = attachment_owners(capture)
    for p in landmarks:
        require(owned.get(p), 'recording has no owning test attempt: ' + p)
    gating = sorted((dict(owners=owned[p], rule=paths[p][0], landmarks=states)
                     for p, states in landmarks.items() if not p.lower().endswith('.gif')), key=e.encoded)
    # Keyed by the owning test and the recording's name without its run UUID, so the two sides line up.
    informational = {}
    def label(p):
        return ','.join('%s@%s:%s(%s)' % (o['test'], o['attempt'], o['role'], o['source']) for o in owned[p]) + '|' + re.sub(UUID_SUFFIX, '', Path(p).name)
    for p, states in sorted(landmarks.items(), key=lambda kv: label(kv[0])):
        if p.lower().endswith('.gif'):
            informational[label(p) + '#' + str(sum(k.startswith(label(p) + '#') for k in informational))] = gif_landmarks(p, states)
    return gating, informational


UUID_SUFFIX = r'-[0-9a-fA-F]{8}-[0-9a-fA-F-]{27}(?=\.gif$)'
HOME_STATES = ('launcher', 'home screen')


def gif_landmarks(path, states):
    """Decision 14 (informational view since decision 15): a GIF starts on whatever was on screen when capture began
    (even two Kotlin runs differ), so leading
    home/launcher states are dropped and the comparison starts at the first app state. A GIF that never leaves the launcher
    (or has no landmarks) therefore compares as an empty list, and two such GIFs are equal. Videos stay untouched."""
    if not path.lower().endswith('.gif'):
        return states
    states = list(states)
    while states and any(h in states[0].lower() for h in HOME_STATES):
        states.pop(0)
    return states
