"""Causal transport observations. Raw transcripts are retained beside this projection."""
import base64
from collections import defaultdict
import json
from pathlib import Path
import re
import shlex
import evidence as e

REDACTED = 'D20'
# A few hundred bytes of rsync protocol around the 76-byte stand-in; the smallest of the five libraries is 126214 bytes.
STAND_IN_TRANSFER_LIMIT = 16384


def records(data):
    e.require(data and data.endswith(b'\n'),'empty/truncated transcript')
    return [e.loads(line) for line in data.splitlines()]


def payload(detail):
    raw = base64.b64decode(detail['base64'],validate=True)
    e.require(len(raw) == detail['size'] and e.sha(raw) == detail['sha256'],'invalid wire payload')
    return raw


def parser_transfer_keys(rows):
    """Connection and channel of each rsync session that pushes the third-party library (D20)."""
    keys = set()
    for row in rows:
        if row['event'] == 'exec':
            wire = payload(row['detail']).decode()
            if wire.startswith('/usr/bin/rsync --server ') and shlex.split(wire)[-1].endswith('/libxctest-parser.dylib'):
                keys.add((row['connection'],row['channel']))
    return keys


def is_parser_chunk(row, keys):
    return row['event'] == 'stdin' and (row['connection'],row['channel']) in keys


def check_redacted(row, keys):
    """A marked row is zeros in the library's transfer, whatever the rest of the record claims."""
    detail = row['detail']
    e.require(detail['redacted'] == REDACTED,'unknown redaction marker')
    e.require(is_parser_chunk(row,keys),'redaction marker outside the parser transfer')
    data = payload(detail)
    e.require(data == bytes(len(data)),'marked parser chunk is not zero-filled')


def check_stream_bound(rows, keys, skip=()):
    """Every byte the transfer's rows retain counts, whichever stream and however small its chunks."""
    sizes = defaultdict(int)
    for row in rows:
        if ((row['connection'],row['channel']) in keys and row['event'] != 'exec' and row['event'] not in skip
                and 'base64' in row['detail']):
            sizes[row['connection'],row['channel']] += len(payload(row['detail']))
    e.require(all(size <= STAND_IN_TRANSFER_LIMIT for size in sizes.values()),'library bytes in the parser transfer')


def redact_parser_transfer(data):
    """D20: a JVM recording's rsync stdin to the library's path carries the library, so its bytes become zeros.

    Every chunk keeps its row, sequence number and size and gets the digest of what it now holds; the projection in
    ssh() drops rsync streams, so no comparison reads them. Other rows stay byte-identical.
    """
    rows = records(data)
    keys = parser_transfer_keys(rows)
    # Its stdin is about to be zeros; what the worker sent back is kept, and holds no library.
    check_stream_bound(rows,keys,skip=('stdin',))
    lines = data.splitlines(keepends=True)
    for i,line in enumerate(lines):
        row = e.loads(line)
        if 'redacted' in row['detail']:
            check_redacted(row,keys)
        elif is_parser_chunk(row,keys):
            size = len(payload(row['detail']))
            row['detail'].update(base64=base64.b64encode(bytes(size)).decode(),sha256=e.sha(bytes(size)),redacted=REDACTED)
            lines[i] = (json.dumps(row,separators=(',',':'),sort_keys=True) + '\n').encode()
    return b''.join(lines)


def check_new_native_transfer(data):
    """D20: a new native capture pushes the stand-in, and no library fits in the bytes of that transfer."""
    rows = records(data)
    keys = parser_transfer_keys(rows)
    e.require(not any('redacted' in row['detail'] for row in rows),'redacted row in a new native transcript')
    check_stream_bound(rows,keys)


# D32: what today's program puts before the one command that has the variable, and a JVM record or a native record pinned
# from before D32 does not. As with D31, those records get exactly this prefix on exactly this command, so a native run
# without it, or with it elsewhere, differs from them. See RE-PINNING above D31_FLAG: this refuses a newer pinned record.
D32_PREFIX = 'env NSUnbufferedIO=YES '
D32_COMMAND = 'xcrun xcodebuild test-without-building '


def with_d32_prefix(wire):
    e.require(not wire.startswith(D32_PREFIX),'D32 prefix in a record from before it')
    return D32_PREFIX + wire if wire.startswith(D32_COMMAND) else wire


def without_d32_prefix(wire):
    """The form the command catalogue describes."""
    return wire.removeprefix(D32_PREFIX) if wire.startswith(D32_PREFIX + D32_COMMAND) else wire


