"""Fresh whole-CLI captures on loopback fakes. Never invokes a real adb binary."""
import base64
import json
import os
import re
from pathlib import Path
import select
import shutil
import signal
import subprocess
import time
import zipfile
import evidence as e
import vendor_console

ROOT = Path(__file__).resolve().parents[2]
JDK = '/Applications/Android Studio.app/Contents/jbr/Contents/Home'
# Minimum spacing of scripted xcodebuild output lines in Apple captures (see settled_world).
SETTLE_MS = 150
APPLE = ('ios-smoke', 'ios-media', 'ios-screenshot', 'macos-smoke')
CASES = {name:dict(base=name,mode='run',exit=1) for name in APPLE}
CASES.update({'android-smoke':dict(base='android-smoke',mode='run',exit=0),
              'android-two-devices':dict(base='android-two-devices',mode='run',exit=1),
              'android-remote':dict(base='android-smoke',mode='remote',exit=0)})
for vendor,base in [('ios','ios-smoke'),('macos','macos-smoke'),('android','android-two-devices')]:
    CASES[vendor+'-ignore'] = dict(base=base,mode='ignore',exit=0)
    CASES[vendor+'-parse'] = dict(base=base if vendor != 'android' else 'android-smoke',mode='parse',exit=0)
for vendor,base in [('ios','ios-smoke'),('macos','macos-smoke')]:
    CASES[vendor+'-pass'] = dict(base=base,mode='pass',exit=0)


def apple(name):
    return CASES[name]['base'] in APPLE


def world_input(name):
    filename = 'world-settled.json' if name == 'ios-media' else 'world.json'
    return ROOT / 'fixtures/differential/inputs' / name / filename


def save(path, value):
    path.write_bytes(e.encoded(value))


def settled_world(name, work):
    """The scripted world with the output-line spacing raised to SETTLE_MS (never lowered).

    A20: the event for a line is consumed concurrently with the line itself, and the per-test log
    window is whatever line text lands between the start and end handlers. The first start handler
    is the slowest; on a loaded box it can outlast the recorded 30 ms spacing, run after the later
    `passed` line was appended and wipe it, so the end handler finds an empty window and writes no
    log. The recorded inputs stay immutable; only this run's copy is respaced.
    """
    spec = e.load(world_input(name))
    spec['line_delay_ms'] = max(spec.get('line_delay_ms', 0), SETTLE_MS)
    save(work / 'world.json', spec)
    return work / 'world.json'


def wait_file(path, process):
    deadline = time.monotonic() + 15
    while not path.exists():
        e.require(process.poll() is None, 'fake exited before readiness')
        e.require(time.monotonic() < deadline, 'fake readiness timeout')
        time.sleep(.01)
    return path.read_text()


def prepare_apple(name, work):
    source = ROOT / 'fixtures/fake-worker/apple' / CASES[name]['base']
    inp = work / 'in'
    shutil.copytree(source / 'in', inp)
    spec = e.load(source / 'script/world.json')
    app = 'AppleWorld.app' if spec['platform'] == 'macos' else 'sample-app.app'
    test = ('AppleWorld.app/Contents/PlugIns/AppleWorldTests.xctest' if spec['platform'] == 'macos'
            else 'sample-appUITests-Runner.app' if CASES[name]['base'] == 'ios-media'
            else 'sample-app.app/PlugIns/sample-appUnitTests.xctest')
    with zipfile.ZipFile(ROOT / 'fixtures/bundles' / (spec['platform'] + '.zip')) as source_zip:
        for filename, prefix in [('app.zip', app), ('tests.zip', test)]:
            with zipfile.ZipFile(inp / filename, 'w', zipfile.ZIP_DEFLATED) as dst:
                for original in source_zip.infolist():
                    if original.filename.startswith(prefix + '/'):
                        import copy
                        entry = copy.copy(original)
                        entry.filename = entry.filename[len(prefix.rsplit('/', 1)[0]) + 1:] if '/' in prefix else entry.filename
                        dst.writestr(entry, source_zip.read(original.filename))
    # This input variant avoids uncontrolled JVM debug identities; complete streams
    # still remain recorded. DeviceLogListener is covered by the production path.
    config = (inp / 'Marathonfile').read_text().replace('debug: true', 'debug: false')
    if CASES[name]['mode'] == 'ignore':
        config += '\nignoreFailures: true\n'
    if CASES[name]['mode'] == 'pass':
        config += '\nfilteringConfiguration:\n  allowlist:\n    - type: fully-qualified-test-name\n      values: ["' + ('AppleWorldTests.SmokeTests#testPass' if name.startswith('macos') else 'sample_appUnitTests.SampleTests#testPassing') + '"]\n'
    (inp / 'Marathonfile').write_text(config)
    return spec


