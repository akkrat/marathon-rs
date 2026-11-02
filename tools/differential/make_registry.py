#!/usr/bin/env python3
"""Explicit ledger refresh. Review before committing; never run implicitly from a gate."""
from pathlib import Path
import hashlib
import json
import sys
sys.path.insert(0, str(Path(__file__).resolve().parent))
from registry_support import factory_fingerprints
import vendors

ROOT = Path(__file__).resolve().parents[2]
def sha(path): return hashlib.sha256((ROOT / path).read_bytes()).hexdigest()
def inputs(*paths): return {p:sha(p) for p in paths}
def target(package, test): return ['-p', package, '--test', test]
rows = []
def active(name, owner, transport, comparison, native, evidence, source_inputs):
    rows.append(dict(id=name, state='active', owner=owner, transport=transport, comparison=comparison,
                     prerequisites=['offline Rust toolchain', 'committed JVM evidence'], native=native,
                     evidence=evidence, inputs=inputs(*source_inputs)))
for name in ['success','parse','filtering-1','filtering-2','filtering-3','disconnect','all-lost','invalid-config',
             'two-pools-disconnect','uncompleted-1','uncompleted-2','uncompleted-3','uncompleted-4','scripted','scripted-failures']:
    active('core-'+name, '3.10', 'injected-core', 'legacy 3.10 outcome/log/report subset; not strict full-manifest certification',
           target('marathon-core','run_flow_oracle'), ['fixtures/oracle/run-flow/scenarios.jsonl'],
           ['tools/oracle/scenarios/'+name+'.json'])
for name in ['local','parse']:
    active('strict-'+name, '7.1', 'actual-local' if name=='local' else 'injected-core', 'full manifest and exact bytes; scoped roles',
           target('marathon-test-support','differential_'+name), ['fixtures/differential/portable/'+name+'/jvm-v1'],
           ['tools/differential/contracts/'+name+'.json'])
active('apple-fake-oracle','5.1','injected-world','Kotlin fake command/result corpus', target('marathon-test-support','fake_simulator_worker'),
       ['fixtures/oracle/fake-simulator/probes.jsonl'], ['fixtures/oracle/fake-simulator/probes.jsonl'])
active('apple-world-contracts','5.1','injected-world-and-loopback-ssh','46 world/SSH wrapper comparisons plus lifecycle contracts; no whole JVM CLI pair',
       target('marathon-test-support','world_executor'), ['fixtures/fake-worker/apple/command-catalogue.json'], [])
for test in ['client_fake_adb','request_fake_adb']:
    active('adb-'+test,'6.1/6.2','scripted-adb','independent JVM request/result/state witnesses; not whole CLI',
           target('marathon-adb',test), ['fixtures/fake-adb/adb-requests/kotlin-transcript.jsonl'],
           ['fixtures/fake-adb/adb-requests/devices.yaml'])
for test in ['local_executor','file_bridge']:
    active('local-'+test,'4.1','actual-local','existing local executor/file contracts',target('marathon-apple',test),
           ['tools/oracle/local-transport-probes/README.md'], [])
for test in ['reporters','allure_reporter','html_reporter','timeline_reporter','screenshot']:
    active('report-'+test,'3.9b/3.9c','injected-reporter','owner JVM corpus; scoped Allure structure and gradient-only GIF tolerance',
           target('marathon-core',test), ['fixtures/oracle/html-allure-gif','fixtures/oracle/reporters'], [])
adapters = {name:dict(kind='portable',contract='tools/differential/contracts/'+name+'.json',
                     golden='fixtures/differential/portable/'+name+'/jvm-v1') for name in ['local','parse']}
release_variants = json.loads((ROOT/'tools/differential/release-variants.json').read_text())
for name,spec in vendors.CASES.items():
    c = 'tools/differential/contracts/vendor-'+name+'.json'
    golden = 'fixtures/differential/vendors/'+name+'/jvm-c95b2128'
    adapters[name] = dict(kind='vendor',contract=c,golden=golden)
    if name in release_variants:
        variant = release_variants[name]
        if not variant['reason'].startswith('A20:') or not variant['paths']:
            raise ValueError('release variant lacks A20 evidence')
        adapters[name]['alternates'] = variant['alternates']
    scenario = (str(vendors.world_input(name).relative_to(ROOT)) if vendors.apple(name) else
                'fixtures/differential/inputs/android-two-devices/devices.yaml' if spec['base']=='android-two-devices'
                else 'fixtures/fake-adb/android-runner-cli/script/devices.yaml')
    source_inputs = [c,scenario,'tools/oracle/scenario-tape.json',
        'crates/marathon-cli/examples/differential_cli.rs','tools/differential/evidence.py','tools/differential/run',
        'tools/differential/vendors.py','tools/differential/vendor_reports.py',
        'tools/fake-worker/src/apple.rs','crates/marathon-adb/src/server/world.rs',
        'tools/differential/vendor_console.py','tools/differential/vendor_transport.py','tools/differential/gif_frames.py']
    if vendors.apple(name):
        source_inputs += ['fixtures/fake-worker/apple/'+spec['base']+'/in/'+filename
                          for filename in ['Marathonfile','Marathondevices']]
        source_inputs.append('fixtures/bundles/'+('macos' if spec['base']=='macos-smoke' else 'ios')+'.zip')
    else:
        source_inputs += ['fixtures/vendor-android/apk/'+filename for filename in ['app-debug.apk','app-debug-androidTest.apk']]
        source_inputs += ['fixtures/adam/logcat-long.txt','fixtures/fake-adb/android-two-devices/script/instrumentation.txt']
        source_inputs += [str(p.relative_to(ROOT)) for p in sorted((ROOT/'fixtures/fake-adb/android-runner-cli/script').glob('*.txt'))]
    active(name,'7.1b','ssh' if vendors.apple(name) else 'adb',
           'whole CLI: full report and worker manifests, typed bytes, ordered console, causal transport, final state',
           target('marathon-cli','differential_vendors'),[golden,*adapters[name].get('alternates',[])],source_inputs)