# D46: the readiness probe of today's program on runtime 18 and later, which a JVM record or a native record pinned from
# before D46 shows as the older `dumpstate`. Those records get exactly this wire on exactly the probes whose own record says
# the simulator runs runtime 18 or later (its `SIMULATOR_RUNTIME_VERSION` reply), so a native run that kept `dumpstate`
# there, used `print` on runtime 16 or 17, or whose runtime was not recorded, differs from them. As with D31 and D32 the
# caller decides by `evidence.may_withhold`, and a record from before D46 that already has the new wire is refused.
D46_OLD = 'launchctl dumpstate user/foreground/com.apple.SpringBoard'
D46_NEW = 'launchctl print user/foreground'
D46_FIRST_RUNTIME = 18
D46_GETENV = re.compile(r'xcrun simctl getenv (\S+) SIMULATOR_RUNTIME_VERSION')
D46_PROBE_OLD = re.compile(r'xcrun simctl spawn (\S+) ' + re.escape(D46_OLD))
D46_PROBE_NEW = re.compile(r'xcrun simctl spawn (\S+) ' + re.escape(D46_NEW))


def runtime_major(reply):
    """The integer before the first dot as the program parses it (ASCII digits, one optional plus, at most `i32::MAX`); None
    when it is not one."""
    head = reply.decode(errors='replace').strip().split('.')[0]
    value = int(head) if re.fullmatch(r'\+?[0-9]+',head) else None
    return value if value is not None and value <= 2**31 - 1 else None


def with_d46_probe(rows):
    """The rows of an older record with the probes of runtime 18 and later as today's program sends them."""
    wires, replies = {}, defaultdict(bytes)
    for row in rows:
        key = (row['connection'],row['channel'])
        if row['event'] == 'exec':
            wires[key] = payload(row['detail']).decode()
        elif row['event'] == 'output' and key in wires and row['detail'].get('extended') is None:
            replies[key] += payload(row['detail'])
    majors = {m[1]:runtime_major(replies[key]) for key,wire in wires.items() if (m := D46_GETENV.fullmatch(wire))}
    result = []
    for row in rows:
        if row['event'] == 'exec':
            wire = wires[row['connection'],row['channel']]
            e.require(not D46_PROBE_NEW.fullmatch(wire),'D46 probe in a record from before it')
            probe = D46_PROBE_OLD.fullmatch(wire)
            if probe and (majors.get(probe[1]) or 0) >= D46_FIRST_RUNTIME:
                data = ('xcrun simctl spawn ' + probe[1] + ' ' + D46_NEW).encode()
                row = dict(row,detail=dict(row['detail'],base64=base64.b64encode(data).decode(),sha256=e.sha(data),size=len(data)))
        result.append(row)
    return result


def without_d46_probe(wire):
    """The form the command catalogue describes: the one probe form that it lists."""
    probe = D46_PROBE_NEW.fullmatch(wire)
    return 'xcrun simctl spawn ' + probe[1] + ' ' + D46_OLD if probe else wire


D96_VIDEO = re.compile(
    r'xcrun simctl io ([A-F0-9-]{36}) recordVideo --codec h264 --display internal --mask black --force '
    r'/tmp/marathon/\1/[^/ ]+\.mp4'
)


def d96_recorder(sessions, signals, legacy):
    """Compare the one recording in ios-media and retain every unrelated command."""
    wrapper = "sh -c 'nohup "
    candidates = []
    for key, session in sessions.items():
        wire = session['wire']
        direct = wire
        if legacy and wire.startswith(wrapper):
            direct = wire[len(wrapper):].split(' > /dev/null 2>&1 & echo $! > ', 1)[0]
        if match := D96_VIDEO.fullmatch(direct):
            pidfile = f'/tmp/marathon/{match[1]}/{match[1]}.pid'
            expected = f"{wrapper}{direct} > /dev/null 2>&1 & echo $! > {pidfile}'" if legacy else direct
            e.require(wire == expected, 'D96 unexpected recorder command')
            candidates.append((key, direct, pidfile))
    e.require(len(candidates) == 1, 'D96 expected one recorder')
    key, direct, pidfile = candidates[0]
    start = sessions[key]
    e.require(_d96_recorder_reply(start, 0), 'D96 recorder did not report success')
    removed = {key}
    if legacy:
        e.require(not signals[key], 'D96 unexpected signal on legacy recorder launcher')
        stop = f"sh -c 'kill -s INT $(cat {pidfile}) && lsof -p $(cat {pidfile}) +r 1 &>/dev/null'"
        observed = []
        for candidate, session in sessions.items():
            wire = session['wire']
            if candidate == key:
                observed.append('launch')
            elif wire == f'cat {pidfile}':
                if _d96_recorder_reply(session, 1, stderr=b'No such file\n'):
                    observed.append('absent')
                else:
                    e.require(_d96_recorder_reply(session, 0, stdout=b'1001\n'), 'D96 unexpected recorder pid lookup')
                    observed.append('pid')
                removed.add(candidate)
            elif wire in (stop, f'rm -rf {pidfile}'):
                e.require(_d96_recorder_reply(session, 0), 'D96 legacy recorder stop failed')
                observed.append('stop' if wire == stop else 'remove')
                removed.add(candidate)
        e.require(observed == ['absent', 'absent', 'launch', 'pid', 'pid', 'stop', 'remove', 'absent'],
                  'D96 unexpected legacy recorder lifecycle')
    else:
        e.require(signals[key] == ['INT'], 'D96 expected one INT on the foreground recorder')
    return removed, [dict(wire=direct, exit=0, end='completed')]


