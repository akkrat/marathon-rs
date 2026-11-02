#!/usr/bin/env python3
"""Compile the pinned Kotlin generator and Apple adapter in a disposable directory.
Usage: tools/oracle/xcresult/run.py [KOTLIN_REPO] [OUTPUT.jsonl]
models.json is written next to OUTPUT.jsonl.
All dependencies come from the local Gradle cache. No reference checkout is modified.
"""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[2]
SOURCE = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(os.path.expanduser('~/src/marathon'))
OUTPUT = Path(sys.argv[2]) if len(sys.argv) > 2 else REPO / 'fixtures/oracle/xcresult/corpus.jsonl'
if not subprocess.check_output(['git', '-C', str(SOURCE), 'rev-parse', 'HEAD'], text=True).startswith('e47352f2'):
    sys.exit('xcresult oracle requires reference e47352f2')
if (SOURCE / 'vendor/vendor-apple/base/src/main/resources/xcresulttool-schema.json').read_bytes() != (REPO / 'fixtures/xcresult/schema-3.39.json').read_bytes():
    sys.exit('checked-in xcresult schema differs from the reference')
CACHE = Path.home() / '.gradle/caches/modules-2/files-2.1'
JAVA = str(Path(os.environ.get('JAVA_HOME', '/Applications/Android Studio.app/Contents/jbr/Contents/Home')) / 'bin/java')


def jar(group, artifact, version):
    return str(next((CACHE / group / artifact / version).glob('*/*.jar')))


compiler = [jar('org.jetbrains.kotlin', 'kotlin-compiler-embeddable', '1.9.24'),
            jar('org.jetbrains.kotlin', 'kotlin-stdlib', '1.9.24'),
            jar('org.jetbrains.kotlin', 'kotlin-script-runtime', '1.9.24'),
            jar('org.jetbrains.kotlin', 'kotlin-reflect', '1.6.10'),
            jar('org.jetbrains.intellij.deps', 'trove4j', '1.0.20200330'),
            jar('org.jetbrains', 'annotations', '13.0')]
libs = [jar('org.jetbrains.kotlin', 'kotlin-stdlib', '1.9.24'),
        jar('org.jetbrains.kotlin', 'kotlin-reflect', '1.9.24'),
        jar('org.jetbrains', 'annotations', '13.0'),
        jar('com.squareup', 'kotlinpoet-jvm', '1.16.0'),
        jar('com.google.code.gson', 'gson', '2.10.1'),
        jar('org.jetbrains.kotlin', 'kotlin-gradle-plugin', '1.9.24')]
libs += [jar('com.fasterxml.jackson.core', a, '2.17.2') for a in ['jackson-core', 'jackson-databind', 'jackson-annotations']]
libs += [jar('com.fasterxml.jackson.module', 'jackson-module-kotlin', '2.17.2')]


def run(args, **kw):
    subprocess.run(args, check=True, **kw)


def compile_kt(sources, dest, extra=()):
    run([JAVA, '-cp', os.pathsep.join(compiler), 'org.jetbrains.kotlin.cli.jvm.K2JVMCompiler',
         '-no-stdlib', '-no-reflect', '-nowarn', '-jvm-target', '11', '-cp', os.pathsep.join(libs + list(extra)),
         '-d', str(dest), *map(str, sources)])


with tempfile.TemporaryDirectory(prefix='mrs-xcresult-') as directory:
    tmp = Path(directory)
    base = SOURCE / 'buildSrc/src/main/kotlin/com/malinskiy/marathon/buildsystem'
    sources = [base / 'XcresulttoolSourceCreator.kt', *(base / 'xcresulttool').glob('*.kt')]
    bootstrap = tmp / 'Generate.kt'
    bootstrap.write_text('''import com.malinskiy.marathon.buildsystem.XcresulttoolSourceCreator
import java.io.File
fun main(args: Array<String>) { XcresulttoolSourceCreator(File(args[0]), "com.malinskiy.marathon.vendor.ios.xcrun.xcresulttool").createKotlinSource(File(args[1])) }
''')
    compile_kt([*sources, bootstrap], tmp / 'generator.jar')
    schema = json.loads((REPO / 'fixtures/xcresult/schema-3.39.json').read_text())
    # This synthetic class exercises a constructor shape absent from the pinned schema.
    schema['types'].append({'type': {'name': 'OracleNullableLists'}, 'kind': 'object', 'properties': [
        {'name': 'strings', 'type': 'Array', 'wrappedType': 'String', 'isOptional': True, 'isInternal': False},
        {'name': 'objects', 'type': 'Array', 'wrappedType': 'Reference', 'isOptional': True, 'isInternal': False}]})
    (tmp / 'schema.json').write_text(json.dumps(schema))
    run([JAVA, '-cp', os.pathsep.join([str(tmp / 'generator.jar'), *libs]), 'GenerateKt',
         str(tmp / 'schema.json'), str(tmp / 'models')], stdout=subprocess.DEVNULL)
    adapter = SOURCE / 'vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/bin/xcrun/xcresulttool/AppleJsonTypeAdapterFactory.kt'
    # Copy the exact mapper setup, avoiding the command executor and tool runtime.
    wrapper = (adapter.parent / 'Xcresulttool.kt').read_text()
    setup = wrapper.split('val AppleJsonMapper: ObjectMapper = ObjectMapper().apply {', 1)[1].split('\n        fun describeTimeout', 1)[0]
    (tmp / 'Mapper.kt').write_text('''import com.fasterxml.jackson.databind.*
import com.fasterxml.jackson.module.kotlin.*
import com.malinskiy.marathon.apple.bin.xcrun.xcresulttool.AppleModule
val AppleJsonMapper: ObjectMapper = ObjectMapper().apply {''' + setup)
    compile_kt([*sorted((tmp / 'models').rglob('*.kt')), adapter, tmp / 'Mapper.kt', HERE / 'Probe.kt'], tmp / 'probe.jar')
    OUTPUT.parent.mkdir(parents=True, exist_ok=True)
    with OUTPUT.open('w') as output:
        run([JAVA, '-cp', os.pathsep.join([str(tmp / 'probe.jar'), *libs]), 'ProbeKt', str(REPO), str(OUTPUT.parent / 'models.json')], stdout=output)
print(OUTPUT)
