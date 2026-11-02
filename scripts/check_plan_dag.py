#!/usr/bin/env python3
"""Check the work-package table in PLAN.md §7: every dependency names an existing WP, there are no cycles,
and print the effort-weighted critical path (L=1, M=2, H=3; Phase 0 exit barrier applied to Phases 2-6)."""
import functools
import pathlib
import re
import sys

plan = (pathlib.Path(__file__).resolve().parent.parent / "PLAN.md").read_text()
sec = plan[plan.index("## 7. Work packages"):plan.index("## 8. Agent workflow")]
rows = re.findall(r"^\| (\d\.\d+[a-c]?) \|.*\| ([LMH–-]) \| ([^|]*) \|$", sec, re.M)
ids = {r[0] for r in rows}
effort = {r[0]: {"L": 1, "M": 2, "H": 3}.get(r[1], 1) for r in rows}


def expand(dep: str) -> set:
    dep = dep.strip()
    if dep in ("–", "-", ""):
        return set()
    if dep.startswith("every WP except"):
        return ids - {"7.5", "7.6"}
    out = set()
    for tok in re.split(r",\s*", dep):
        m = re.match(r"(\d\.\d+)([a-c])–([a-c])$", tok)
        if m:
            out |= {m.group(1) + c for c in "abc"["abc".index(m.group(2)):"abc".index(m.group(3)) + 1]}
        elif tok in ("5.x", "6.x"):
            out |= {i for i in ids if i.startswith(tok[0] + ".")}
        else:
            out.add(tok)
    return out


graph = {wp: expand(dep) for wp, _, dep in rows}
phase0 = {i for i in ids if i.startswith("0.")}
for wp in graph:
    if wp[0] in "23456":
        graph[wp] |= phase0

errors = sorted(f"{wp} -> {d}" for wp, ds in graph.items() for d in ds if d not in ids)
state, cycles = {}, []


def dfs(n, path):
    if state.get(n) == 1:
        cycles.append(path + [n])
        return
    if state.get(n) == 2:
        return
    state[n] = 1
    for d in graph.get(n, ()):
        dfs(d, path + [n])
    state[n] = 2


for wp in graph:
    dfs(wp, [])


@functools.lru_cache(None)
def longest(n):
    # sorted(): set order depends on string hashing, so ties would otherwise break differently per run.
    best = max((longest(d) for d in sorted(graph[n])), default=(0, ()), key=lambda x: x[0])
    return best[0] + effort[n], best[1] + (n,)


print(f"{len(ids)} work packages")
for e in errors:
    print("UNKNOWN DEPENDENCY", e)
for c in cycles:
    print("CYCLE", " -> ".join(c))
if not errors and not cycles:
    weight, path = longest("7.6")
    print("critical path:", " -> ".join(path), f"(weight {weight})")
sys.exit(1 if errors or cycles else 0)