def _d96_recorder_reply(session, status, stdout=b'', stderr=b''):
    return (session['exit'], session['stdout'], session['stderr'], session['stdin'], session['end']) == (
        status, stdout, stderr, b'', 'completed'
    )


def d96_native_golden(lanes):
    """The pinned ios-media native view predates the foreground recorder."""
    control = lanes['control']
    sessions = {
        index: dict(session, **{stream: base64.b64decode(session[stream], validate=True)
                                for stream in ('stdout', 'stderr', 'stdin')})
        for index, session in enumerate(control)
        if 'wire' in session
    }
    removed, recorder = d96_recorder(sessions, defaultdict(list), True)
    return dict(lanes, recorder=recorder, control=[s for index, s in enumerate(control) if index not in removed])


def _d96_probe(session):
    wire = session['wire']
    if not wire.startswith('/usr/bin/perl -MMIME::Base64 -e '):
        return False
    source = Path(__file__).resolve().parents[2] / 'crates/marathon-apple/src/cmd/remote/ssh/sshj/supervisor'
    expected = ['/usr/bin/perl', '-MMIME::Base64', '-e', 'eval decode_base64 shift; die $@ if $@', '--',
                base64.b64encode((source / 'bootstrap.pl').read_bytes()).decode(),
                base64.b64encode((source / 'worker.pl').read_bytes()).decode()]
    e.require(_d96_words(wire) == expected, 'D96 unexpected worker probe')
    return True


D96_ROOT = r'/tmp/marathon-supervisor-[a-zA-Z0-9_-]{10}'
D96_TOKEN = r'[a-f0-9]{32}'
D96_RSYNC = base64.b64encode(b'/usr/bin/rsync').decode()


def _d96_words(wire):
    try:
        return shlex.split(wire)
    except ValueError:
        raise e.Invalid('D96 malformed supervisor argv') from None


def _d96_fallback_bootstrap(session):
    if not _d96_probe(session):
        return False
    e.require(isinstance(session['exit'], int) and 0 < session['exit'] <= 255 and
              session['end'] == 'completed' and not session['stdin'], 'D96 unexpected worker probe response')
    return True


def _d96_header(argv):
    e.require(len(argv) >= 5 and re.fullmatch(D96_ROOT, argv[3]) and
              argv[:2] == ['/usr/bin/perl', argv[3] + '/supervisor.pl'] and
              re.fullmatch(r'[0-9]{5,7}', argv[4]), 'D96 invalid supervisor argv')
    lease = int(argv[4])
    e.require(27000 <= lease <= 1264000 and lease % 1000 == 0, 'D96 invalid lease period')
    return argv[3], argv[4]


def _d96_sender(argv):
    root, lease = _d96_header(argv)
    e.require(len(argv) >= 7 and argv[2] == 'rsync' and re.fullmatch(D96_TOKEN, argv[5]) and argv[6] == D96_RSYNC,
              'D96 invalid supervisor rsync sender')
    return root, lease, argv[5]