def prepare_adb(work, name):
    source = ROOT / 'fixtures/fake-adb/android-runner-cli/script'
    inp = work / 'in'; inp.mkdir()
    for apk in ('app-debug.apk', 'app-debug-androidTest.apk'):
        shutil.copyfile(ROOT / 'fixtures/vendor-android/apk' / apk, inp / apk)
    sdk = inp / 'sdk/platform-tools'; sdk.mkdir(parents=True)
    shim = sdk / 'adb'
    shim.write_text('#!/bin/sh\nprintf "%s\\n" "$*" >> "$MRS_ADB_TRIPWIRE"\nexit 70\n')
    shim.chmod(0o755)
    if CASES[name]['base'] == 'android-two-devices':
        return ROOT / 'fixtures/differential/inputs/android-two-devices/devices.yaml'
    script = e.load(source / 'devices.yaml')
    for rule in script['shell']:
        if 'stdout_file' in rule:
            rule['stdout_file'] = str((source / rule['stdout_file']).resolve())
    save(work / 'script.json', script)
    return work / 'script.json'


def raw_capture(name, implementation, work, binaries, command):
    """The returned raw directory has waited CLI/fake outcomes, payloads and state."""
    work = work.resolve(); work.mkdir(parents=True, exist_ok=False)
    for directory in ('home', 'tmp'):
        (work / directory).mkdir()
    env = dict(os.environ, HOME=str(work / 'home'), TMPDIR=str(work / 'tmp'),
               LANG='en_US.UTF-8', LC_ALL='en_US.UTF-8', TZ='UTC', JAVA_HOME=os.environ.get('JAVA_HOME', JDK))
    env['PATH'] = '/usr/bin:/bin:/usr/sbin:/sbin'
    env['MRS_ADB_TRIPWIRE'] = str(work / 'adb-invoked')
    fake = None
    handles = []
    try:
        if apple(name):
            prepare_apple(name, work)
            for key in ('host-key', 'client-key'):
                subprocess.run(['/usr/bin/ssh-keygen', '-q', '-t', 'ed25519', '-N', '', '-f', str(work / key)], check=True)
            config = dict(listen='127.0.0.1:0', root=str(work / 'root'), transcript=str(work / 'transcript.jsonl'),
                          host_key=str(work / 'host-key'), auth=dict(user='fixture', password=None,
                          authorized_keys=[str(work / 'client-key.pub')], password_auth=False, keyboard_interactive=False),
                          max_sessions=32, rsync_binary=os.environ.get('MRS_RSYNC', '/usr/bin/rsync'),
                          fixtures=str(ROOT / 'fixtures/fake-worker'), run=name)
            save(work / 'worker-config.json', config)
            err = (work / 'fake.stderr').open('wb'); handles.append(err)
            fake = subprocess.Popen([binaries['fake-worker'], '--config', work / 'worker-config.json', '--apple',
                        settled_world(name, work), '--state', work / 'state.json'],
                        stdout=subprocess.PIPE, stderr=err, start_new_session=True, umask=0o022)
            e.require(select.select([fake.stdout], [], [], 15)[0], 'worker readiness timeout')
            ready = json.loads(fake.stdout.readline()); save(work / 'ready.json', ready)
            port = ready['endpoint'].rsplit(':', 1)[1]
            public = (work / 'host-key.pub').read_text().split()
            (work / 'known_hosts').write_text(f'[127.0.0.1]:{port} {public[0]} {public[1]}\n')
            shims = ROOT / 'tools/fake-worker/shims'
            if implementation == 'native' and CASES[name]['base'] == 'macos-smoke':
                # D20: the native program has no library of its own; the JVM distribution keeps using its copy.
                supplied = work / 'supplied-parser/MacOSX'
                supplied.mkdir(parents=True)
                (supplied / 'libxctest-parser.dylib').write_bytes(e.PARSER_STAND_IN)
                env['MARATHON_XCTEST_PARSER_DIR'] = str(supplied.parent)
            env.update(FW_FIXTURE_KEY=str(work / 'client-key'), FW_KNOWN_HOSTS=str(work / 'known_hosts'), FW_PORT=port,
                       PATH=str(shims) + ':' + env['PATH'], RSYNC4J_RSYNC=str(shims / 'rsync'),
                       FW_REAL_RSYNC=config['rsync_binary'], FW_REAL_SSH='/usr/bin/ssh', FW_ARGV_LOG=str(work / 'host-argv.jsonl'))
            report = work / 'in/report'
        else:
            script = prepare_adb(work, name)
            out = (work / 'fake.stdout').open('wb'); err = (work / 'fake.stderr').open('wb'); handles += [out,err]
            fake = subprocess.Popen([binaries['fake-adb'], '--script', script, '--port', '0',
                '--port-file', work / 'port', '--transcript', work / 'transcript.jsonl', '--state', work / 'state.json', '--strict'],
                stdout=out, stderr=err, start_new_session=True, umask=0o022)
            port = wait_file(work / 'port', fake).strip()
            e.require(port != '5037', 'refuse standard adb port')
            vendor = dict(type='Android', androidSdk='./sdk', applicationApk='./app-debug.apk', testApplicationApk='./app-debug-androidTest.apk',
                autoGrantPermission=False, mockLocation=False, applicationPmClear=False, testApplicationPmClear=False,
                allureConfiguration=dict(enabled=False), screenRecordConfiguration=dict(videoConfiguration=dict(enabled=False),
                screenshotConfiguration=dict(enabled=False)), adbServers=[dict(host='127.0.0.1',port=int(port))])
            config = dict(name='differential-android',outputDir='./report',debug=False,analyticsTracking=False,
                bugsnagReporting=False,uncompletedTestRetryQuota=0, filteringConfiguration=dict(allowlist=[
                dict(type='fully-qualified-test-name',values=['com.example.MainActivityTest#testText'])]),vendorConfiguration=vendor)
            if CASES[name]['base'] == 'android-two-devices':
                config['filteringConfiguration']['allowlist'][0]['values'] += ['com.example.MainActivityAllureTest#testFailure','com.example.ClassIgnoredTest#testAlwaysIgnored']
                config['batchingStrategy'] = dict(type='fixed-size',size=10)
            if CASES[name]['mode'] == 'ignore': config['ignoreFailures'] = True
            if CASES[name]['mode'] == 'remote': vendor['testParserConfiguration'] = dict(type='remote')
            save(work / 'in/Marathonfile', config)
            report = work / 'in/report'
        tape = e.load(ROOT / 'tools/oracle/scenario-tape.json')
        tape['clocks'] = dict(wall=dict(mode='frozen',start='2020-01-01T00:00:00Z'),monotonic=dict(mode='frozen',startNanos=0),scheduler=dict(mode='real'))
        del tape['ids']['test.ConfigurationFactory.outputDir']
        for label in ['apple.TestRootFactory.generate', 'apple.XCTestParser.parseTests', 'ios.AppleSimulatorDevice.getScreenshot']:
            tape['ids'][label] = dict(pattern='%012d')
        if apple(name) or CASES[name]['mode'] == 'remote':
            label = ('macos.AppleMacosProvider.borrow' if CASES[name]['base'] == 'macos-smoke' else 'ios.AppleSimulatorProvider.borrow')
            serial = 'AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE@fixture@127.0.0.1:' + port
            if not apple(name):
                label = 'android.AdamDeviceProvider.borrow'
                serial = '127.0.0.1:' + port + ':emulator-5554'
            tape['draws'] = {label:[serial]}
            # Bind the same scenario device across the JVM's public serial and
            # native provider-map namespace. Keep live native allocation choice
            # intact when two devices expose the same public serial.
            native_key = serial
            if not apple(name): native_key = 'emulator-5554'
            elif CASES[name]['base'] != 'macos-smoke':
                native_key = 'AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE@127.0.0.1:' + port
            env['MRS_DIFFERENTIAL_BORROW'] = native_key
            env['MRS_DIFFERENTIAL_BORROW_LABEL'] = label
        if CASES[name]['base'] == 'android-two-devices':
            tape['barriers'] = ['android.AdamDeviceProvider.connected:emulator-5554','android.AdamDeviceProvider.connected:emulator-5556',
                f'core.DeviceActor.prepare:127.0.0.1:{port}:emulator-5554',f'core.DeviceActor.prepare:127.0.0.1:{port}:emulator-5556',
                f'core.DeviceActor.execute:127.0.0.1:{port}:emulator-5554']
            env['MRS_DIFFERENTIAL_BARRIERS'] = json.dumps(tape['barriers'])
        save(work / 'tape.json', tape)
        env['MRS_DIFFERENTIAL_HOOKS'] = str(work / 'hooks.json')
        env['JAVA_OPTS'] = (f'-Doracle.hooks.tape={work}/tape.json -Doracle.hooks.report={work}/hooks.json '
                            f'-Djava.io.tmpdir={work}/tmp -Duser.timezone=UTC -Dfile.encoding=UTF-8')
        binary = binaries[implementation]
        report.mkdir(exist_ok=True)
        (report/'sentinel').write_bytes(b'seeded literal 12345678-1234-1234-1234-123456789abc\n')
        # command's cwd normally is the repository; the CLI receives an explicit
        # configuration path, so every product write remains in the fresh input.
        argv = [binary, 'parse' if CASES[name]['mode'] == 'parse' else 'run', '-m', work / 'in/Marathonfile']
        if CASES[name]['mode'] == 'parse': argv += ['-o','parsed']
        command(argv, work / 'process', env, timeout=60, expected_exit=CASES[name]['exit'])
    finally:
        if fake is not None:
            if fake.poll() is None:
                fake.send_signal(signal.SIGTERM)
            try:
                fake_exit = fake.wait(timeout=15)
            except subprocess.TimeoutExpired:
                os.killpg(fake.pid, signal.SIGKILL); fake.wait()
                raise e.Invalid('fake shutdown timeout')
            save(work / 'fake-process.json', dict(exit=fake_exit, watchdog=False, joined=True))
        for handle in handles:
            handle.close()
    e.require(fake_exit == 0, 'fake failed; retained ' + str(work))
    e.require(not (work / 'adb-invoked').exists(), 'adb tripwire invoked')
    e.require(report.is_dir(), 'CLI produced no report tree')
    e.require((report/'sentinel').exists() == (CASES[name]['mode'] == 'parse'), 'parse/wipe side effect differs')
    return work


