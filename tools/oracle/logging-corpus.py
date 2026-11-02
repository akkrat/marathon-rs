#!/usr/bin/env python3
"""Capture real reference logging. Run after tools/oracle/build.sh reference.

PTY output processing is disabled so the terminal driver cannot add CR bytes.
Each stream/mode case runs in a fresh JVM because Jansi caches stdout's mode.
"""
import errno
import json
import os
from pathlib import Path
import pty
import subprocess
import tempfile
import termios

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / 'fixtures/oracle/logging'
ORACLE = ROOT / 'tools/oracle/oracle'
JAVA_OPTIONS = '-Duser.timezone=UTC -Dfile.encoding=UTF-8 -Dlogback.configurationFile=' + str(ROOT / 'tools/oracle/build/marathon-src/cli/src/main/resources/logback.xml')
ENV = dict(os.environ, JAVA_OPTS=JAVA_OPTIONS, TZ='UTC', TERM='xterm-256color', LANG='en_US.UTF-8',
           JAVA_HOME='/Applications/Android Studio.app/Contents/jbr/Contents/Home')


def capture(layout, target, mode, failure=False):
    env = dict(ENV, JAVA_OPTS=JAVA_OPTIONS +
               ('' if mode == 'default' else ' -Djansi.mode=' + mode))
    command = [str(ORACLE), 'logging-probe', layout]
    if target == 'pipe':
        result = subprocess.run(command, env=env, check=not failure, capture_output=True)
        return (result.stdout, result.returncode) if failure else result.stdout
    if target == 'file':
        with tempfile.TemporaryFile() as output:
            result = subprocess.run(command, env=env, stdout=output, stderr=subprocess.PIPE, check=not failure)
            output.seek(0)
            data = output.read()
            return (data, result.returncode) if failure else data
    master, slave = pty.openpty()
    attrs = termios.tcgetattr(slave)
    attrs[1] &= ~termios.OPOST
    termios.tcsetattr(slave, termios.TCSANOW, attrs)
    process = subprocess.Popen(command, env=env, stdout=slave, stderr=subprocess.PIPE)
    os.close(slave)
    chunks = []
    try:
        while True:
            try:
                chunk = os.read(master, 65536)
            except OSError as error:
                if error.errno != errno.EIO:
                    raise
                break
            if not chunk:
                break
            chunks.append(chunk)
    finally:
        os.close(master)
    _, errors = process.communicate(timeout=60)
    if process.returncode and not failure:
        raise RuntimeError(errors.decode())
    data = b''.join(chunks)
    return (data, process.returncode) if failure else data


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    with (OUT / 'console.jsonl').open('w') as output:
        for layout in ('startup', 'android', 'apple', 'compact'):
            for target in ('terminal', 'pipe', 'file'):
                for mode in ('default', 'force', 'strip'):
                    row = dict(layout=layout, target=target, mode=mode,
                               output=capture(layout, target, mode).decode('utf-8'))
                    output.write(json.dumps(row, ensure_ascii=False) + '\n')
    with (OUT / 'failure-console.jsonl').open('w') as output:
        for layout in ('failure-m', 'failure-G', 'failure-private'):
            for target in ('terminal', 'pipe', 'file'):
                for mode in ('default', 'force', 'strip'):
                    data, code = capture(layout, target, mode, failure=True)
                    output.write(json.dumps(dict(layout=layout, target=target, mode=mode,
                        output=data.decode('utf-8'), exitCode=code)) + '\n')
    with tempfile.TemporaryDirectory() as temp:
        hierarchy = Path(temp) / 'hierarchy.jsonl'
        subprocess.run([str(ORACLE), 'logging-probe', 'hierarchy', str(hierarchy)], env=ENV,
                       capture_output=True, check=True)
        (OUT / 'hierarchy.jsonl').write_bytes(hierarchy.read_bytes())

        state = Path(temp) / 'state.jsonl'
        subprocess.run([str(ORACLE), 'logging-probe', 'state', str(state)], env=ENV,
                       capture_output=True, check=True)
        for line in state.read_text().splitlines():
            json.loads(line)
        (OUT / 'state.jsonl').write_bytes(state.read_bytes())
    result = subprocess.run([str(ORACLE), 'logging-probe', 'details'], env=ENV,
                            capture_output=True, check=True)
    for line in result.stdout.decode().splitlines():
        json.loads(line)
    (OUT / 'details.jsonl').write_bytes(result.stdout)
    result = subprocess.run([str(ORACLE), 'logging-probe', 'failures'], env=ENV,
                            capture_output=True, check=True)
    for line in result.stdout.decode().splitlines():
        json.loads(line)
    (OUT / 'failures.jsonl').write_bytes(result.stdout)
    result = subprocess.run([str(ORACLE), 'logging-probe', 'fallback'], env=ENV,
                            capture_output=True, check=True)
    for line in result.stdout.decode().splitlines():
        json.loads(line)
    (OUT / 'fallback.jsonl').write_bytes(result.stdout)


if __name__ == '__main__':
    main()
