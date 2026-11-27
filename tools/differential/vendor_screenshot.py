"""D100 projection for the single-frame iOS screenshot SSH scenario."""
import base64
from pathlib import Path
import re
import shlex

import evidence as e

UDID = 'AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE'
PATH = f'/tmp/marathon/{UDID}/{UDID}.png'
LEGACY_FILE = PATH.replace('/tmp/marathon/', 'worker/', 1)
RSYNC = '/usr/bin/rsync --server --sender {protocol-options} . ' + PATH
SIMCTL = ['xcrun', 'simctl', 'io', UDID, 'screenshot', '--type=png', '--display=internal', '--mask=black']
SCRIPT = (
    "umask 077; file=$1; limit=$2; shift 2; "
    "trap '/bin/rm -f \"$file\"' EXIT; trap 'exit 143' TERM; trap 'exit 129' HUP; "
    '"$@" "$file" >/dev/null || exit $?; '
    'size=$(/usr/bin/wc -c < "$file") || exit $?; '
    '[ "$size" -le "$limit" ] || exit 65; '
    'exec 3< "$file"; /bin/rm -f "$file" || exit $?; exec /usr/bin/base64 <&3'
)
FRAME = Path(__file__).resolve().parents[2] / 'fixtures/fake-worker/apple/media/screenshot.png'


def d100_streams(sessions, signals):
    # The world makes the test finish only after it sees a second request.
    # Retain that count/order, the full payload and the stop, not just the GIF.
    frames = []
    files = set()
    for key, session in sessions.items():
        wire = shlex.split(session['wire'])
        e.require(wire != [*SIMCTL, PATH], 'D100 unexpected legacy screenshot exec')
        e.require(not (wire[:3] == ['/usr/bin/rsync', '--server', '--sender'] and wire[-1] == PATH),
                  'D100 unexpected screenshot sender')
        if wire[:2] != ['/bin/sh', '-c'] or len(wire) < 4 or wire[3] != 'marathon-screenshot':
            continue
        e.require(len(wire) == 14 and wire[2] == SCRIPT and wire[5:] == ['25165824', *SIMCTL]
                  and re.fullmatch(re.escape(PATH) + r'\.capture-[0-9a-f]{32}\.png', wire[4])
                  and wire[4] not in files, 'D100 unexpected screenshot wrapper')
        files.add(wire[4])
        e.require(session['end'] == 'completed' and not session['stdin'] and not session['stderr'],
                  'D100 unexpected screenshot streams')
        frames.append((key, session))
    e.require(len(frames) == 2, 'D100 expected exactly two screenshot execs')
    (first_key, first), (second_key, second) = frames
    e.require(first_key[0] == second_key[0] and first['exit'] == 0 and not signals[first_key]
              and first['stdout'] == base64.b64encode(FRAME.read_bytes()) + b'\n',
              'D100 first screenshot differs')
    e.require(second['exit'] in (None, 143) and not second['stdout'] and signals[second_key] == ['TERM'],
              'D100 expected cancellation of the second screenshot')
    for key, _ in frames:
        del sessions[key]


def d100_check_host_argv(lanes):
    for rows in lanes.values():
        for row in rows:
            e.require(not any(arg.endswith(PATH) or (PATH + '.capture-') in arg for arg in row['argv']),
                      'D100 unexpected screenshot rsync process')


def d100_prior_view(view):
    # Only the captured frame's legacy trail differs. Keep the decoded GIF,
    # reports, other execs/rsyncs and the virtual world's checkpoints exact.
    leftover = view.pop(LEGACY_FILE, None)
    e.require(leftover == dict(kind='file', mode=0o600, content=base64.b64encode(FRAME.read_bytes()).decode()),
              'D100 legacy worker frame differs')
    lanes = view['transcript.jsonl']['content']
    sender = lanes.pop('rsync:' + RSYNC, None)
    e.require(sender == [dict(wire=RSYNC, exit=0, end='completed')], 'D100 legacy sender differs')
    control = lanes['control']
    captures = [row for row in control if row.get('wire', '').startswith('xcrun simctl io ')
                and shlex.split(row['wire']) == [*SIMCTL, PATH]]
    e.require(len(captures) == 2 and [row['exit'] for row in captures] == [0, None]
              and all(row['end'] == 'completed' and not any(row[s] for s in ('stdin', 'stdout', 'stderr'))
                      for row in captures), 'D100 legacy screenshot execs differ')
    for row in captures:
        control.remove(row)
    host = view['host-argv.jsonl']['content']
    rsync = host.pop('rsync:{temp_screenshot}', None)
    ssh = host.pop('ssh:' + PATH, None)
    e.require(rsync and len(rsync) == 1 and rsync[0]['argv'][-2:] == ['127.0.0.1:' + PATH, '{temp_screenshot}']
              and ssh and len(ssh) == 1 and ssh[0]['argv'][-5:] == ['--server', '--sender', '{protocol-options}', '.', PATH],
              'D100 legacy screenshot host argv differs')