def identities(name, raw):
    port = (e.load(raw / 'ready.json')['endpoint'].rsplit(':',1)[1] if apple(name) else (raw / 'port').read_text().strip())
    serial = ('AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE@fixture@127.0.0.1:' + port if apple(name) else '127.0.0.1:' + port + ':emulator-5554')
    safe = serial.replace(':','-').replace('@','-')
    files = list((raw / 'in/report/devices/omni').glob('*.json'))
    if CASES[name]['mode'] != 'parse':
        count = 2 if CASES[name]['base'] == 'android-two-devices' else 1
        e.require(len(files) == count and any(p.stem == safe and e.load(p)['serialNumber'] == serial for p in files),
                  'device identity differs from explicit scenario role')
    values = dict(sandbox=str(raw),port=port,serial=serial,safe_serial=safe)
    if CASES[name]['base'] in ('macos-smoke','ios-screenshot'):
        rows = [e.loads(line) for line in (raw/'host-argv.jsonl').read_bytes().splitlines()]
        roots = []
        for row in rows:
            if row['program'] != 'rsync': continue
            source,target = row['argv'][-2:]
            if source.startswith(str(raw/'tmp')+'/') and target.endswith('/marathon.xctestrun'):
                roots.append(source)
            elif source.startswith(str(raw/'tmp')+'/') and target.endswith('/libxctest-parser.dylib'):
                values['temp_parser'] = source
            elif target.startswith(str(raw/'tmp')+'/') and source.endswith('.png'):
                values['temp_screenshot'] = target
        if CASES[name]['base'] == 'macos-smoke':
            count = 1 if CASES[name]['mode'] == 'parse' else 2
            e.require(len(roots) == count and 'temp_parser' in values,'missing macOS temporary file roles')
            values.update(temp_discovery=roots[0])
            if len(roots) == 2: values['temp_execution'] = roots[1]
        else:
            e.require('temp_screenshot' in values,'missing screenshot temporary file role')
    if CASES[name]['base'] == 'android-two-devices':
        values['idle_serial'] = '127.0.0.1:' + port + ':emulator-5556'
        values['idle_safe_serial'] = values['idle_serial'].replace(':','-')
    return values