def d96_worker_sessions(sessions, signals, fallback_connections=None):
    """Validate the envelope and receipts before comparing the original payloads."""
    boots, roots, commands, requests, retirement = set(), {}, {}, [], []
    for key, session in list(sessions.items()):
        wire = session['wire']
        if _d96_probe(session):
            e.require(key[0] not in boots and not signals[key], 'D96 duplicate or signalled worker probe')
            boots.add(key[0])
            if session['exit'] != 0:
                _d96_fallback_bootstrap(session)
                if fallback_connections is not None:
                    fallback_connections.add(key[0])
            else:
                root = re.fullmatch(rb'MARATHON_D96_1 (' + D96_ROOT.encode() + rb')\n', session['stdout'])
                e.require(root and _d96_recorder_reply(session, 0, stdout=session['stdout']),
                          'D96 invalid supervisor installation')
                root = root[1].decode()
                e.require(root not in roots, 'D96 duplicate supervisor root')
                roots[root] = (key[0], session['seq'])
            del sessions[key]
        elif wire.removeprefix('exec ').startswith('/usr/bin/perl /tmp/marathon-supervisor-'):
            argv = _d96_words(wire.removeprefix('exec '))
            root, lease = _d96_header(argv)
            e.require(not signals[key], 'D96 unexpected SSH signal on supervisor channel')
            if argv[2] == 'control':
                e.require(len(argv) == 6, 'D96 unexpected control arguments')
                requests.append((key, session, root, lease, argv[5]))
                continue
            if argv[2] == 'retire':
                e.require(len(argv) == 5 and not wire.startswith('exec '), 'D96 unexpected retirement arguments')
                retirement.append((key, session, root, lease))
                continue
            e.require(len(argv) >= 6, 'D96 supervisor token missing')
            token = argv[5]
            e.require(re.fullmatch(D96_TOKEN, token) and token not in commands,
                      'D96 invalid or reused supervised exec token')
            mode = argv[2]
            sender = mode == 'rsync'
            if sender:
                _d96_sender(argv)
                remote = argv[7:]
                e.require(remote[:2] == ['--server', '--sender'] and len(remote) >= 4 and remote[-2] == '.',
                          'D96 invalid wrapped sender framing')
                session['wire'] = shlex.join(['/usr/bin/rsync', *remote])
            else:
                e.require(wire.startswith('exec ') and mode in ('run', 'detach'), 'D96 invalid supervised command')
                if argv[6:8] == ['/bin/sh', '-c']:
                    e.require(len(argv) == 9 and argv[8], 'D96 invalid supervised shell command')
                    session['wire'] = argv[8]
                else:
                    e.require(len(argv) >= 7 and argv[6].startswith('/') and
                              all(re.fullmatch(r'[a-zA-Z0-9/._-]+', part) for part in argv[6:]),
                              'D96 invalid supervised literal command')
                    session['wire'] = ' '.join(argv[6:])
            if mode == 'detach':
                e.require(D96_VIDEO.fullmatch(session['wire']) and _d96_recorder_reply(session, 0),
                          'D96 invalid detached recorder acknowledgement')
            commands[token] = (key, session, root, lease, mode)
    proofs = {}
    for key, session, root, lease, request in requests:
        matched = re.fullmatch(fr'({D96_TOKEN}):(status|renew-[0-9]{{1,16}}|[123])', request)
        e.require(matched and matched[1] in commands, 'D96 invalid or unowned control request')
        token, operation = matched.groups()
        launch_key, launch, command_root, command_lease, mode = commands[token]
        e.require((root, lease) == (command_root, command_lease), 'D96 control addresses another lease')
        e.require(root in roots and session['seq'] > roots[root][1], 'D96 control precedes installation')
        e.require(_d96_recorder_reply(session, 0, stdout=session['stdout']), 'D96 invalid control channel end')
        reply = re.fullmatch(token.encode() + rb' (unknown|busy|running:[0-9]{1,16}|done:[0-9]{1,3})\n',
                             session['stdout'])
        e.require(reply, 'D96 invalid worker receipt')
        value = reply[1]
        if value.startswith(b'done:'):
            status = int(value[5:])
            e.require(value[5:] == str(status).encode() and status <= 255 and
                      (mode == 'detach' or status == launch['exit']) and
                      (token not in proofs or proofs[token] == status) and session['seq'] > launch['seq'],
                      'D96 worker receipt differs from the payload exit')
            proofs[token] = status
        elif operation in ('1', '2', '3') and value.startswith(b'running:'):
            signals[launch_key].append({'1': 'INT', '2': 'TERM', '3': 'KILL'}[operation])
        del sessions[key]
    for token, (key, session, root, _, mode) in commands.items():
        e.require(root in roots and roots[root][1] < session['seq'], 'D96 worker token has no installed supervisor')
        e.require((mode == 'rsync') != (key[0] == roots[root][0]), 'D96 wrong supervisor transport for payload')
        e.require(token in proofs or (mode == 'run' and session['end'] == 'completed' and session['exit'] is not None),
                  'D96 missing worker exit proof')
        if mode == 'detach':
            session['exit'] = proofs[token]
    retired = set()
    for key, session, root, lease in retirement:
        e.require(root in roots and root not in retired and session['seq'] > roots[root][1],
                  'D96 retirement has no active installation')
        requests_on_root = [(k[0], s) for k, s, worker, _, _ in requests if worker == root]
        e.require(key[0] == roots[root][0] or any(key[0] == connection for connection, _ in requests_on_root),
                  'D96 retirement used an unrelated connection')
        payloads = [(s, period) for _, s, worker, period, _ in commands.values() if worker == root]
        e.require(all(period == lease for _, period in payloads), 'D96 retirement addresses another lease')
        response = session['stdout']
        e.require(response in (b'MARATHON_D96_1 busy\n', b'MARATHON_D96_1 retired\n') and
                  _d96_recorder_reply(session, 0, stdout=response), 'D96 invalid retirement response')
        if response == b'MARATHON_D96_1 retired\n':
            e.require(all(s['seq'] < session['seq'] for s, _ in payloads) and
                      all(s['seq'] < session['seq'] for _, s in requests_on_root),
                      'D96 supervisor retired before its commands')
            retired.add(root)
        del sessions[key]
    e.require(retired == {root for _, _, root, _ in retirement}, 'D96 supervisor retirement did not finish')
    return {(root, lease, token) for token, (_, _, root, lease, mode) in commands.items() if mode == 'rsync'}


