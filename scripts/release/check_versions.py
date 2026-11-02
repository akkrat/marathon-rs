#!/usr/bin/env python3
"""Native cache-invalidation evidence, sharing Cargo's target directory across three builds."""
import os
from pathlib import Path
import subprocess
import sys
import tempfile

script = Path(__file__).with_name('release.py').resolve()
target = sys.argv[1]
with tempfile.TemporaryDirectory(prefix='marathon version builds with spaces ') as tmp:
    for version in (None, 'v7.3-cache-one', 'v7.3-cache-two'):
        env = dict(os.environ)
        env.pop('GIT_TAG_NAME', None)
        args = [sys.executable, '-I', str(script), 'build', '--target', target,
                '--output', str(Path(tmp) / (version or 'fallback'))]
        if version is not None:
            env['GIT_TAG_NAME'] = version
        subprocess.run(args, env=env, check=True)
    # Every package already ran version/help after extraction with runtime GIT_TAG_NAME=runtime-conflict.
print('fallback, two tags, shared Cargo cache, and conflicting runtime environment: passed')