def canonical(path, values):
    parts = path.split('/')
    # FileManager's device directory and device/bill basename only.
    for i, part in enumerate(parts):
        for key,value in values.items():
            if not key.endswith('safe_serial'): continue
            if part == value: parts[i] = '{' + key + '}'
            elif part == value + '.json': parts[i] = '{' + key + '}.json'
    return '/'.join(parts)


def terminal(raw):
    process = e.load(raw / 'process/process.json')
    fake = e.load(raw / 'fake-process.json')
    hooks = e.load(raw / 'hooks.json')
    validate_hooks(hooks)
    e.require(not hooks.get('unused') and not hooks.get('failures'), 'unused or failed tape')
    e.require(type(process.get('exit')) is int and type(process.get('watchdog')) is bool, 'missing CLI outcome')
    e.require(type(fake.get('exit')) is int and fake.get('joined') is True, 'missing fake outcome')
    return dict(cli=process['exit'],fake=fake['exit'],harness=0,watchdog=process['watchdog'] or fake['watchdog'],
                joined=not process['watchdog'] and fake['joined'],unused=[],unmatched=[])


def define_contract(name, raw):
    """An explicit JVM-only schema inventory; never infer expected paths from native."""
    values = identities(name, raw)
    entries, _ = e.snapshot(raw / 'in/report', 'reports')
    artifacts = ['stdout','stderr','transcript.jsonl','state.json']
    if apple(name):
        worker, _ = e.snapshot(raw/'root','worker')
        entries.extend(worker)
        artifacts.append('host-argv.jsonl')
    paths = [canonical(row['path'],values) for row in entries] + artifacts
    normal = {}
    for path in paths:
        if path.startswith('reports/allure-results/') and path.endswith('-result.json'):
            normal[path] = dict(mode='allure-json',rules=[])
        if path == 'reports/allure-results/environment.xml':
            normal[path] = dict(mode='allure-environment',rules=[])
        if path.startswith('reports/screenshot/') and path.endswith('.gif'):
            normal[path] = dict(mode='gif',rules=[])
    env = raw/'in/report/allure-results/environment.xml'
    object_ids = re.findall(r'com\.malinskiy\.marathon\.config\.[A-Za-z0-9_.$]+@[0-9a-f]+',env.read_text()) if env.exists() else []
    return dict(format='marathon-differential/contract@1',scenario='vendor',vendor=name,jvm_config_identities=object_ids,
        roles={key:('path' if key == 'sandbox' or key.startswith('temp_') else 'endpoint' if key == 'port' else 'device') for key in values},paths=paths,
        normalization=normal,outcome=terminal(raw),uses_tapes=True,transport='ssh' if apple(name) else 'adb')


