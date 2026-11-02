#!/usr/bin/env python3
"""Release-pinned Kotlin tests plus return-only observation; no expected values from Rust."""
import hashlib, json, os, pathlib, subprocess, sys, tempfile
HERE = pathlib.Path(__file__).resolve().parent
ROOT = HERE.parents[2]
SHA = 'c95b212882c7ba7b1ad91b7981916ed175f1530e'
SOURCE = pathlib.Path(os.environ.get('MARATHON_KT', '/tmp/mrs-7.5-src'))
LIB = ROOT / f'tools/oracle/build-{SHA}/marathon-src/cli/build/install/marathon/lib'
assert subprocess.check_output(['git', '-C', str(SOURCE), 'rev-parse', 'HEAD'], text=True).strip() == SHA
cache = pathlib.Path.home()/'.gradle/caches/modules-2/files-2.1'
def jar(group, artifact, version): return next((cache/group/artifact/version).glob('*/*.jar'))
compiler = [jar(*v) for v in [('org.jetbrains.kotlin','kotlin-compiler-embeddable','1.9.24'),('org.jetbrains.kotlin','kotlin-stdlib','1.9.24'),('org.jetbrains.kotlin','kotlin-script-runtime','1.9.24'),('org.jetbrains.kotlin','kotlin-reflect','1.6.10'),('org.jetbrains.intellij.deps','trove4j','1.0.20200330'),('org.jetbrains','annotations','13.0')]]
extra = [jar(*v) for v in [('org.amshove.kluent','kluent','1.73'),('org.junit.jupiter','junit-jupiter-api','5.11.0'),('org.opentest4j','opentest4j','1.3.0'),('org.junit.platform','junit-platform-commons','1.11.0'),('org.apiguardian','apiguardian-api','1.1.2')]]
java = str(pathlib.Path(os.environ.get('JAVA_HOME','/Applications/Android Studio.app/Contents/jbr/Contents/Home'))/'bin/java')
cp = os.pathsep.join(map(str, [*sorted(LIB.glob('*.jar')), *extra]))
base = pathlib.Path('vendor/vendor-apple/base/src')
production = base/'main/kotlin/com/malinskiy/marathon/apple/xctestrun'
test = base/'test/kotlin/com/malinskiy/marathon/apple/xctestrun/CodeCoverageTargetsTest.kt'
files = [production/'CodeCoverageTargets.kt', production/'MachO.kt', test]
for path in files:
    assert (SOURCE/path).read_bytes() == subprocess.check_output(['git','-C',str(SOURCE),'show',SHA+':'+str(path)])
sha256 = lambda data: hashlib.sha256(data).hexdigest()
meta = dict(source_sha=SHA, source_files={str(p):sha256((SOURCE/p).read_bytes()) for p in files}, recorder_sha256=sha256((HERE/'Recorder.kt').read_bytes()), jdk=subprocess.check_output([java,'-version'],stderr=subprocess.STDOUT,text=True).strip())
out = pathlib.Path(sys.argv[1]) if len(sys.argv)>1 else ROOT/'fixtures/oracle/coverage-c95b2128/targets.jsonl'
out.parent.mkdir(parents=True, exist_ok=True)
with tempfile.TemporaryDirectory(prefix='mrs-coverage-compiler-') as temporary:
    temporary = pathlib.Path(temporary)
    original = (SOURCE/files[0]).read_text()
    signature = '    fun toRemote(roots: List<Pair<File, String>>, staticDirectory: String, joinPath: (String, Array<String>) -> String): Remote {'
    assert original.count(signature) == 1
    overlay = original.replace(signature, '''    fun toRemote(roots: List<Pair<File, String>>, staticDirectory: String, joinPath: (String, Array<String>) -> String): Remote =
        CoverageRecorder.remote(xctestrun, roots, staticDirectory) { toRemoteObserved(roots, staticDirectory, joinPath) }

    private fun toRemoteObserved(roots: List<Pair<File, String>>, staticDirectory: String, joinPath: (String, Array<String>) -> String): Remote {''')
    observed = temporary/'CodeCoverageTargets.kt'; observed.write_text(overlay)
    meta['observation_overlay_sha256'] = sha256(overlay.encode())
    compile_command = [java,'-cp',os.pathsep.join(map(str,compiler)),'org.jetbrains.kotlin.cli.jvm.K2JVMCompiler','-no-stdlib','-no-reflect','-nowarn','-jvm-target','17','-cp',cp]
    # Both variants execute every unmodified upstream assertion. The recorder only observes return/error values.
    for variant, coverage in [('original',SOURCE/files[0]),('observed',observed)]:
        classes = temporary/variant
        subprocess.run([*compile_command,'-d',str(classes),str(coverage),str(SOURCE/files[1]),str(SOURCE/test),str(HERE/'Recorder.kt')],check=True)
        result = temporary/f'{variant}.jsonl'
        subprocess.run([java,'-cp',str(classes)+os.pathsep+cp,'com.malinskiy.marathon.apple.xctestrun.RecorderKt',str(result)],check=True)
        if variant == 'original': original_extensions = (temporary/'extensions.jsonl').read_bytes()
        if variant == 'observed':
            repeat = temporary/'repeat.jsonl'
            subprocess.run([java,'-cp',str(classes)+os.pathsep+cp,'com.malinskiy.marathon.apple.xctestrun.RecorderKt',str(repeat)],check=True)
            assert result.read_bytes() == repeat.read_bytes()
            original_rows = [json.loads(l) for l in (temporary/'original.jsonl').read_text().splitlines()]
            rows = [json.loads(l) for l in result.read_text().splitlines()]
            assert [{**r,'calls':[]} for r in rows] == original_rows
            out.write_bytes(result.read_bytes())
            assert (temporary/'extensions.jsonl').read_bytes() == original_extensions
            out.with_name('extensions.jsonl').write_bytes(original_extensions)
meta.update(tests=17, original_assertions='passed', observation_equivalence='all original assertions and file/MachO snapshots identical', repeated_capture='byte-identical', fixture_sha256=sha256(out.read_bytes()), extension_fixture_sha256=sha256(out.with_name('extensions.jsonl').read_bytes()))
out.with_name('metadata.json').write_text(json.dumps(meta,indent=2)+'\n')
print(f'17 tests -> {out}')
