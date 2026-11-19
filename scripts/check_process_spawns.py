#!/usr/bin/env python3
"""Keep local spawn APIs at the process ownership boundary."""
from pathlib import Path
import re
import sys


ROOT = Path(__file__).resolve().parents[1]
EXEMPT = {"marathon-process", "marathon-test-support"}
NATIVE_SPAWN = r'(?:fork|vfork|posix_spawnp?|exec\w*|system|popen)'
RAW_SPAWN = re.compile(
    r'\bprocess\s*::\s*(?:Command\b|\*|\{[^}]*?(?:\bCommand\b|\*))'
    r'|\b(?:std|tokio)\s*::\s*(?:\{[^}]*?)?\bprocess\s+as\b'
    rf'|\b(?:libc|unistd|spawn)\s*::\s*(?:\{{[^}}]*?)?\b{NATIVE_SPAWN}\b'
)


def main():
    bad = False
    for source in sorted((ROOT / "crates").glob("*/src/**/*.rs")):
        relative = source.relative_to(ROOT / "crates")
        if relative.parts[0] in EXEMPT:
            continue
        code = source.read_text(encoding="utf-8")
        for match in RAW_SPAWN.finditer(code):
            line = code.count("\n", 0, match.start()) + 1
            print(f"crates/{relative}:{line}: use marathon_process::Command")
            bad = True
    return bad


if __name__ == "__main__":
    sys.exit(main())