def ssh(data, name=None, before_d32=False, *, d96_recorder_transport=False, d96_worker_transport=False, sender_tokens=None):
    rows = records(data)
    if before_d32:
        rows = with_d46_probe(rows)
    e.require([r['seq'] for r in rows] == list(range(len(rows))),'truncated/reordered SSH transcript')
    e.require(rows[-1]['event'] == 'cleanup' and rows[-1]['detail']['outcome'] == 'completed'
              and rows[-1]['detail']['active_processes'] == [],'incomplete SSH cleanup')
    if name is not None:
        e.require(all(row['run'] == name for row in rows),'SSH scenario identity differs')
    sessions, lanes = {}, defaultdict(list)
    signals = defaultdict(list)
    for row in rows:
        event = row['event']; detail = row['detail']
        e.require(event not in ('warning','route_error'),'unmatched SSH command')
        key = (row['connection'],row['channel'])
        if event == 'exec':
            e.require(key not in sessions,'duplicate exec')
            wire = payload(detail).decode()
            wire = with_d32_prefix(wire) if before_d32 else wire
            sessions[key] = dict(wire=wire,stdout=b'',stderr=b'',stdin=b'',exit=None,end=None,seq=row['seq'])
        elif event in ('output','stdin') and key in sessions:
            e.require(sessions[key]['end'] is None,'payload after SSH channel completion')
            stream = 'stdin' if event == 'stdin' else 'stderr' if detail.get('extended') is not None else 'stdout'
            sessions[key][stream] += payload(detail)
        elif event == 'exit_status' and key in sessions:
            e.require(sessions[key]['exit'] is None,'duplicate SSH exit')
            sessions[key]['exit'] = detail['status']
        elif event == 'channel_end' and key in sessions:
            sessions[key]['end'] = detail['outcome']
        elif event == 'signal' and key in sessions:
            signals[key].append(detail.get('signal'))
    e.require(sessions,'no SSH sessions')
    ended = {row['connection'] for row in rows if row['event'] == 'connection_end'}
    e.require({key[0] for key in sessions} <= ended,'unfinished SSH connection')
    fallback_connections = set()
    if d96_worker_transport:
        senders = d96_worker_sessions(sessions, signals, fallback_connections)
        if sender_tokens is not None:
            sender_tokens.update(senders)
    control = {key[0] for key,s in sessions.items() if not s['wire'].startswith('/usr/bin/rsync --server ') and s['wire'] != 'xcrun simctl list --json'}
    inventory = {key[0] for key,s in sessions.items() if s['wire'] == 'xcrun simctl list --json'}
    transfers = [key[0] for key,s in sessions.items() if s['wire'].startswith('/usr/bin/rsync --server ')]
    e.require(len(control) == 1 and len(inventory) <= 1 and control.isdisjoint(inventory),
              'SSH control/inventory connection roles differ')
    e.require(len(set(transfers)) == len(transfers) and not set(transfers)&(control|inventory),
              'SSH transfer connection role collision')
    if name == 'ios-media' and d96_recorder_transport:
        removed, lanes['recorder'] = d96_recorder(sessions, signals, before_d32 or control <= fallback_connections)
        sessions = {key: value for key, value in sessions.items() if key not in removed}
    preparing = False
    for session in sessions.values():
        wire = session['wire']
        e.require(session['end'] == 'completed','unfinished SSH command')
        e.require(session['exit'] is not None or (wire.startswith('xcrun simctl io ') and ' screenshot ' in wire),
                  'missing SSH exit status')
        # These are source-declared parallel activities. Keep order and
        # multiplicity within each lane; ordinary control commands stay ordered.
        if wire.startswith('/usr/bin/rsync --server '):
            args = shlex.split(wire)
            e.require(args[0:2] == ['/usr/bin/rsync','--server'] and args[-2] == '.', 'unknown rsync server framing')
            session['wire'] = args[0] + ' --server ' + ('--sender ' if '--sender' in args else '') + '{protocol-options} . ' + args[-1]
            lane = 'rsync:' + session['wire']
            # Native rsync protocol traffic is diagnostic. Host argv, status,
            # and complete transferred/final filesystem bytes are contracts.
            session = {k:v for k,v in session.items() if k not in ('stdin','stdout','stderr')}
        elif wire.startswith("sh -c 'nm -gUj "): lane = 'nm-swift'
        elif wire.startswith("sh -c 'nm -U "): lane = 'nm-objc'
        elif wire.startswith('mkdir -p /tmp/marathon/') or wire.startswith('test -e /tmp/marathon/shared/'):
            lane = 'shared-transfers'
        elif wire == 'xcrun simctl list --json': lane = 'inventory'
        else: lane = 'control'
        if name is not None and name.startswith('ios-') and lane == 'control':
            if wire.startswith("pkill -f 'simctl io "):
                preparing = True
                # AppleSimulatorDevice.prepare joins installation and boot
                # before the next ordinary control command is allowed.
                lanes['control'].append(dict(phase='ios-preparation-fork-join'))
            elif preparing:
                if (wire.startswith('/usr/libexec/PlistBuddy ') or
                        re.fullmatch(r'cat /Users/worker/Library/Developer/CoreSimulator/Devices/[^/]+/device.plist',wire) or
                        re.fullmatch(r'xcrun simctl boot [^ ]+',wire) or
                        re.fullmatch(r'xcrun simctl spawn [^ ]+ launchctl (dumpstate user/foreground/com\.apple\.SpringBoard|print user/foreground)',wire)):
                    lane = 'prepare:simulator'
                elif (wire == 'xcrun --sdk iphonesimulator --show-sdk-platform-path' or
                        wire.startswith(("chmod +x '", 'rm -R /tmp/marathon/', 'ln -s /tmp/marathon/shared/', 'lipo /tmp/marathon/')) or
                        (wire.startswith("sh -c 'umask 077 && echo ") and "/marathon.xctestrun.marathon-tmp" in wire)):
                    lane = 'prepare:installation'
                else:
                    preparing = False
        session.pop('seq')
        for stream in ('stdout','stderr','stdin'):
            if stream in session: session[stream] = base64.b64encode(session[stream]).decode()
        lanes[lane].append(session)
    return dict(lanes)


