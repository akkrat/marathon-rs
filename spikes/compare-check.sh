#!/usr/bin/env bash
# Regression checks for comparison failures which used to look like parity.
set -euo pipefail
cd "$(dirname "$0")"
python3 - <<'PY'
import os
from pathlib import Path
import subprocess
import tempfile

with tempfile.TemporaryDirectory() as tmp:
    env = dict(os.environ, SPIKE_TMP=tmp)
    def compare():
        return subprocess.run(['./compare.sh', 'fake', 'russh-patched'], env=env, capture_output=True, text=True)
    assert compare().returncode != 0
    left = Path(tmp)/'results/sshj-fake'
    right = Path(tmp)/'results/russh-patched-fake'
    left.mkdir(parents=True)
    right.mkdir(parents=True)
    assert compare().returncode != 0
    for directory in (left, right):
        (directory/'t01.txt').write_text('T01.result: ok\n')
        (directory/'t01.status').write_text('completed\n')
    assert compare().returncode == 0
    (right/'t01.txt').write_text('T01.result: failed\n')
    assert compare().returncode != 0
    for directory in (left, right):
        (directory/'t01.txt').write_text('')
        (directory/'t01.status').write_text('skipped\n')
    result = compare()
    assert result.returncode == 0 and '1 skipped' in result.stdout
    (right/'t01.status').write_text('running\n')
    assert compare().returncode != 0
    for directory, outcome in ((left, '2'), (right, 'hung (no return within 20000 ms)')):
        (directory/'t01.txt').write_text(f'T01.result: ok\nT01.info.close_ms: {outcome}\n')
        (directory/'t01.status').write_text('completed\n')
    assert compare().returncode != 0
    for directory, value in ((left, 'first.info.value'), (right, 'second.info.value')):
        (directory/'t01.txt').write_text(f'T01.result: stdout=[{value}]\n')
    assert compare().returncode != 0
    for directory, value in ((left, 'one'), (right, 'two')):
        (directory/'t01.txt').write_text(f'T01.result: ok\nT01.info.detail: {value}\n')
    assert compare().returncode == 0
    (right/'t01.txt').unlink()
    assert compare().returncode != 0
print('comparison regressions: passed')
PY
