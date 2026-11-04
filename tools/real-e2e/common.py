"""Offline preparation helpers; never substitutes for Marathon product formatting."""
import json
import hashlib
import os
from pathlib import Path
import re
import signal
import subprocess
import tempfile
import sys
import time

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'tools/differential'))
import evidence as e

HERE = Path(__file__).resolve().parent
LEDGER = e.load(HERE / 'scenarios.json')
SCENARIOS = {s['id']: s for s in LEDGER['scenarios']}


def save(path, value):
    path = Path(path)
    with tempfile.NamedTemporaryFile(dir=path.parent, prefix='.journal-', delete=False) as output:
        temporary = Path(output.name)
        try:
            output.write(e.encoded(value)); output.flush(); os.fsync(output.fileno())
            os.replace(temporary, path)
        finally:
            temporary.unlink(missing_ok=True)


def require(ok, message):
    e.require(ok, message)


def port(value):
    require(type(value) is int and 1024 <= value <= 65535 and value != 5037,
            'blocked: explicit non-default unprivileged port required')
    return value


def private_config(path, require_transport=True):
    path = Path(path)
    require(not path.is_symlink() and path.is_file(), 'private config must be a regular file')
    require(path.stat().st_mode & 0o077 == 0, 'private config must be mode 0600')
    config = e.load(path)
    require(config['profile'] in LEDGER['profiles'], 'unknown host profile')
    require(config.get('worker_id') and not config['worker_id'].startswith('SET_'), 'worker identity required')
    if config['profile'] == 'ssh-26.6' and require_transport:
        ssh = config['ssh']
        require(ssh['host'] == '127.0.0.1', 'only the approved loopback SSH forward is allowed')
        port(ssh['port'])
        require(re.fullmatch(r'[A-Za-z0-9_.-]+', ssh['user']) is not None, 'invalid SSH account')
        for key in ('key', 'known_hosts'):
            require(Path(ssh[key]).is_absolute() and Path(ssh[key]).is_file(), f'missing SSH {key}')
    if config['profile'] == 'linux-kvm':
        port(config['adb_port']); port(config['emulator_port'])
        require(config['adb_port'] not in (config['emulator_port'], config['emulator_port'] + 1), 'overlapping ports')
        require(5554 <= config['emulator_port'] <= 5682 and config['emulator_port'] % 2 == 0,
                'invalid emulator console port')
        require(re.fullmatch(r'system-images;android-(\d+);[^;]+;x86_64', config['image']) is not None,
                'installed x86_64 system image required')
        require(int(config['image'].split(';')[1].split('-')[1]) >= 34, 'API must be >=34')
    require(600 <= config.get('watchdog_seconds', 1800) <= 7200, 'watchdog outside bounded range')
    require(set(config.get('environment', {})) <= {'DEVELOPER_DIR', 'ANDROID_HOME', 'ANDROID_SDK_ROOT', 'MARATHON_XCTEST_PARSER_DIR'},
            'unapproved environment key')
    config['harness_sha256'] = harness_hashes()
    return config


FROZEN = e.SOURCE
SOURCE_KINDS = ('frozen', 'release')
KOTLIN_SAMPLES = {'iOS': 'sample/ios-app', 'Android': 'sample/android-app'}
# The macOS sample is this repository's own project, not a Kotlin-repository path (PROVENANCE.md).
REPOSITORY_SAMPLE = 'sample/macos-app'
REFERENCE_FORMAT = 'marathon-real-e2e/reference@1'
PATCH_FILE = 'tools/oracle/patches/0001-oracle-module.patch'


def release_sha():
    value = (ROOT / 'docs/release-source.sha').read_text().strip()
    require(re.fullmatch(r'[0-9a-f]{40}', value) is not None and value != FROZEN, 'docs/release-source.sha is not a release commit')
    return value


def source_sha(kind):
    """The full SHA of a named source. The frozen baseline is the default everywhere; release is always explicit."""
    require(kind in SOURCE_KINDS, 'unknown Kotlin source kind')
    return FROZEN if kind == 'frozen' else release_sha()


