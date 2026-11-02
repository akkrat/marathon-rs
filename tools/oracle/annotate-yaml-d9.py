#!/usr/bin/env python3
"""Retain JVM truth; annotate only target-specific D9 policy assertions, never skip rows."""
import decimal
import json
import pathlib

root = pathlib.Path(__file__).resolve().parents[2]
for name in ("yaml-corpus", "yaml-review-corpus", "yaml-round3-corpus"):
    path = root / "fixtures/oracle" / (name + ".jsonl")
    lines = []
    for line in path.read_text().split("\n"):
        if not line:
            continue
        row = json.loads(line)
        row.pop("binding_expected_fail", None)
        rules = {}
        ident = row.get("id", "")
        if ident in ("B-quoted-0", "B-quoted-1", "B-quoted-2", "B-quoted-3", "R3-sol-3-arabic"):
            rules.update({"Int": "reject", "Long": "reject"})
        text = row.get("input", "".join(s * n for s, n in row.get("input_gen", [])))
        if isinstance(text, str) and text.startswith("k: "):
            try:
                number = decimal.Decimal(text[3:].strip())
                overflow = number.is_finite() and (number >= 2**63 or number < -(2**63))
            except decimal.InvalidOperation:
                overflow = False
            if overflow:
                for target in ("Instant?", "Duration?"):
                    if "ok" in row.get("bind", {}).get(target, {}):
                        rules[target] = "reject"
        if ident in ("R3-sol-8-empty", "R3-sol-8-blank"):
            rules.update({target: "error-class" for target in row["bind"]})
        rules.update({
            "R3-sol-2-null-string": {"String": "last-string", "List<String>": "reject"},
            "R3-sol-2-string-null": {"String": "last-null"},
            "R3-sol-2-null-list": {"String": "reject", "List<String>": "last-list"},
            "R3-sol-2-list-null": {"List<String>": "last-null"},
            "R3-sol-2-parse-order": {"String": "syntax-before-null", "List<String>": "syntax-before-null"},
        }.get(ident, {}))
        row.pop("d9", None)
        if rules:
            row["d9"] = rules
        # Preserve the original byte spelling on unaffected rows.
        lines.append(line if row == json.loads(line) else json.dumps(row, ensure_ascii=False, separators=(",", ":")))
    path.write_text("\n".join(lines) + "\n")
