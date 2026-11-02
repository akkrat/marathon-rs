#!/usr/bin/env python3
"""Record original argv before exec; fixture-only authentication and known_hosts."""
import json
import os
import pathlib
import re
import sys

name = sys.argv[1]
args = sys.argv[2:]
real = pathlib.Path(os.environ['FW_REAL_' + name.upper()]).resolve(strict=True)
if not real.is_absolute() or real.parent == pathlib.Path(__file__).resolve().parent:
    raise SystemExit('real binary must be an explicit non-shim path')
if name == 'sshpass' and ('-p' not in args or args[args.index('-p') + 1] not in ('fake-password', 'wrong-fixture-password')):
    raise SystemExit('only labelled fixture passwords may be recorded')
extra = []
if name == 'ssh':
    protected = {'userknownhostsfile', 'globalknownhostsfile', 'updatehostkeys', 'identityagent', 'identitiesonly'}
    for index, arg in enumerate(args):
        option = args[index + 1] if arg == '-o' and index + 1 < len(args) else arg[2:] if arg.startswith('-o') else ''
        if re.split(r'[=\s]', option.strip(), maxsplit=1)[0].lower() in protected:
            raise SystemExit('caller must not override harness SSH isolation: ' + option)
    extra = ['-o', 'UserKnownHostsFile=/dev/null', '-o', 'GlobalKnownHostsFile=/dev/null', '-o', 'UpdateHostKeys=no', '-o', 'IdentityAgent=none', '-o', 'IdentitiesOnly=yes']
harness_env = {}
if name == 'ssh' and os.environ.get('FW_USE_FIXTURE_ASKPASS') == '1':
    harness_env = {'SSH_ASKPASS': str(pathlib.Path(__file__).resolve().parent / 'fixture-askpass'), 'SSH_ASKPASS_REQUIRE': 'force', 'DISPLAY': 'fixture:0'}
    os.environ.update(harness_env)
row = {'schema': 1, 'event': 'host_argv', 'program': name, 'argv': args, 'harness_options': extra, 'harness_env': harness_env}
fd = os.open(os.environ['FW_ARGV_LOG'], os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
try:
    os.write(fd, (json.dumps(row, separators=(',', ':')) + '\n').encode())
finally:
    os.close(fd)
os.execv(str(real), [str(real), *extra, *args])