def pack(name, implementation, raw, contract, destination, provenance):
    outcome = terminal(raw)
    facts = dict(provenance, process=e.load(raw/'process/process.json'),terminal_observation=outcome,
                 fake_process=e.load(raw/'fake-process.json'),hooks=e.load(raw/'hooks.json'))
    if implementation == 'jvm':
        source = facts.get('source_sha', e.SOURCE)
        build = 'build' if source == e.SOURCE else 'build-'+source
        lib = ROOT / ('tools/oracle/'+build+'/marathon-patched/cli/build/install/marathon/lib')
        facts['jars'] = {p.name:e.sha(p.read_bytes()) for p in sorted(lib.glob('*.jar'))}
        facts['patches'] = {p.name:e.sha(p.read_bytes()) for p in sorted((ROOT/'tools/oracle/patches').glob('*.patch'))}
        facts['hook_sources'] = {str(p.relative_to(ROOT)):e.sha(p.read_bytes()) for p in sorted((ROOT/'tools/oracle/hooks/src/main').rglob('*.kt'))}
    trees = {'reports':raw/'in/report'}
    artifacts = {name:(raw/name).read_bytes() for name in ['transcript.jsonl','state.json']}
    artifacts.update({name:(raw/'process'/name).read_bytes() for name in ['stdout','stderr']})
    if apple(name):
        trees['worker'] = raw/'root'
        artifacts['host-argv.jsonl'] = (raw/'host-argv.jsonl').read_bytes()
        # rsync4j warns on stderr when its default sshpass path is missing; which stream is right depends on the host.
        facts[vendor_console.SSHPASS_FACT] = os.path.exists(vendor_console.SSHPASS)
    else:
        facts['stdin_payloads'] = {str(p.relative_to(ROOT)):e.sha(p.read_bytes()) for p in
            [ROOT/'fixtures/vendor-android/apk'/apk for apk in ['app-debug.apk','app-debug-androidTest.apk']]}
    return e.write_record(destination,contract,implementation,trees,artifacts,outcome,facts,identities(name,raw),compressed=True)