# D31: what today's program passes to rsync on a send, and a JVM record or a native record pinned from before D31 lacks.
# Those records get exactly this flag at exactly its place, on exactly their sends (the caller decides, by
# `evidence.may_withhold`). A native run of today's code is left as it is, so a send without the flag, or a receive with
# it, differs from them. D43: the same flag on a receive, in `with_d43_flag`. Both go on the records from before D31, so a
# record is either from before both or of today's code.
#
# RE-PINNING: a native record made after D31 and D32 already has the flag and the prefix, so pinning one in a verification
# manifest makes `may_withhold` true for it, and this file then refuses it: "D31 flag in a record from before it" or "D32
# prefix in a record from before it". That is deliberate. Whoever re-pins must scope these two allowances to the records
# from before them (the JVM's and the digests pinned before x13), not to `may_withhold`, which D20 and D27 keep using. D46's
# rewrite, which rides on the D32 flag in ssh(), is refused the same way: "D46 probe in a record from before it".
D31_FLAG = '--timeout=300'


def with_d31_flag(program, args):
    if program == 'rsync' and args[-1].startswith('127.0.0.1:'):
        e.require(D31_FLAG not in args,'D31 flag in a record from before it')
        args.insert(args.index('--times') + 1,D31_FLAG)
    return args


def with_d43_flag(program, args):
    if program == 'rsync' and args[-2].startswith('127.0.0.1:'):
        e.require(D31_FLAG not in args,'D43 flag in a record from before it')
        args.insert(args.index('--times') + 1,D31_FLAG)
    return args


# D97: only a native artifact receive's destination gains a private directory.
# Keep its worker source, flags, report role, and final file name.
D97_WORKER_PATH = r'127\.0\.0\.1:/tmp/marathon/(?P<device>[A-Fa-f0-9-]{36})/'
D97_REPORT_PATH = r'\{sandbox\}/in/report/'
D97_STAGE = r'\.marathon-artifact-[A-Za-z0-9]{6}/'
D97_ARTIFACT_DESTINATIONS = (
    (D97_WORKER_PATH + r'(?P=device)\.[A-Fa-f0-9-]{36}\.xcresult',
     '(' + D97_REPORT_PATH + r'device-files/[^/]+/\{safe_serial\}/xcresult/)' + D97_STAGE + '()'),
    (D97_WORKER_PATH + r'[^/]+\.mp4(?:\.tmp)?',
     '(' + D97_REPORT_PATH + r'video/[^/]+/\{safe_serial\}/)' + D97_STAGE + r'([^/]+\.mp4)'),
    (D97_WORKER_PATH + r'device\.log',
     '(' + D97_REPORT_PATH + r'device-logs/[^/]+/\{safe_serial\}/)' + D97_STAGE + r'([^/]+)'),
)


def d97_staging_destination(args):
    if len(args) < 2:
        return
    for source, destination in D97_ARTIFACT_DESTINATIONS:
        if re.fullmatch(source, args[-2]):
            stage = re.fullmatch(destination, args[-1])
            if stage:
                args[-1] = stage[1] + stage[2]
            return


