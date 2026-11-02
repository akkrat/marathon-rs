"""Causal transport observations. Raw transcripts are retained beside this projection."""
import base64
from collections import defaultdict
import json
import re
import shlex
import evidence as e


def records(data):
    e.require(data and data.endswith(b'\n'),'empty/truncated transcript')
    return [e.loads(line) for line in data.splitlines()]


def payload(detail):
    raw = base64.b64decode(detail['base64'],validate=True)
    e.require(len(raw) == detail['size'] and e.sha(raw) == detail['sha256'],'invalid wire payload')
    return raw


def ssh(data, name=None):
    rows = records(data)
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
                        re.fullmatch(r'xcrun simctl spawn [^ ]+ launchctl dumpstate user/foreground/com.apple.SpringBoard',wire)):
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


def host_argv(data, values):
    lanes = defaultdict(list)
    for row in records(data):
        e.require(row['event'] == 'host_argv' and row['program'] in ('rsync','ssh'),'unknown host argv')
        row = dict(row); args = list(row['argv'])
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


def adb(data):
    rows = records(data)
    e.require(rows[-1] == dict(type='event',event='server_stopped'),'incomplete ADB shutdown')
    sessions, lanes = {}, defaultdict(list)
    for row in rows:
        kind = row['type']
        e.require(kind != 'warning','unmatched ADB request')
        if kind == 'shell':
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
