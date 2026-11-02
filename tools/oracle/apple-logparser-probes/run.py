#!/usr/bin/env python3
"""WP 4.7: compile unchanged Kotlin parsers/listeners against installed jars and emit JSONL witnesses.
Usage: python3 tools/oracle/apple-logparser-probes/run.py [output]
"""
import json, os, pathlib, re, subprocess, sys, tempfile

here = pathlib.Path(__file__).resolve().parent
repo = here.parents[2]
source = pathlib.Path(os.environ.get('MARATHON_KT', os.path.expanduser('~/src/marathon')))
lib = pathlib.Path(os.environ.get('MARATHON_KOTLIN_LIB', str(source / 'cli/build/install/marathon/lib')))
cache = pathlib.Path.home() / '.gradle/caches/modules-2/files-2.1'
def jar(g, a, v): return next((cache / g / a / v).glob('*/*.jar'))
compiler = [jar('org.jetbrains.kotlin', 'kotlin-compiler-embeddable', '1.9.24'), jar('org.jetbrains.kotlin', 'kotlin-stdlib', '1.9.24'),
            jar('org.jetbrains.kotlin', 'kotlin-script-runtime', '1.9.24'), jar('org.jetbrains.kotlin', 'kotlin-reflect', '1.6.10'),
            jar('org.jetbrains.intellij.deps', 'trove4j', '1.0.20200330'), jar('org.jetbrains', 'annotations', '13.0')]
java = str(pathlib.Path(os.environ.get('JAVA_HOME', '/Applications/Android Studio.app/Contents/jbr/Contents/Home')) / 'bin/java')
cp = os.pathsep.join(map(str, sorted(lib.glob('*.jar'))))
base = source / 'vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple'
files = [str(base / p) for p in [
    'test/TestEvent.kt','logparser/TestEventProducer.kt','logparser/XctestEventProducer.kt',
    'logparser/parser/DeviceFailureParser.kt','logparser/parser/DeviceFailureException.kt',
    'logparser/parser/TestRunProgressParser.kt','logparser/parser/DebugLogPrinter.kt',
    'logparser/parser/DiagnosticLogsPathFinder.kt','logparser/parser/SessionResultsPathFinder.kt',
    'listener/AppleTestRunListener.kt','listener/CompositeTestRunListener.kt',
    'listener/AccumulatingTestResultListener.kt','listener/TestResultsListener.kt',
    'listener/TestRunListenerAdapter.kt','listener/DebugTestRunListener.kt','listener/ArtifactFinderListener.kt']]

out = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else repo / 'fixtures/oracle/apple-logparser/probes.jsonl'

with tempfile.TemporaryDirectory(prefix='mrs47-probe-') as tmp:
    classes = pathlib.Path(tmp) / 'classes'
    subprocess.run([java, '-cp', os.pathsep.join(map(str, compiler)), 'org.jetbrains.kotlin.cli.jvm.K2JVMCompiler', '-no-stdlib', '-no-reflect',
                    '-nowarn', '-jvm-target', '17', '-cp', cp, '-d', str(classes), str(here / 'Probe.kt'), *files], check=True)
    result = subprocess.run([java, '-cp', str(classes) + os.pathsep + cp, 'ProbeKt', str(repo / 'fixtures')],
                            check=True, stdout=subprocess.PIPE, timeout=120)
    lines = [line for line in result.stdout.decode().split('\n') if line.startswith('{"case"')]
    # Kotlin measures enhancement with the host clock, independently of the fixed Timer.
    rows = [json.loads(line) for line in lines]
    for row in rows:
        if row['kind'] == 'projection':
            for log in row.get('logs', []):
                log['message'] = re.sub(r'(Enhancing report using xcresult took )-?\d+( ms)', r'\1<elapsed>\2', log['message'])
    lines = [json.dumps(row, ensure_ascii=False, separators=(',', ':')) for row in rows]
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text('\n'.join(lines) + '\n')
    print(f'{len(lines)} cases -> {out}')