def host_argv(data, values, before_d31=False, d97_native_staging=False, *, d96_worker=False, sender_tokens=None):
    lanes, wrapped = defaultdict(list), defaultdict(set)
    def sender(program, argv):
        identity = _d96_sender(argv)
        if sender_tokens is not None:
            e.require(identity in sender_tokens and identity not in wrapped[program],
                      'D96 duplicate or unobserved host sender token')
            wrapped[program].add(identity)
    for row in records(data):
        e.require(row['event'] == 'host_argv' and row['program'] in ('rsync','ssh'),'unknown host argv')
        row = dict(row); args = list(row['argv'])
        if d96_worker and row['program'] == 'rsync':
            for i, arg in enumerate(args):
                if arg.startswith('--rsync-path=/usr/bin/perl /tmp/marathon-supervisor-'):
                    worker_argv = _d96_words(arg.removeprefix('--rsync-path='))
                    sender("rsync", worker_argv)
                    e.require(len(worker_argv) == 7 and args[-2].startswith('127.0.0.1:'),
                              'D96 wrapped a host rsync receiver or added sender arguments')
                    args[i] = '--rsync-path=/usr/bin/rsync'
        if d96_worker and row['program'] == 'ssh':
            start = args.index('127.0.0.1') + 1
            if args[start].startswith('/usr/bin/perl'):
                worker_argv = _d96_words(' '.join(args[start:]))
                sender("ssh", worker_argv)
                e.require(worker_argv[7:9] == ['--server', '--sender'],
                          'D96 wrapped a worker rsync receiver')
                args = [*args[:start], '/usr/bin/rsync', *worker_argv[7:]]
        if before_d31:
            args = with_d43_flag(row['program'],with_d31_flag(row['program'],args))
        for i,arg in enumerate(args):
            bound = next((key for key,value in values.items() if key.startswith('temp_') and arg == value),None)
            if bound:
                args[i] = '{' + bound + '}'
                continue
            arg = arg.replace(values['sandbox']+'/', '{sandbox}/')
            arg = arg.replace('/'+values['safe_serial']+'/', '/{safe_serial}/')
            if row['program'] == 'ssh' and i and args[i-1] == '-p' and i < args.index('127.0.0.1'):
                e.require(arg == values['port'],'inconsistent SSH port')
                arg = '{port}'
            if i and args[i-1] == '--rsh':
                e.require(' -p ' + values['port'] + ' ' in arg,'inconsistent --rsh port')
                arg = arg.replace(' -p ' + values['port'] + ' ',' -p {port} ')
            args[i] = arg
        if row['program'] == 'ssh':
            index = args.index('/usr/bin/rsync')
            remote = args[index:]
            e.require(remote[1] == '--server' and remote[-2] == '.', 'unknown host rsync framing')
            args = args[:index+1] + ['--server'] + (['--sender'] if '--sender' in remote else []) + ['{protocol-options}', '.', remote[-1]]
        if d97_native_staging and row['program'] == 'rsync':
            d97_staging_destination(args)
        row['argv'] = args
        # Each send/receive has its own host process and no shared total order.
        # The explicit destination identifies its transfer lane; repeated uses
        # of one destination remain ordered.
        lanes[row['program'] + ':' + args[-1]].append(row)
    if sender_tokens is not None:
        e.require(wrapped['rsync'] == wrapped['ssh'] == sender_tokens,
                  'D96 worker sender lacks a matching host invocation')
    return dict(lanes)


# D27: the root wipe that prepare runs on the fake's external storage in a JVM record and in a native record pinned from
# before D27, and that today's program does not. Exactly these commands, and only in those records (the caller decides,
# by `evidence.may_withhold`): a native run of today's code that wiped the storage again differs from them.
D27_WIPE = frozenset(['rm -r /sdcard', 'mkdir /sdcard'])

# D85: the older steady-state recordings have no boot ID read during setup. Match its exact wire request
# between the setup's final property read and feature query, then require the fake's complete built-in reply.
D85_BOOT_ID = 'cat /proc/sys/kernel/random/boot_id'
D85_MARKER = 'marker=/data/local/tmp/.marathon-ready-$token; '
D85_WRITE = '(umask 077; echo "$token" > "$marker") 2>/dev/null && echo "$token"'
D85_REMOVE = 'rm -f "$marker"'
D85_TOKEN = re.compile(r'token=([0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12})')


