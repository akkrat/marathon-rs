#!/usr/bin/env python3
"""Offline helpers of the WP 7.5 closure sweep (stdlib only; run as `python3 -I plan.py ...`).

The scenario table lives in plan.json; host-specific values only ever come from the git-ignored host config.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import sys

PLAN = Path(__file__).resolve().parent / 'plan.json'
NAME = re.compile(r'(?:(?P<pkg>[\w.$]+)\.)?(?P<cls>[\w$]+)#(?P<method>\w+)')


class Stop(Exception):
    pass


def need(ok, message):
    if not ok:
        raise Stop(message)


def load(path):
    return json.loads(Path(path).read_text())


def plan():
    return load(PLAN)


def scenario(name):
    table = plan()['scenarios']
    need(name in table, 'unknown scenario: ' + name)
    return table[name]


def get(name, key):
    return scenario(name)[key]


def scenarios(profile):
    return [n for n, v in plan()['scenarios'].items() if v['profile'] == profile]


def builds(profile):
    table = plan()['profiles']
    need(profile in table, 'unknown profile: ' + profile)
    return [(d, v[0], v[1]) for d, v in table[profile]['builds'].items()]


def write_private(path, value):
    path = Path(path)
    need(not path.exists(), 'exists, retained: ' + str(path))
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, 'w') as out:
        json.dump(value, out, indent=2)
        out.write('\n')


def selection(old, inputs, review):
    """The 7.2 selection re-bound to the release discovery; every selected identity must be listed in it."""
    inputs = Path(inputs)
    discovery = (inputs / 'discovery.yml').read_bytes()
    text = discovery.decode('utf-8', 'replace')
    value = load(old)
    need(review.strip() and not review.startswith('SET_'), 'a review text naming what was compared is required')
    for test in value['tests']:
        m = NAME.fullmatch(test)
        need(m, 'unparsable test name: ' + test)
        for part in (m['pkg'], m['cls'], m['method']):
            need(part is None or part in text, 'the release discovery does not list ' + test)
    value['discovery_sha256'] = hashlib.sha256(discovery).hexdigest()
    value['review'] = review
    write_private(inputs / 'selection.json', value)


def draft(old, out, review):
    """The 7.2 r3 policy as a draft: no stale qualification (jvm_runs, fingerprint, rule evidence); qualify writes a new policy."""
    value = load(old)
    need(review.strip() and not review.startswith('SET_'), 'a review text is required')
    for key in ('jvm_runs', 'projection_sha256'):
        value.pop(key, None)
    for rule in value.get('rules', []):
        rule.pop('evidence', None)
    value['review'] = review
    write_private(out, value)


def host_config(base, wk, out):
    """The worker's host config for the sweep: every path absolute under WK, everything else from the 7.2 config."""
    wk = Path(wk)
    need(wk.is_absolute(), 'WK must be an absolute path')
    value = load(base)
    value.update(kotlin_source=str(wk / 'kotlin-src'), jvm=str(wk / 'jvm/marathon/bin/marathon'),
                 jvm_reference=str(wk / 'jvm/reference.json'), native=str(wk / 'target/release/marathon'),
                 worker_checkout=str(wk / 'checkout'))
    write_private(out, value)


def check_build(path, source):
    build = load(path)
    need(build.get('source') == source and build.get('source_kind') == 'release', 'the build is not bound to the release source')
    samples = build.get('samples') or {}
    need(samples.get('matches_frozen') is True and samples.get('paths'), 'the build does not prove its sample trees')
    return build