def source_kind(sha):
    require(sha in (FROZEN, release_sha()), 'unknown Kotlin source')
    return 'frozen' if sha == FROZEN else 'release'


def capture_source(record):
    """Captures and build records from before WP 7.5 carry no source field; all of them are frozen-source."""
    return record.get('source', FROZEN)


def git_output(*argv, timeout=120):
    return subprocess.check_output(['git', *argv], text=True, timeout=timeout).strip()


def verified_source(kind, kotlin_source):
    """Resolve a source kind to its full SHA and prove the checkout holds that exact commit."""
    sha = source_sha(kind)
    found = subprocess.run(['git', '-C', str(kotlin_source), 'rev-parse', '--verify', '--quiet', sha + '^{commit}'],
                           capture_output=True, text=True, timeout=60)
    require(found.returncode == 0 and found.stdout.strip() == sha, 'the Kotlin checkout does not hold the selected commit: ' + kind)
    return sha


def sample_record(vendor, kotlin_source, sha):
    """Tree hashes of the sample a build archives, from the selected commit and from the frozen one."""
    if vendor == 'macOS':
        path = REPOSITORY_SAMPLE
        require(not git_output('-C', str(ROOT), 'status', '--porcelain', '--', path), 'sample/macos-app has local changes')
        tree = git_output('-C', str(ROOT), 'rev-parse', 'HEAD:' + path)
        return dict(origin='repository', paths={path: tree}, frozen={path: tree}, matches_frozen=True)
    path = KOTLIN_SAMPLES[vendor]
    chosen = git_output('-C', kotlin_source, 'rev-parse', sha + ':' + path)
    frozen = git_output('-C', kotlin_source, 'rev-parse', FROZEN + ':' + path)
    return dict(origin='kotlin', paths={path: chosen}, frozen={path: frozen}, matches_frozen=chosen == frozen)


def select_build_source(vendor, kotlin_source, kind):
    """The source a new build is bound to, with proof of its sample trees. Raises before anything is staged."""
    # Only a frozen macOS build (which archives nothing from the Kotlin repository) may omit the checkout.
    if kotlin_source or vendor != 'macOS' or kind != 'frozen':
        require(kotlin_source, 'kotlin_source is required to bind the build to its source')
        source = verified_source(kind, kotlin_source)
    else:
        source = FROZEN
    samples = sample_record(vendor, kotlin_source, source)
    # The 7.2 scenario ledger was written against these samples; a different tree needs a new ledger, not a rebuild.
    require(samples['matches_frozen'], 'the selected source changed the sample; the scenario ledger does not apply')
    return source, samples


def build_source(build):
    """The source a build record is bound to; it must be internally consistent."""
    sha = capture_source(build)
    kind = source_kind(sha)
    require(build.get('source_kind', 'frozen') == kind, 'build record source kind differs from its source')
    if kind == 'release':
        samples = build.get('samples')
        require(isinstance(samples, dict) and samples.get('matches_frozen') is True and samples.get('paths'),
                'release build record lacks verified sample trees')
    return sha


RECEIPT_FORMAT = 'marathon-oracle/build-receipt@1'


def jar_hashes(root):
    return {p.name: e.sha(p.read_bytes()) for p in sorted(Path(root).glob('*.jar'))}


def tree_with_patches(clone, sha, patches):
    """The tree build.sh verifies a clone against: the commit's tree with the oracle patches applied, in a scratch index."""
    with tempfile.TemporaryDirectory() as scratch:
        env = dict(os.environ, GIT_INDEX_FILE=str(Path(scratch) / 'index'))
        for argv in [['read-tree', sha], *[['apply', '--cached', str(p)] for p in patches]]:
            subprocess.run(['git', '-C', str(clone), *argv], env=env, check=True, capture_output=True, timeout=120)
        return subprocess.run(['git', '-C', str(clone), 'write-tree'], env=env, check=True, capture_output=True,
                              text=True, timeout=120).stdout.strip()