def d85_setup_requests(rows):
    requests = defaultdict(list)
    for row in rows:
        if row['type'] != 'request':
            continue
        service = row['service']
        serial = row.get('serial')
        if service.startswith('host-serial:') and service.endswith(':features'):
            serial = service[len('host-serial:'):-len(':features')]
        if serial is not None and not service.startswith('host:transport:') and service != 'shell:logcat -v long':
            requests[serial].append(row)
    boots = {
        (probe['conn'], serial)
        for serial, lane in requests.items()
        for before, probe, after in zip(lane, lane[1:], lane[2:])
        if before['service'] == 'shell:getprop;echo x$?'
        and probe['service'] == f'shell:{D85_BOOT_ID};echo x$?'
        and after['service'] == f'host-serial:{serial}:features'
    }
    writes = {
        (write['conn'], serial): token
        for serial, lane in requests.items()
        for before, write in zip(lane, lane[1:])
        if before['service'] == 'shell:logcat -c;echo x$?'
        and (token := d85_marker_service(write['service'], D85_WRITE))
    }
    removals = {
        (row['conn'], serial): token
        for serial, lane in requests.items()
        for row in lane
        if (token := d85_marker_service(row['service'], D85_REMOVE))
    }
    return boots, writes, removals


def d85_marker_service(service, suffix):
    return (service.startswith('shell:') and service.endswith(';echo x$?')
            and d85_marker_command(service[len('shell:'):-len(';echo x$?')], suffix))


def d85_marker_command(command, suffix):
    prefix, sep, body = command.partition('; ')
    token = D85_TOKEN.fullmatch(prefix)
    return token[1] if token and sep and body == D85_MARKER + suffix and int(token[1].replace('-', ''), 16) else None


def d85_builtin_reply(session, command, stdout):
    return session == {
        'serial': session['serial'], 'command': command, 'service': 'shell', 'protocol': 'raw',
        'exit_code_trick': True, 'stdin_bytes': None, 'stdin_sha256': None, 'handler': 'builtin',
        'rule': None, 'exit_code': 0, 'hang': False,
        'stdout': stdout, 'stderr': b'',
    }


def adb(data, ran_the_wipe=False, d85_native_setup=False):
    rows = records(data)
    e.require(rows[-1] == dict(type='event',event='server_stopped'),'incomplete ADB shutdown')
    sessions, lanes, wiped = {}, defaultdict(list), set()
    for row in rows:
        kind = row['type']
        e.require(kind != 'warning','unmatched ADB request')
        if kind == 'shell' and ran_the_wipe and row.get('command') in D27_WIPE:
            wiped.add(row['conn'])
        elif kind == 'output' and row['conn'] in wiped:
            pass
        elif kind == 'shell':
            e.require(row['conn'] not in sessions,'duplicate ADB shell')
            value = {k:v for k,v in row.items() if k not in ('conn','type')}
            value.update(stdout=b'',stderr=b'')
            sessions[row['conn']] = value
        elif kind == 'output':
            e.require(row['conn'] in sessions and row['stream'] in ('stdout','stderr'),'orphan ADB output')
            value = row['bytes']
            e.require(isinstance(value,list) and all(type(x) is int and 0 <= x <= 255 for x in value),'invalid ADB output')
            sessions[row['conn']][row['stream']] += bytes(value)
        elif kind == 'sync':
            value = {k:v for k,v in row.items() if k != 'conn'}
            lanes['sync:' + row['serial']].append(value)
        elif kind == 'request':
            service = row['service']
            e.require(service != 'host:kill','unexpected kill-server')
            # host negotiation/transport selection is client protocol plumbing;
            # retain it raw. Every shell/sync operation below remains compared.
        elif kind == 'event':
            if row.get('event') not in ('server_stopped','client_closed'):
                lanes['events'].append({k:v for k,v in row.items() if k != 'conn'})
        else:
            raise e.Invalid('unknown ADB transcript record: ' + kind)
    setup_probes, marker_writes, marker_removals = d85_setup_requests(rows) if d85_native_setup else (set(), {}, {})
    active_markers = set()
    for conn, session in sessions.items():
        serial, command = session['serial'], session['command']
        if d85_native_setup:
            if ((conn, serial) in setup_probes
                    and d85_builtin_reply(session, D85_BOOT_ID, b'00000000-0000-4000-8000-000000000001\n')):
                continue
            if (conn, serial) in marker_writes:
                token = d85_marker_command(command, D85_WRITE)
                if (token == marker_writes[conn, serial]
                        and (serial, token) not in active_markers
                        and d85_builtin_reply(session, command, (token + '\n').encode())):
                    active_markers.add((serial, token))
                    continue
            if (conn, serial) in marker_removals:
                token = d85_marker_command(command, D85_REMOVE)
                if (token == marker_removals[conn, serial]
                        and (serial, token) in active_markers
                        and d85_builtin_reply(session, command, b'')):
                    active_markers.remove((serial, token))
                    continue
        for stream in ('stdout','stderr'):
            session[stream] = base64.b64encode(session[stream]).decode()
        lane = 'logcat:' if session['command'].startswith('logcat') else 'device:'
        lanes[lane + session['serial']].append(session)
    return dict(lanes)