def recorded_native(root):
    """The sweep's native commit as the build records of this runs root name it.

    HEAD moves with later commits (offline fixes, findings), so the audit compares the captures with what the
    sweep itself recorded: every `<root>/*-build/build.json` carries `native_source`, and they must all agree.
    """
    found = {}
    for f in sorted(Path(root).glob('*-build/build.json')):
        value = load(f).get('native_source')
        need(isinstance(value, str) and re.fullmatch(r'[0-9a-f]{40}', value), 'no native commit in the build record ' + str(f))
        found[str(f.relative_to(root))] = value
    need(found, 'no build record under ' + str(root))
    need(len(set(found.values())) == 1, 'the build records name different native commits: ' + ', '.join(sorted(set(found.values()))))
    return next(iter(found.values()))


def audit(root, source, native=None):
    """Every capture names the release source, the JVM reference and the sweep's recorded native commit."""
    root, bad, seen = Path(root), [], 0
    recorded = recorded_native(root)
    need(native is None or native == recorded, 'the build records name native commit %s, not %s' % (recorded, native))
    for f in sorted(root.glob('*-pair-*/*/capture.json')):
        side = f.parent.name
        if side not in ('jvm', 'native'):
            continue
        seen += 1
        c = load(f)
        ok = (c.get('source') == source and c.get('source_kind') == 'release' and c.get('jvm_reference_source') == source
              and c.get('native_source') == recorded)
        try:
            b = load(f.parent / 'in/build.json')
            ok = ok and b.get('source') == source and b.get('source_kind') == 'release' and (b.get('samples') or {}).get('matches_frozen') is True
            ok = ok and b.get('native_source') == recorded
        except (OSError, ValueError):
            ok = False
        print(('ok  ' if ok else 'BAD ') + str(f.relative_to(root)))
        if not ok:
            bad.append(f)
    for f in sorted(root.glob('*-pair-*/release*.json')):
        print('released ' + str(f.relative_to(root)))
    print('native commit (from the build records): ' + recorded)
    need(seen, 'no captures under ' + str(root))
    need(not bad, '%d capture(s) are not bound to the release source and the recorded native commit' % len(bad))


def main(argv):
    p = argparse.ArgumentParser()
    sub = p.add_subparsers(dest='cmd', required=True)
    g = sub.add_parser('get'); g.add_argument('scenario'); g.add_argument('key')
    s = sub.add_parser('scenarios'); s.add_argument('profile')
    b = sub.add_parser('builds'); b.add_argument('profile')
    h = sub.add_parser('profile'); h.add_argument('host')
    hg = sub.add_parser('host-get'); hg.add_argument('host'); hg.add_argument('key')
    sel = sub.add_parser('selection'); [sel.add_argument('--' + k, required=True) for k in ('old', 'inputs', 'review')]
    d = sub.add_parser('draft'); [d.add_argument('--' + k, required=True) for k in ('old', 'out', 'review')]
    hc = sub.add_parser('host-config'); [hc.add_argument('--' + k, required=True) for k in ('base', 'wk', 'out')]
    cb = sub.add_parser('check-build'); cb.add_argument('build'); cb.add_argument('source')
    a = sub.add_parser('audit'); a.add_argument('--root', required=True); a.add_argument('--source', required=True); a.add_argument('--native')
    args = p.parse_args(argv)
    try:
        if args.cmd == 'get':
            print(get(args.scenario, args.key))
        elif args.cmd == 'scenarios':
            print('\n'.join(scenarios(args.profile)))
        elif args.cmd == 'builds':
            print('\n'.join(' '.join(row) for row in builds(args.profile)))
        elif args.cmd == 'profile':
            print(load(args.host)['profile'])
        elif args.cmd == 'host-get':
            print(load(args.host)[args.key])
        elif args.cmd == 'selection':
            selection(args.old, args.inputs, args.review)
        elif args.cmd == 'draft':
            draft(args.old, args.out, args.review)
        elif args.cmd == 'host-config':
            host_config(args.base, args.wk, args.out)
        elif args.cmd == 'check-build':
            check_build(args.build, args.source)
        else:
            audit(args.root, args.source, args.native)
    except (Stop, OSError, ValueError, KeyError) as error:
        print('sweep: ' + str(error), file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))
