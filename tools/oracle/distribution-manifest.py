#!/usr/bin/env python3
"""Observe a staged RELEASE :cli:distZip, keeping only sanitized packaging evidence."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import zipfile

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('archive', type=Path)
p.add_argument('--version', required=True)
a = p.parse_args()
with zipfile.ZipFile(a.archive) as z, tempfile.TemporaryDirectory(prefix='Kotlin distribution with spaces ') as tmp:
    root = f'marathon-{a.version}'
    z.extractall(tmp)
    launchers = []
    for name in (f'{root}/bin/marathon', f'{root}/bin/marathon.bat'):
        info = z.getinfo(name)
        path = Path(tmp) / name
        path.chmod((info.external_attr >> 16) & 0o777)
        launchers.append(dict(path=name, mode=oct(info.external_attr >> 16), sha256=hashlib.sha256(z.read(name)).hexdigest()))
    cwd = Path(tmp) / 'unrelated cwd'
    cwd.mkdir()
    env = dict(PATH='/usr/bin:/bin', HOME=str(cwd), JAVA_HOME=os.environ.get(
        'JAVA_HOME', '/Applications/Android Studio.app/Contents/jbr/Contents/Home'))
    result = subprocess.run([str(Path(tmp) / root / 'bin/marathon'), 'version'], cwd=cwd,
                            env=env, capture_output=True, text=True, timeout=60, check=True)
    if result.stdout.splitlines()[-1] != a.version:
        raise ValueError('reference embedded version mismatch')
    print(json.dumps(dict(source='e47352f25cbc50333900d5d040a5544ff1224501', release_mode='RELEASE',
                          version=a.version, gradle='8.10.1', archive=a.archive.name,
                          archive_sha256=hashlib.sha256(a.archive.read_bytes()).hexdigest(),
                          directories=[dict(path=i.filename, mode=oct(i.external_attr >> 16)) for i in z.infolist() if i.is_dir()],
                          jar_modes=sorted({oct(i.external_attr >> 16) for i in z.infolist() if i.filename.endswith('.jar')}),
                          entries=len(z.infolist()), jars=sum(i.filename.endswith('.jar') for i in z.infolist()),
                          launchers=launchers, invocation=dict(argv=['<extracted>/bin/marathon', 'version'],
                          cwd='<unrelated directory with spaces>', exit=result.returncode,
                          final_stdout_line=result.stdout.splitlines()[-1],
                          startup_info_before_version='Starting marathon v' + a.version in result.stdout.splitlines()[0])), indent=2))