def validate_record(directory, contract, record):
    facts = record['provenance']
    cli, fake, hooks = facts.get('process'),facts.get('fake_process'),facts.get('hooks')
    e.require(isinstance(cli,dict) and type(cli.get('exit')) is int and type(cli.get('watchdog')) is bool,
              'missing vendor CLI process observation')
    e.require(isinstance(fake,dict) and type(fake.get('exit')) is int and fake.get('joined') is True
              and type(fake.get('watchdog')) is bool, 'missing vendor fake process observation')
    validate_hooks(hooks)
    outcome = dict(cli=cli['exit'],fake=fake['exit'],harness=0,watchdog=cli['watchdog'] or fake['watchdog'],
                   joined=not cli['watchdog'] and fake['joined'],unused=[],unmatched=[])
    e.require(outcome == record['outcome'],'vendor outcome differs from waited processes')
    def artifact(path):
        rows = [r for r in record['entries'] if r['path'] == path and r['kind'] == 'file']
        e.require(len(rows) == 1,'missing vendor artifact: ' + path)
        return e.payload(directory,rows[0],record)
    import vendor_transport as t
    if apple(contract['vendor']):
        t.ssh(artifact('transcript.jsonl'),contract['vendor'])
        if e.may_withhold(directory, record):
            e.require(t.redact_parser_transfer(artifact('transcript.jsonl')) == artifact('transcript.jsonl'),
                      'third-party library bytes in the transcript of a recorded run')
        else:
            t.check_new_native_transfer(artifact('transcript.jsonl'))
        t.host_argv(artifact('host-argv.jsonl'),record['identities'])
        state = e.loads(artifact('state.json'))
        e.require(not state['deferred'] and all(s['state'] == 'finished' for s in state['sessions']),
                  'unfinished Apple world state')
    else:
        rows = t.records(artifact('transcript.jsonl'))
        t.adb(artifact('transcript.jsonl'))
        payloads = facts.get('stdin_payloads')
        e.require(isinstance(payloads,dict) and len(payloads) == 2,'missing stdin payload sources')
        for path,digest in payloads.items():
            e.relative(path)
            e.require(path.startswith('fixtures/vendor-android/apk/') and e.sha((ROOT/path).read_bytes()) == digest,
                      'changed stdin source payload')
        for row in rows:
            if row['type'] == 'shell' and row.get('stdin_bytes'):
                sources = [ROOT/path for path,digest in payloads.items() if digest == row.get('stdin_sha256')]
                e.require(sources and all(path.stat().st_size == row['stdin_bytes'] for path in sources),'unrecorded/truncated stdin payload')
        state = e.loads(artifact('state.json'))
        e.require(isinstance(state,dict) and set(state) == {'devices'} and isinstance(state['devices'],list),'malformed ADB state')
        expected = ['emulator-5554','emulator-5556'] if CASES[contract['vendor']]['base'] == 'android-two-devices' else ['emulator-5554']
        e.require([d.get('serial') for d in state['devices']] == expected,'ADB device state inventory differs')
        for device in state['devices']:
            e.require(set(device) == {'serial','state','connected','features','getprop','reverse','files'}
                      and isinstance(device['files'],list) and device['files'],'malformed ADB device state')
            paths = set()
            for node in device['files']:
                e.require(set(node) == {'path','mode','mtime','content'} and node['path'].startswith('/')
                          and node['path'] not in paths,'malformed/duplicate ADB file state')
                paths.add(node['path'])
                e.require(type(node['mode']) is int and 0 <= node['mode'] <= 0o7777
                          and type(node['mtime']) is int and 0 <= node['mtime'] <= 0xffffffff,'invalid ADB file metadata')
                e.require(node['content'] is None or (isinstance(node['content'],list)
                          and all(type(x) is int and 0 <= x <= 255 for x in node['content'])),'invalid ADB file bytes')
            e.require('/' in paths,'missing ADB filesystem root')



def validate_hooks(hooks):
    e.require(isinstance(hooks,dict) and hooks.get('active') is True
              and isinstance(hooks.get('unused'),(dict,list)) and isinstance(hooks.get('failures'),list),
              'missing vendor hook accounting')
    e.require(not hooks['unused'] and not hooks['failures'],'unused/failed vendor tape')


def link_mode(path, mode, record, contract):
    # umask 0022 affects Darwin symlinks; Linux always reports 0777. Keep raw
    # lstat values and validate the host rule at this one scripted link only.
    if (contract['vendor'] == 'ios-media' and path ==
            'worker/AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE/sample_appUITests-Runner.app'):
        host = record['provenance'].get('platform', '')
        expected = 0o755 if host.startswith('macOS-') else 0o777 if host.startswith('Linux-') else None
        e.require(expected is not None and mode == expected, 'unexpected host symlink mode: ' + path)
        return 'kernel-default-symlink@0022'
    return mode
