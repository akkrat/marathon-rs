#!/usr/bin/env python3
"""WP 3.10: run the scenario inputs through the reference build's `oracle scenario` driver and condense the transcripts.

Usage: tools/oracle/run-flow-probes/capture_scenarios.py [OUT]   (default fixtures/oracle/run-flow/scenarios.jsonl)
Needs `tools/oracle/build.sh reference`, and `patched` for the TAPED scenarios: their `entry: run` exercises the hooked
`runBlocking` bridge, so they run with the patched build and tools/oracle/scenario-tape.json. One row per scenario: the outcome (completed, value or exception, completedAt),
the event/device-call log with batch UUIDs renamed batch-1, batch-2... by first appearance (the reference build draws
random UUIDs), raw.json's exact text, the parse output files, the outputDir manifest (directories end in '/') and the text of the
Allure and HTML reporter outputs other than their static assets.
"""
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path

here = Path(__file__).resolve().parent
oracle = here.parent
repo = oracle.parents[1]
SCENARIOS = ['success', 'parse', 'filtering-1', 'filtering-2', 'filtering-3', 'disconnect', 'all-lost', 'invalid-config',
             'two-pools-disconnect', 'uncompleted-1', 'uncompleted-2', 'uncompleted-3', 'uncompleted-4']
TAPED = ['scripted', 'scripted-failures']


def manifest(root):
    entries = []
    for path in sorted(root.rglob('*')):
        rel = path.relative_to(root).as_posix()
        entries.append(rel + '/' if path.is_dir() else rel)
    return entries


# Copied unchanged from the reporter resources; WP 3.9c pins their bytes.
STATIC = ('html/app.min.css', 'html/app.min.js', 'html/app.min.js.LICENSE.txt', 'html/timeline/chart.css',
          'html/timeline/chart.js', 'html/timeline/iframeResizer.contentWindow.min.js')


def reporter_files(root):
    files = {}
    for path in sorted(root.rglob('*')):
        rel = path.relative_to(root).as_posix()
        if path.is_file() and rel.startswith(('allure-results/', 'html/')) and rel not in STATIC:
            files[rel] = path.read_text()
    return files


def condense(log):
    batches = {}
    out = []
    for entry in log:
        if 'subscribed' in entry or 'runBlocking' in entry:
            continue
        entry = dict(entry)
        if 'batch' in entry:
            entry['batch'] = batches.setdefault(entry['batch'], f'batch-{len(batches) + 1}')
        out.append(entry)
    return out


def main():
    out = Path(sys.argv[1]) if len(sys.argv) > 1 else repo / 'fixtures/oracle/run-flow/scenarios.jsonl'
    env = dict(os.environ)
    env.setdefault('JAVA_HOME', '/Applications/Android Studio.app/Contents/jbr/Contents/Home')
    rows = [{'schema': 'run-flow/scenarios@1', 'source': 'tools/oracle/run-flow-probes/capture_scenarios.py',
             'build': 'reference e47352f2 + 0001', 'driver': 'oracle scenario (replay driver, no tape)'}]
    with tempfile.TemporaryDirectory(prefix='run-flow-scenarios-') as tmp:
        for name in SCENARIOS + TAPED:
            work = Path(tmp) / name
            binary = oracle / 'oracle'
            run_env = env
            if name in TAPED:
                binary = oracle / 'build/marathon-patched/oracle-build/install/oracle/bin/oracle'
                scratch = Path(tmp) / f'{name}-tmp'
                scratch.mkdir()
                run_env = dict(env, JAVA_OPTS=f'-Doracle.hooks.tape={oracle / "scenario-tape.json"} -Djava.io.tmpdir={scratch}')
            proc = subprocess.run([str(binary), 'scenario', str(oracle / 'scenarios' / f'{name}.json'), str(work)],
                                  env=run_env, capture_output=True, text=True)
            transcript = json.loads((work / 'transcript.json').read_text())
            if proc.returncode != 0 or transcript['errors']:
                sys.exit(f'{name}: exit {proc.returncode}, errors {transcript["errors"]}\n{proc.stderr[-2000:]}')
            output = work / 'output'
            raw = output / 'test_result/raw.json'
            files = {p.name: p.read_text() for p in sorted(output.glob('*.yml'))} if output.is_dir() else {}
            rows.append({
                'scenario': name,
                'taped': name in TAPED,
                'outcome': transcript['outcome'],
                'log': condense(transcript['log']),
                'rawJson': raw.read_text() if raw.is_file() else None,
                'files': files,
                'manifest': manifest(output) if output.is_dir() else None,
                'reporterFiles': reporter_files(output) if output.is_dir() else {},
            })
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(''.join(json.dumps(r, ensure_ascii=False, separators=(',', ':')) + '\n' for r in rows))


if __name__ == '__main__':
    main()