def check_receipt(receipt, sha):
    """Structure and bindings of the receipt tools/oracle/build.sh writes after a successful build."""
    require(isinstance(receipt, dict) and receipt.get('format') == RECEIPT_FORMAT and receipt.get('variant') == 'reference',
            'invalid builder receipt')
    require(receipt.get('source') == sha and receipt.get('head') == sha, 'the builder receipt is for another source')
    require(receipt.get('gradle_exit') == 0 and re.fullmatch(r'[0-9a-f]{40}', str(receipt.get('verified_tree'))) is not None,
            'the builder receipt does not record a verified, successful build')
    require(receipt.get('patches') == {'0001-oracle-module.patch': e.sha((ROOT / PATCH_FILE).read_bytes())},
            'the builder receipt names other patches than the reference one')
    jars = receipt.get('jars')
    require(isinstance(jars, dict) and jars and all(re.fullmatch(r'[0-9a-f]{64}', str(v)) for v in jars.values()),
            'the builder receipt lists no output jars')


def jvm_reference_source(host):
    """The source the configured JVM distribution was built from, or None when the host has no reference record.

    The record is written by `run reference` from the builder receipt of tools/oracle/build.sh and embeds it; here the
    distribution's jars are re-hashed against it, so a copied distribution (the Linux worker's) cannot silently be
    another build.
    """
    path = host.get('jvm_reference')
    if not path:
        return None
    record = e.load(path)
    require(record.get('format') == REFERENCE_FORMAT and re.fullmatch(r'[0-9a-f]{40}', str(record.get('source'))),
            'invalid JVM reference record')
    source_kind(record['source'])
    check_receipt(record.get('receipt'), record['source'])
    require(record['receipt']['jars'] == record['jars'], 'JVM reference record differs from its builder receipt')
    jars = Path(host['jvm']).resolve().parent.parent / 'lib'
    require(jars.is_dir(), 'JVM installDist library directory missing')
    require(jar_hashes(jars) == record['jars'], 'JVM distribution differs from its reference record')
    require(record['patches'] == {'0001-oracle-module.patch': e.sha((ROOT / PATCH_FILE).read_bytes())}, 'JVM reference patch differs')
    return record['source']


def require_reference_source(host, source):
    """Every new capture, frozen or release, proves which source its JVM distribution was built from."""
    bound = jvm_reference_source(host)
    require(bound is not None, 'blocked: the host config needs a jvm_reference record (run reference)')
    require(bound == source, 'JVM reference is not bound to the selected source')
    return bound


def harness_hashes():
    return {name: e.sha((HERE / name).read_bytes()) for name in ('run', 'worker.py', 'common.py', 'configs.py', 'build.py', 'outcomes.py', 'projection.py')}


def ssh_argv(config):
    s = config['ssh']
    return ['ssh', '-T', '-p', str(s['port']), '-i', s['key'], '-o', 'BatchMode=yes',
            '-o', 'IdentitiesOnly=yes', '-o', 'StrictHostKeyChecking=yes',
            '-o', 'UserKnownHostsFile=' + s['known_hosts'], '-o', 'ConnectTimeout=15',
            '-o', 'ServerAliveInterval=10', '-o', 'ServerAliveCountMax=3', s['user'] + '@127.0.0.1']


def clean_env(home, tmp, config):
    env = dict(PATH='/usr/bin:/bin:/usr/sbin:/sbin', HOME=str(home), TMPDIR=str(tmp),
               LANG='en_US.UTF-8', LC_ALL='en_US.UTF-8', TZ='UTC')
    allowed = {'DEVELOPER_DIR', 'ANDROID_HOME', 'ANDROID_SDK_ROOT', 'MARATHON_XCTEST_PARSER_DIR'}
    additions = config.get('environment', {})
    require(set(additions) <= allowed, 'unapproved environment key')
    env.update(additions)
    if config.get('java_home'):
        env['JAVA_HOME'] = config['java_home']
    if config['profile'] == 'linux-kvm':
        env.update(ADB_SERVER_SOCKET=f'tcp:127.0.0.1:{port(config["adb_port"])}',
                   ANDROID_ADB_SERVER_PORT=str(config['adb_port']))
    return env