# P1: integrate the owning WPs' feature/fault contracts without multiplying every
# feature into every whole-CLI baseline. Their narrower scopes remain explicit.
for name,owner,transport,package,test,paths,scope in [
 ('apple-faults','5.5b','injected-world','marathon-ios','simulator_recovery',['fixtures/oracle/simulator-recovery/probes.jsonl'],
  'owner JVM recovery/boot/crash/disconnect contracts; no extra whole-CLI fault cross-product'),
 ('apple-faults-pool','5.5b','injected-world','marathon-ios','simulator_pool_recovery',['fixtures/oracle/simulator-recovery/pool.json'],
  'owner JVM-backed pool recovery and late admission contracts'),
 ('android-install-matrix','6.5','adb','marathon-android','scenarios_test',['fixtures/fake-adb/android-install-matrix/expected/kotlin.jsonl'],
  'owner installer/provider request sequences and state; not a whole-CLI report claim'),
 ('android-video','6.6b','adb','marathon-android','android_artifacts',['fixtures/oracle/android-artifact-probes.jsonl'],
  'owner JVM video bytes, requests and cleanup'),
 ('android-screenshot','6.6b','adb','marathon-android','android_artifacts',['fixtures/oracle/android-artifact-probes.jsonl'],
  'owner JVM decoded framebuffer/GIF and metadata'),
 ('android-filesync-coverage','6.6b','adb','marathon-android','android_artifacts',['fixtures/oracle/android-artifact-probes.jsonl'],
  'owner JVM file-sync and coverage listener requests/bytes'),
 ('android-coverage-argv','6.6a','adb','marathon-android','android_runner',['fixtures/oracle/android-runner-probes.jsonl'],
  'owner JVM coverage and instrumentation argv/callback contracts'),
 ('android-profiling','6.6b','adb','marathon-android','android_artifacts',['fixtures/oracle/android-artifact-probes.jsonl'],
  'owner JVM perfetto bytes/commands and D16 cleanup expectations'),
 ('android-chaos','6.5/6.6a','adb','marathon-android','android_runner',['fixtures/oracle/android-runner-probes.jsonl'],
  'owner JVM runner loss/timeout/crash and callback contracts'),
]:
    active(name,owner,transport,scope,target(package,test),paths,paths)
# Pin the old evidence as immutable, explicitly reduced inventories, not upgraded goldens.
legacy = []
for pattern in ['fixtures/fake-worker/apple/*/expected/**/*','fixtures/fake-adb/android-two-devices/expected/*',
                'fixtures/oracle/run-flow/*.jsonl','fixtures/fake-adb/adb-requests/*','fixtures/oracle/fake-simulator/*']:
    legacy.extend(str(p.relative_to(ROOT)) for p in ROOT.glob(pattern) if p.is_file())
legacy += ['fixtures/differential/vendor-verification/manifest.json']
legacy += [str(p.relative_to(ROOT)) for p in ROOT.glob('fixtures/differential/vendors/*/jvm-v2/evidence.json')]
ledger = dict(format='marathon-differential/registry@1',source='c95b212882c7ba7b1ad91b7981916ed175f1530e',
              profile='registered-vendors',profiles=['macos-arm64','linux-x86_64'],adapters=adapters,
              **factory_fingerprints(ROOT),
              evidence_inventory=inputs('fixtures/differential/verification/manifest.json','fixtures/differential/vendor-verification-c95b2128/manifest.json','tools/differential/release-variants.json',
                  *[row['golden']+'/evidence.json' for row in adapters.values()],
                  *[path+'/evidence.json' for row in adapters.values() for path in row.get('alternates',[])]),
              legacy_inventory=inputs(*sorted(set(legacy))),cases=rows)
path = ROOT / 'tools/differential/registry.json'
text = json.dumps(ledger,indent=2)+'\n'
if '--check' in sys.argv:
    if path.read_text() != text:
        sys.exit('differential registry is stale; review make_registry.py and regenerate explicitly')
else:
    path.write_text(text)
