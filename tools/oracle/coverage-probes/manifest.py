import hashlib,json,pathlib,subprocess
root=pathlib.Path(__file__).resolve().parents[3];base=root/'fixtures/oracle/coverage-c95b2128'
sha=lambda p:hashlib.sha256(p.read_bytes()).hexdigest()
source='c95b212882c7ba7b1ad91b7981916ed175f1530e'
lib=root/f'tools/oracle/build-{source}/marathon-src/cli/build/install/marathon/lib'
files={str(p.relative_to(root)):sha(p) for p in sorted(base.rglob('*')) if p.is_file() and p.name!='release-metadata.json'}
tools={str(p.relative_to(root)):sha(p) for p in sorted((root/'tools/oracle/coverage-probes').glob('*')) if p.is_file()}
tools.update({str(p):sha(root/p) for p in map(pathlib.Path,['tools/oracle/config-differential-lib/suite.py','tools/oracle/validate_patches.py','tools/oracle/hook_inventory.py','tools/differential/run','tools/differential/evidence.py','tools/differential/vendors.py','tools/differential/make_registry.py'])})
metadata=dict(source_sha=source,baseline_sha='e47352f25cbc50333900d5d040a5544ff1224501',native_implementation_commit=subprocess.check_output(['git','log','-1','--format=%H','--','crates','Cargo.toml','Cargo.lock'],text=True).strip(),patches={p.name:sha(p) for p in sorted((root/'tools/oracle/patches').glob('*.patch'))},reference_jars={p.name:sha(p) for p in sorted(lib.glob('*.jar'))},generators=tools,fixtures=files,copied_fixtures={str(p.relative_to(root)):sha(p) for p in sorted((root/'fixtures/configuration/fixture/config/ios').glob('sample_[89].yaml'))})
cache=pathlib.Path.home()/'.gradle/caches/modules-2/files-2.1'
compiler=[('org.jetbrains.kotlin','kotlin-compiler-embeddable','1.9.24'),('org.jetbrains.kotlin','kotlin-stdlib','1.9.24'),('org.jetbrains.kotlin','kotlin-script-runtime','1.9.24'),('org.jetbrains.kotlin','kotlin-reflect','1.6.10'),('org.jetbrains.intellij.deps','trove4j','1.0.20200330'),('org.jetbrains','annotations','13.0')]
metadata['probe_compiler_jars']={p.name:sha(p) for group,artifact,version in compiler for p in [next((cache/group/artifact/version).glob('*/*.jar'))]}
(base/'release-metadata.json').write_text(json.dumps(metadata,indent=2)+'\n')
print(len(files),'release oracle artifacts indexed')