def signal_group(pid, sig):
    try:
        os.killpg(pid, sig)
    except ProcessLookupError:
        pass


def execute(argv, cwd, env, log, timeout=120, marker=None, cancel_grace=90, guard=None):
    """Bounded process group, raw separate streams, signal only after an observed marker."""
    log = Path(log); log.mkdir(exist_ok=False)
    result = dict(argv=[str(x) for x in argv], cwd=str(cwd), env=env,
                  started=time.time(), exit=None, watchdog=False, interrupted=False,
                  marker_seen=False, signal=None, joined=False)
    save(log / 'process.json', result)
    proc = None
    try:
        with (log / 'stdout').open('wb') as out, (log / 'stderr').open('wb') as err:
            proc = subprocess.Popen(argv, cwd=cwd, env=env, stdin=subprocess.DEVNULL,
                                    stdout=out, stderr=err, start_new_session=True, umask=0o022)
            result['pid'] = proc.pid
            deadline = time.monotonic() + timeout
            next_guard = time.monotonic()
            while proc.poll() is None:
                if guard and time.monotonic() >= next_guard:
                    guard()
                    next_guard = time.monotonic() + 1
                if marker and not result['marker_seen']:
                    if any(marker in (log / name).read_bytes() for name in ('stdout', 'stderr')):
                        result.update(marker_seen=True, signal='SIGINT', signal_at=time.time())
                        signal_group(proc.pid, signal.SIGINT)
                        deadline = min(deadline, time.monotonic() + cancel_grace)
                if time.monotonic() >= deadline:
                    result['watchdog'] = True
                    break
                time.sleep(.05)
    except BaseException:
        result['interrupted'] = True
        raise
    finally:
        if proc is not None:
            # Descendants can still hold worker resources after the CLI exits. The
            # fresh session is ours; never kill by executable name or worker-wide PID.
            if proc.poll() is None:
                signal_group(proc.pid, signal.SIGTERM)
                try:
                    proc.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    signal_group(proc.pid, signal.SIGKILL)
            result['exit'] = proc.wait(timeout=10)
            try:
                os.killpg(proc.pid, 0)
            except ProcessLookupError:
                result['joined'] = True
            else:
                signal_group(proc.pid, signal.SIGKILL)
                result['descendants_reclaimed'] = True
                result['joined'] = False
        result['finished'] = time.time()
        save(log / 'process.json', result)
    if marker:
        require(result['marker_seen'], 'blocked: cancellation start marker was not observed')
    return result


def file_sha(path):
    digest = hashlib.sha256()
    with Path(path).open('rb') as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            digest.update(block)
    return digest.hexdigest()


def inventory(root):
    return e.snapshot(root, 'tree')[0]


def check_selection(scenario, selection, discovery):
    require(selection['discovery_sha256'] == e.sha(Path(discovery).read_bytes()), 'discovery hash differs')
    require(selection.get('review'), 'selection requires reviewed JVM discovery')
    tests = selection['tests']
    require(isinstance(tests, list) and tests and len(set(tests)) == len(tests), 'empty/duplicate selection')
    methods = set(scenario['methods'])
    selected = set()
    for name in tests:
        require(isinstance(name, str) and re.fullmatch(r'[\w.$]+#[\w]+', name), 'invalid qualified test name')
        method = name.rsplit('.', 1)[-1]
        require(method in methods and method not in selected, 'unexpected/duplicate selected method')
        selected.add(method)
    omitted = selection.get('excluded_methods', {})
    require(set(omitted) == methods - selected and all(isinstance(v, str) and v.strip() for v in omitted.values()),
            'every discovery omission needs a JVM explanation and review')
    require(not omitted or scenario['vendor'] == 'Android', 'Apple workload must discover every requested test')
    return tests
