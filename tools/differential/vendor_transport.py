"""Causal transport observations. Raw transcripts are retained beside this projection."""
import base64
from collections import defaultdict
import json
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


def ssh(data, name=None, before_d32=False):
    rows = records(data)
    if before_d32:
        rows = with_d46_probe(rows)
    e.require([r['seq'] for r in rows] == list(range(len(rows))),'truncated/reordered SSH transcript')
    e.require(rows[-1]['event'] == 'cleanup' and rows[-1]['detail']['outcome'] == 'completed'
              and rows[-1]['detail']['active_processes'] == [],'incomplete SSH cleanup')
    if name is not None:
        e.require(all(row['run'] == name for row in rows),'SSH scenario identity differs')
    sessions, lanes = {}, defaultdict(list)
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
    e.require(sessions,'no SSH sessions')
    control = {key[0] for key,s in sessions.items() if not s['wire'].startswith('/usr/bin/rsync --server ') and s['wire'] != 'xcrun simctl list --json'}
    inventory = {key[0] for key,s in sessions.items() if s['wire'] == 'xcrun simctl list --json'}
    transfers = [key[0] for key,s in sessions.items() if s['wire'].startswith('/usr/bin/rsync --server ')]
    e.require(len(control) == 1 and len(inventory) <= 1 and control.isdisjoint(inventory),
              'SSH control/inventory connection roles differ')
    e.require(len(set(transfers)) == len(transfers) and not set(transfers)&(control|inventory),
              'SSH transfer connection role collision')
    ended = {row['connection'] for row in rows if row['event'] == 'connection_end'}
    e.require({key[0] for key in sessions} <= ended,'unfinished SSH connection')
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
# it, differs from them.
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


def host_argv(data, values, before_d31=False):
    lanes = defaultdict(list)
    for row in records(data):
        e.require(row['event'] == 'host_argv' and row['program'] in ('rsync','ssh'),'unknown host argv')
        row = dict(row); args = list(row['argv'])
        if before_d31:
            args = with_d31_flag(row['program'],args)
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
        row['argv'] = args
        # Each send/receive has its own host process and no shared total order.
        # The explicit destination identifies its transfer lane; repeated uses
        # of one destination remain ordered.
        lanes[row['program'] + ':' + args[-1]].append(row)
    return dict(lanes)


# D27: the root wipe that prepare runs on the fake's external storage in a JVM record and in a native record pinned from
# before D27, and that today's program does not. Exactly these commands, and only in those records (the caller decides,
# by `evidence.may_withhold`): a native run of today's code that wiped the storage again differs from them.
D27_WIPE = frozenset(['rm -r /sdcard', 'mkdir /sdcard'])


def adb(data, ran_the_wipe=False):
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
    for session in sessions.values():
        for stream in ('stdout','stderr'):
            session[stream] = base64.b64encode(session[stream]).decode()
        lane = 'logcat:' if session['command'].startswith('logcat') else 'device:'
        lanes[lane + session['serial']].append(session)
    return dict(lanes)
