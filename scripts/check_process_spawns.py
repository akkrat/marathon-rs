#!/usr/bin/env python3
"""Keep local spawn APIs at the process ownership boundary."""
from pathlib import Path
import re
import sys


ROOT = Path(__file__).resolve().parents[1]
EXEMPT = {"marathon-process", "marathon-test-support"}
NATIVE_SPAWN = r'(?:fork|vfork|posix_spawnp?|exec[lv](?:p|e|pe)?|execveat|fexecve|system|popen)'
RAW_SPAWN = re.compile(
    r'\bprocess\s*::\s*(?:Command\b|\*|\{[^}]*?(?:\bCommand\b|\*))'
    r'|\b(?:std|tokio)\s*::\s*(?:\{[^}]*?)?\bprocess\s+as\b'
    rf'|\b(?:libc|unistd|spawn)\s*::\s*(?:\{{[^}}]*?)?\b{NATIVE_SPAWN}\b'
)
GLOB_USE = re.compile(r'\buse\s+[^;]*?\b(?:libc|unistd|spawn)\s*::\s*(?:\*|\{[^};]*\*)')
UNQUALIFIED_SPAWN = re.compile(rf'(?<![\w.:])\b{NATIVE_SPAWN}\s*\(')
LITERAL_OR_COMMENT = re.compile(
    r'//[^\n]*|/\*'
    r'|\br(?P<hash>#{0,255})".*?"(?P=hash)|\b[bc]r(?P<bytes>#{0,255})".*?"(?P=bytes)'
    r'|"(?:[^"\\]|\\.)*"'
    r"|'(?:\\(?:u\{[0-9a-fA-F_]+\}|x[0-9a-fA-F]{2}|.)|[^'\\\n])'",
    re.DOTALL,
)
BLOCK_DELIMITER = re.compile(r'/\*|\*/')


def blank_non_code(text):
    """Mask comment and string contents while preserving diagnostic positions."""
    parts, pos = [], 0
    while match := LITERAL_OR_COMMENT.search(text, pos):
        parts.append(text[pos:match.start()])
        end = match.end()
        if match[0] == '/*':
            depth = 1
            for delimiter in BLOCK_DELIMITER.finditer(text, end):
                depth += 1 if delimiter[0] == '/*' else -1
                if depth == 0:
                    end = delimiter.end()
                    break
            else:
                end = len(text)
        parts.append(re.sub(r'[^\n]', ' ', text[match.start():end]))
        pos = end
    parts.append(text[pos:])
    return ''.join(parts)


def spawn_lines(text):
    code = blank_non_code(text)
    matches = list(RAW_SPAWN.finditer(code))
    if GLOB_USE.search(code):
        matches.extend(UNQUALIFIED_SPAWN.finditer(code))
    return sorted({code.count("\n", 0, match.start()) + 1 for match in matches})


def main():
    bad = False
    for source in sorted((ROOT / "crates").glob("*/src/**/*.rs")):
        relative = source.relative_to(ROOT / "crates")
        if relative.parts[0] in EXEMPT:
            continue
        for line in spawn_lines(source.read_text(encoding="utf-8")):
            print(f"crates/{relative}:{line}: use marathon_process::Command")
            bad = True
    return bad


if __name__ == "__main__":
    sys.exit(main())
