#!/usr/bin/env python3
"""Keep local spawn APIs at the process ownership boundary."""
from pathlib import Path
import re
import sys


ROOT = Path(__file__).resolve().parents[1]
EXEMPT = {"marathon-process", "marathon-test-support"}
RAW_SPAWN = re.compile(
    r'\bprocess\s*::\s*(?:Command\b|\{[^}]*\bCommand\b)'
    r'|\buse\s+(?:std|tokio)\s*::\s*process\s+as\b'
    r'|\b(?:libc|unistd|spawn)\s*::\s*(?:fork|vfork|posix_spawnp?|execv[ep]?)\b'
)
bad = False
for source in sorted((ROOT / "crates").glob("*/src/**/*.rs")):
    if source.relative_to(ROOT / "crates").parts[0] in EXEMPT:
        continue
    code = source.read_text(encoding="utf-8")
    for match in RAW_SPAWN.finditer(code):
        print(f"{source.relative_to(ROOT)}:{code.count(chr(10), 0, match.start()) + 1}: use marathon_process::Command")
        bad = True
sys.exit(bad)
