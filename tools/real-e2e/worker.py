#!/usr/bin/env python3
"""Opt-in worker lease. JSON-lines protocol; stdout is reserved for responses.

Run only on the reserved worker. An EOF, signal, or failed operation reclaims
owned resources. It never enables services or changes host SIP/TCC permissions.
"""
import os
from pathlib import Path
import platform
import re
import shutil
import signal
import socket
import subprocess
import sys
import time
import uuid

sys.path.insert(0, str(Path(__file__).resolve().parent))
from common import ROOT, e, file_sha, harness_hashes, inventory, port, require, save

LOCK = Path('/tmp/marathon-real-e2e.lock')
WORKER_ROOT = Path('/tmp/marathon')
USB_DEVICES = Path('/sys/bus/usb/devices')


def require_no_adb_usb(usb):
    """Block only USB devices with an ADB-capable interface (ff/42/01).

    The guard keeps the run away from real Android devices; an unrelated
    reader, Bluetooth or storage bridge on a dedicated desktop is harmless.
    Anything unreadable blocks, so it fails closed. Root hubs and devices
    appear as top-level entries; their interfaces are the 'bus-port:cfg.if' ones.
    """
    require(usb.is_dir(), 'blocked: cannot inspect USB inventory')
    try:
        interfaces = [p for p in usb.iterdir() if ':' in p.name]
        for interface in interfaces:
            found = tuple((interface / name).read_text().strip().lower()
                          for name in ('bInterfaceClass', 'bInterfaceSubClass', 'bInterfaceProtocol'))
            require(found != ('ff', '42', '01'), 'blocked: attached USB device with an ADB interface; reserve an isolated worker')
    except OSError as error:
        raise e.Invalid('blocked: cannot read USB interface descriptors: ' + str(error))


def command(argv, timeout=120, env=None, input=None):
    if env is None:
        env = dict(os.environ, PATH='/usr/bin:/bin:/usr/sbin:/sbin')
    process = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                               env=env, start_new_session=True)
    try:
        out, err = process.communicate(input=input, timeout=timeout)
    except BaseException:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        process.communicate(timeout=10)
        raise
    result = subprocess.CompletedProcess(argv, process.returncode, out, err)
    require(result.returncode == 0, f'blocked: {argv[0]} returned {result.returncode}: {result.stderr.decode(errors="replace")}')
    return result.stdout.decode('utf-8')


def active_tools():
    names = {'xcodebuild', 'xctest', 'ffmpeg', 'recordVideo', 'log', 'sample-app',
             'sample-appUITests-Runner', 'AppleWorld', 'AppleWorldUITests-Runner'}
    result = {}
    for line in command(['ps', '-axo', 'pid=,comm=']).splitlines():
        fields = line.strip().split(None, 1)
        require(len(fields) == 2 and fields[0].isdigit(), 'cannot inspect worker processes')
        if Path(fields[1]).name in names:
            result[fields[0]] = fields[1]
    return result


def simulator_inventory():
    rows = e.loads(command(['xcrun', 'simctl', 'list', 'devices', '--json']))['devices']
    return {d['udid']: dict(runtime=runtime, name=d['name'], state=d['state'])
            for runtime, devices in rows.items() for d in devices}


def security(home):
    result = {'sip': command(['csrutil', 'status']), 'tcc': {}}
    # A18 is complete-line membership, not substring matching. A real response
    # that would permit writes makes this host unsuitable for the approved run.
    require('disabled' not in result['sip'].splitlines(), 'blocked: permission writes would be reachable')
    paths = {'system': Path('/Library/Application Support/com.apple.TCC/TCC.db'),
             'user': Path(home) / 'Library/Application Support/com.apple.TCC/TCC.db'}
    for name, path in paths.items():
        # A host with no per-user grants has no user database; absence is a
        # state too, and a database appearing during the run still fails the
        # before/after equality check.
        if not path.parent.exists():
            result['tcc'][name] = 'absent'
            continue
        value = command(['sqlite3', '-readonly', str(path), 'SELECT count(*) FROM access;']).strip()
        require(re.fullmatch(r'\d+', value), 'unreadable TCC count')
        result['tcc'][name] = int(value)
    return result


def group_alive(pgid):
    """True while any process is left in the process group `pgid` (a zombie leader counts until it is waited for)."""
    try:
        os.killpg(pgid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True  # a group exists but is not ours to signal: not gone
    return True


def process_table():
    """pid -> (pgid, stat) for every process (ps is the one portable way to see a zombie without reaping it)."""
    table = subprocess.run(['ps', '-A', '-o', 'pid=,pgid=,stat='], capture_output=True, text=True, timeout=30,
                           env=dict(os.environ, PATH='/usr/bin:/bin'))
    require(table.returncode == 0, 'cannot inspect processes: ' + table.stderr.strip())
    rows = {}
    for line in table.stdout.splitlines():
        fields = line.split()
        require(len(fields) == 3 and fields[0].isdigit() and fields[1].isdigit(), 'cannot inspect processes')
        rows[int(fields[0])] = (int(fields[1]), fields[2])
    return rows


class OwnedChild(subprocess.Popen):
    """A child that leads its own process group (start_new_session) and is not reaped until its group is gone.

    While the leader is unreaped (a zombie at worst) the kernel cannot hand its id to another process or group, so
    `pgid == pid` keeps meaning "ours". `poll` therefore only looks, and `reclaim_group` reaps the leader after the
    group is empty. An exited-but-unreaped leader polls as -1: its real status is collected at the reap.
    """
    lost = False

    def poll(self):
        if self.returncode is not None:
            return self.returncode
        row = process_table().get(self.pid)
        if row is None:
            self.lost = True  # reaped behind our back: the id may already belong to someone else
            return super().poll()
        return -1 if row[1].startswith('Z') else None


def group_members(pgid, leader):
    """Live processes in `pgid` other than its leader (zombies are already dead and only await their parent)."""
    return [pid for pid, (group, stat) in process_table().items()
            if group == pgid and pid != leader and not stat.startswith('Z')]


def reclaim_group(child, grace=10):
    """Terminate and verify the whole process group of an owned child, then reap its leader.

    The leader exiting says nothing about its descendants: an emulator or adb helper can outlive it in the same
    group. Signals are sent only while the leader is still unreaped (see OwnedChild), so they cannot reach a
    group that merely reuses its number. If ownership cannot be shown, nothing is signalled and cleanup fails.
    """
    pgid = child.pid
    require(isinstance(child, OwnedChild) and not child.lost and child.returncode is None,
            'cannot establish ownership of process group %d: its leader was already reaped' % pgid)
    def settled():
        exited = child.poll() is not None
        # Polling can be what discovers that the leader was reaped elsewhere; from then on the number is not ours.
        require(not child.lost, 'lost ownership of process group %d: its leader was reaped elsewhere' % pgid)
        return exited and not group_members(pgid, pgid)
    for sig in (signal.SIGTERM, signal.SIGKILL):
        if settled():
            break
        try:
            os.killpg(pgid, sig)
        except ProcessLookupError:
            pass
        deadline = time.monotonic() + grace
        while time.monotonic() < deadline and not settled():
            time.sleep(.1)
    require(settled(), 'owned process group %d still has running members' % pgid)
    child.wait(timeout=10)


def adb_devices(endpoint):
    """Read the already-running private server directly; never auto-start adb."""
    with socket.create_connection(('127.0.0.1', port(endpoint)), timeout=5) as sock:
        def read(n):
            value = b''
            while len(value) < n:
                part = sock.recv(n - len(value))
                require(part, 'truncated ADB inventory')
                value += part
            return value
        request = b'host:devices'
        sock.sendall(f'{len(request):04x}'.encode() + request)
        require(read(4) == b'OKAY', 'ADB inventory failed')
        data = read(int(read(4), 16)).decode()
    rows = [line.split('\t') for line in data.splitlines()]
    require(all(len(row) == 2 for row in rows), 'malformed ADB inventory')
    return dict(rows)


def require_port_free(number):
    # Connections lingering in TIME_WAIT from the previous capture's own server make a plain bind
    # fail for a minute. Only something that accepts connections makes the port busy.
    with socket.socket() as probe:
        probe.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        try:
            probe.bind(('127.0.0.1', number))
            return
        except OSError:
            pass
    try:
        socket.create_connection(('127.0.0.1', number), timeout=2).close()
    except OSError:
        return
    raise e.Invalid(f'blocked: port {number} is in use')


def adb_server_argv(sdk, adb_port):
    # adb 1.0.41 rejects a host in -L ("listening on specified hostname currently unsupported");
    # without -a it listens on the loopback interface only.
    return [str(Path(sdk) / 'platform-tools/adb'), '-L', f'tcp:{port(adb_port)}', 'nodaemon', 'server']


def owned_devices(actual, serial):
    require(actual == {serial: 'device'}, f'blocked: private ADB inventory is not exactly the owned emulator (saw {actual})')


class Lease:
    def __init__(self, config, scenario, run_id):
        require(re.fullmatch(r'[0-9a-f]{32}', run_id), 'invalid ownership token')
        self.config, self.scenario, self.run_id = config, scenario, run_id
        self.root = ROOT / 'tools/real-e2e/runs/worker' / run_id
        self.root.mkdir(parents=True, mode=0o700, exist_ok=False)
        save(self.root / 'request.json', {'config': config, 'scenario': scenario, 'run_id': run_id})
        self.children = []
        self.udid = None
        self.locked = False
        self.before = None
        self.baseline = None
        self.cleaned = False
        self.recovered = False
        self.baseline_accepted = False
        self.events = []

    @classmethod
    def recover(cls, run_id):
        require(re.fullmatch(r'[0-9a-f]{32}', run_id), 'invalid ownership token')
        root = ROOT / 'tools/real-e2e/runs/worker' / run_id
        require(not root.is_symlink(), 'linked recovery directory')
        request = e.load(root / 'request.json')
        events = e.load(root / 'journal.json')
        require(LOCK.is_dir() and not LOCK.is_symlink(), 'no retained reservation')
        owner = e.load(LOCK / 'owner.json')
        require(owner['token'] == run_id, 'foreign reservation')
        # Never kill by a stale PID. Recovery is allowed only after the original
        # supervisor and its recorded children are already gone; PID reuse blocks.
        for pid in [owner['pid']] + [x['pid'] for x in events if x['event'] == 'process-created']:
            try:
                os.kill(pid, 0)
            except ProcessLookupError:
                pass
            else:
                raise e.Invalid('blocked: recorded process still exists; inspect ownership before manual intervention')
        # A child's descendants share its process group; they can outlive the child itself.
        for pid in [x['pid'] for x in events if x['event'] == 'process-created']:
            if group_alive(pid):
                raise e.Invalid('blocked: recorded process group still has members; inspect ownership before manual intervention')
        lease = cls.__new__(cls)
        lease.config, lease.scenario, lease.run_id = request['config'], request['scenario'], run_id
        lease.root, lease.events, lease.children = root, events, []
        lease.locked, lease.cleaned, lease.recovered = True, False, True
        lease.baseline_accepted = any(x['event'] == 'reserved' for x in events)
        lease.before = next((x['facts'] for x in events if x['event'] == 'preflight'), None)
        lease.baseline = lease.before.get('simulators') if lease.before else None
        lease.udid = next((x['udid'] for x in events if x['event'] == 'simulator-created'), None)
        require(not any(x['event'] == 'create-simulator-requested' for x in events) or lease.udid,
                'blocked: create was interrupted before its returned UDID was journalled; inspect named allocation')
        return lease

    def event(self, event_name, **fields):
        self.events.append(dict(event=event_name, at=time.time(), **fields))
        save(self.root / 'journal.json', self.events)

    def preflight(self):
        c = self.config
        require(set(c.get('environment', {})) <= {'DEVELOPER_DIR', 'ANDROID_HOME', 'ANDROID_SDK_ROOT'}, 'unapproved worker environment')
        os.environ.update(c.get('environment', {}))
        require(c.get('harness_sha256') == harness_hashes(), 'blocked: worker harness revision differs')
        require(Path(c['worker_home']).resolve() == Path.home().resolve(), 'blocked: worker account/home differs')
        expected = 'Linux' if self.scenario['vendor'] == 'Android' else 'Darwin'
        require(platform.system() == expected, 'blocked: wrong worker OS')
        require(not WORKER_ROOT.exists() and not WORKER_ROOT.is_symlink(), 'blocked: occupied /tmp/marathon')
        require(not LOCK.exists() and not LOCK.is_symlink(), 'blocked: worker already reserved')
        facts = dict(os=command(['uname', '-a']), worker_id=c['worker_id'])
        if expected == 'Darwin':
            facts.update(xcode=command(['xcodebuild', '-version']), sdk=command(['xcodebuild', '-showsdks']),
                         rsync=command(['rsync', '--version']), simulators=simulator_inventory(), processes=active_tools())
            expected_xcode = '27' if c['profile'] == 'local-27' else '26.6'
            require(re.search(r'^Xcode ' + re.escape(expected_xcode) + r'(?:\.|\s|$)', facts['xcode']),
                    'blocked: Xcode profile mismatch')
            if self.scenario['vendor'] == 'macOS':
                facts['security'] = security(c['worker_home'])
        else:
            require(Path('/dev/kvm').exists() and os.access('/dev/kvm', os.R_OK | os.W_OK), 'blocked: KVM access missing')
            # A different port does not isolate USB or emulator scanning. The
            # operator must reserve a dedicated empty worker, not just a port.
            require(c.get('exclusive_android_worker') is True and c.get('reservation_review'),
                    'blocked: approved exclusive Android worker reservation required')
            processes = command(['ps', '-eo', 'args='])
            require(not re.search(r'(?m)^(?:\S*/)?(?:adb|emulator|qemu-system-\S*)(?:\s|$)', processes),
                    'blocked: existing adb/emulator process')
            require_no_adb_usb(USB_DEVICES)
            facts.update(kvm=True, image=c['image'], reservation_review=c['reservation_review'])
            sdk = Path(c['sdk'])
            image = sdk.joinpath(*c['image'].split(';'))
            require(image.is_dir(), 'blocked: system image is not installed')
            facts['image_hashes'] = {str(p.relative_to(image)): file_sha(p) for p in sorted(image.rglob('*')) if p.is_file()}
            require(facts['image_hashes'], 'blocked: empty system image')
            facts['adb'] = command([str(sdk / 'platform-tools/adb'), 'version'])
            facts['emulator'] = command([str(sdk / 'emulator/emulator'), '-version'])
            for n in (5037, port(c['adb_port']), port(c['emulator_port']), c['emulator_port'] + 1):
                require_port_free(n)
        self.event('preflight', facts=facts)
        return facts

    def prepare(self):
        self.before = self.preflight()
        LOCK.mkdir(mode=0o700)
        self.locked = True
        save(LOCK / 'owner.json', {'token': self.run_id, 'pid': os.getpid(), 'evidence': str(self.root)})
        # Recheck after the atomic reservation; no baseline is ever erased.
        require(not WORKER_ROOT.exists() and not WORKER_ROOT.is_symlink(), 'worker became occupied')
        self.baseline_accepted = True
        self.event('reserved', baseline='absent')
        vendor = self.scenario['vendor']
        if vendor == 'iOS':
            self.baseline = self.before['simulators']
            c = self.config
            name = 'mrs-real-e2e-' + self.run_id
            self.event('create-simulator-requested', name=name)
            self.udid = command(['xcrun', 'simctl', 'create', name, c['device_type'], c['runtime']]).strip()
            require(re.fullmatch(r'[0-9A-Fa-f-]{36}', self.udid), 'invalid returned simulator UDID')
            self.event('simulator-created', udid=self.udid)
        elif vendor == 'Android':
            self.start_android()
        return dict(root=str(self.root), udid=self.udid, before=self.before,
                    serial=('emulator-' + str(self.config['emulator_port'])) if vendor == 'Android' else None)

    def spawn(self, argv, env, label):
        out = (self.root / (label + '.stdout')).open('wb')
        err = (self.root / (label + '.stderr')).open('wb')
        try:
            child = OwnedChild(argv, env=env, stdin=subprocess.DEVNULL, stdout=out, stderr=err,
                                     start_new_session=True)
        finally:
            out.close(); err.close()
        self.children.append(child)
        self.event('process-created', label=label, pid=child.pid, argv=argv)
        return child

    def start_android(self):
        c = self.config; sdk = Path(c['sdk'])
        port(c['adb_port']); port(c['emulator_port'])
        home = self.root / 'home'; home.mkdir()
        avds = self.root / 'avds'; avds.mkdir()
        env = dict(PATH='/usr/bin:/bin', HOME=str(home), JAVA_HOME=c['java_home'],
                   ANDROID_HOME=str(sdk), ANDROID_SDK_ROOT=str(sdk), ANDROID_AVD_HOME=str(avds),
                   ANDROID_USER_HOME=str(home / '.android'), ANDROID_EMULATOR_HOME=str(home / '.android'),
                   ADB_SERVER_SOCKET=f'tcp:127.0.0.1:{port(c["adb_port"])}',
                   ANDROID_ADB_SERVER_PORT=str(c['adb_port']))
        manager = str(sdk / 'cmdline-tools/latest/bin/avdmanager')
        name = 'mrs-' + self.run_id
        command([manager, 'create', 'avd', '-n', name, '-k', c['image'], '-p', str(avds / name)],
                timeout=120, env=env, input=b'no\n')
        self.event('avd-created', name=name, image=c['image'])
        server = self.spawn(adb_server_argv(sdk, c['adb_port']), env, 'adb')
        deadline = time.monotonic() + 20
        while True:
            require(server.poll() is None and time.monotonic() < deadline, 'private ADB startup failed')
            try:
                require(adb_devices(c['adb_port']) == {}, 'private ADB discovered a foreign device')
                break
            except (ConnectionError, socket.timeout):
                time.sleep(.1)
        self.spawn([str(sdk / 'emulator/emulator'), '-avd', name, '-port', str(c['emulator_port']),
                    '-no-snapshot', '-no-window', '-no-audio', '-no-boot-anim'], env, 'emulator')
        serial = 'emulator-' + str(c['emulator_port'])
        deadline = time.monotonic() + 240
        while time.monotonic() < deadline:
            require(all(p.poll() is None for p in self.children), 'owned Android process exited')
            devices = adb_devices(c['adb_port'])
            require(set(devices) <= {serial}, 'foreign serial detected')
            if devices == {serial: 'device'}:
                boot = command([str(sdk / 'platform-tools/adb'), '-H', '127.0.0.1', '-P', str(c['adb_port']),
                                '-s', serial, 'shell', 'getprop', 'sys.boot_completed'], env=env).strip()
                if boot == '1':
                    self.wait_stable(serial, deadline)
                    self.event('android-ready', serial=serial)
                    return
            time.sleep(1)
        raise e.Invalid('owned emulator boot deadline')

    def wait_stable(self, serial, deadline, window=20):
        # sys.boot_completed is not the end of boot: the transport was seen going offline for a moment
        # within ~15 s of it, which the guard rightly treats as a foreign state. Require a quiet window.
        stable_since = time.monotonic()
        while time.monotonic() - stable_since < window:
            require(time.monotonic() < deadline, 'owned emulator boot deadline')
            require(all(p.poll() is None for p in self.children), 'owned Android process exited')
            devices = adb_devices(self.config['adb_port'])
            require(set(devices) <= {serial}, 'foreign serial detected')
            if devices != {serial: 'device'}:
                self.event('android-unstable', saw=devices)
                stable_since = time.monotonic()
            time.sleep(1)

    def guard(self):
        require(self.locked and e.load(LOCK / 'owner.json')['token'] == self.run_id, 'lost worker reservation')
        if self.scenario['vendor'] == 'Android':
            require(all(p.poll() is None for p in self.children), 'owned Android process exited')
            owned_devices(adb_devices(self.config['adb_port']), 'emulator-' + str(self.config['emulator_port']))
        return {'ok': True}

    def inspect(self):
        require(self.locked and e.load(LOCK / 'owner.json')['token'] == self.run_id, 'lost worker reservation')
        result = {'at': time.time(), 'worker_tree': inventory(WORKER_ROOT) if WORKER_ROOT.exists() else []}
        vendor = self.scenario['vendor']
        if vendor != 'Android':
            result['processes'] = active_tools()
            require(result['processes'] == self.before['processes'], 'worker tool process inventory changed; inspect before reclamation')
        if vendor == 'iOS':
            actual = simulator_inventory()
            require({k: v for k, v in actual.items() if k != self.udid} == self.baseline,
                    'foreign simulator changed or unexpected recovery allocation')
            require(self.udid in actual, 'owned simulator disappeared')
            result['simulators'] = actual
        elif vendor == 'macOS':
            result['security'] = security(self.config['worker_home'])
            require(result['security'] == self.before['security'], 'SIP/TCC state changed; retain evidence, do not restore it')
        else:
            # A start-up failure before android-ready leaves no server to inspect; the recorded
            # children are still joined, so the failed run must remain recoverable.
            # A recovered lease has no live server by construction (recovery requires every recorded child gone).
            ready = any(x['event'] == 'android-ready' for x in self.events) and not self.recovered
            try:
                result['devices'] = adb_devices(self.config['adb_port'])
            except OSError:
                if ready:
                    raise
                result['devices'] = None
            if ready:
                owned_devices(result['devices'], 'emulator-' + str(self.config['emulator_port']))
        self.event('inspection', observation=result)
        return result

    def wait_emulator_helpers(self, timeout=60):
        # The emulator leaves its own `emulator -kill <pid> -sleep 20` watcher behind for about 20 s;
        # the next capture's preflight would read it as a foreign emulator process.
        pids = [x['pid'] for x in self.events if x['event'] == 'process-created' and x['label'] == 'emulator']
        deadline = time.monotonic() + timeout
        while pids:
            rows = command(['ps', '-eo', 'args=']).splitlines()
            left = [row for row in rows if any(re.search(rf'emulator\s+-kill\s+{pid}\b', row) for pid in pids)]
            if not left:
                return
            require(time.monotonic() < deadline, 'owned emulator kill helper still running: ' + left[0])
            time.sleep(1)

    def cleanup(self):
        if self.cleaned:
            return e.load(self.root / 'cleanup.json')
        errors = []
        result = dict(complete=False, errors=errors, root=str(self.root))
        if self.locked:
            try:
                result['runner_after'] = self.inspect()
            except Exception as error:
                errors.append(str(error))
            # Retain runner leftovers before harness reclamation, including A19.
            try:
                require(self.baseline_accepted, 'baseline was not reserved; do not reclaim worker root')
                require(e.load(LOCK / 'owner.json')['token'] == self.run_id, 'lost ownership')
                if self.scenario['vendor'] != 'Android':
                    require(active_tools() == self.before['processes'], 'tool processes remain; retain worker files in place')
                require(not WORKER_ROOT.is_symlink(), 'worker root became a symlink')
                if WORKER_ROOT.exists():
                    shutil.move(str(WORKER_ROOT), str(self.root / 'worker-tree'))
                    self.event('worker-tree-retained')
            except Exception as error:
                errors.append(str(error))
        for child in reversed(self.children):
            try:
                reclaim_group(child)
                self.event('process-joined', pid=child.pid, exit=child.returncode)
            except Exception as error:
                errors.append(str(error))
        if self.scenario['vendor'] == 'Android':
            try:
                self.wait_emulator_helpers()
            except Exception as error:
                errors.append(str(error))
        if self.udid:
            try:
                command(['xcrun', 'simctl', 'delete', self.udid], timeout=120)
                self.event('simulator-deleted', udid=self.udid)
                require(simulator_inventory() == self.baseline, 'post-cleanup simulator inventory differs')
            except Exception as error:
                errors.append(str(error))
        if self.locked and not errors:
            try:
                require(not WORKER_ROOT.exists() and not WORKER_ROOT.is_symlink(), 'worker cleanup incomplete')
                if self.scenario['vendor'] == 'macOS':
                    result['security_after'] = security(self.config['worker_home'])
                    require(result['security_after'] == self.before['security'], 'SIP/TCC changed during cleanup')
                if self.scenario['vendor'] == 'Android':
                    shutil.rmtree(self.root / 'avds')
                (LOCK / 'owner.json').unlink(); LOCK.rmdir()
                self.locked = False
            except Exception as error:
                errors.append(str(error))
        result['complete'] = not errors and not self.locked
        self.cleaned = True
        self.event('cleanup', result=result)
        save(self.root / 'cleanup.json', result)
        return result


def main():
    require(sys.argv[1:] == ['--approved'], 'worker operations require --approved')
    lease = None
    def stop(_sig, _frame):
        raise KeyboardInterrupt()
    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    try:
        for line in sys.stdin:
            try:
                request = e.loads(line)
                op = request['op']
                if op in ('prepare', 'preflight'):
                    require(lease is None, 'one invocation per worker session')
                    lease = Lease(request['config'], request['scenario'], request['run_id'])
                    value = lease.prepare() if op == 'prepare' else lease.preflight()
                elif op == 'recover':
                    require(lease is None, 'worker session already has a lease')
                    lease = Lease.recover(request['run_id'])
                    value = lease.cleanup()
                elif op == 'guard':
                    require(lease is not None, 'worker is not prepared')
                    value = lease.guard()
                elif op == 'inspect':
                    require(lease is not None, 'worker is not prepared')
                    value = lease.inspect()
                elif op == 'cleanup':
                    require(lease is not None, 'worker is not prepared')
                    value = lease.cleanup()
                else:
                    raise e.Invalid('unknown worker operation')
                print(e.encoded({'ok': True, 'value': value}).decode().replace('\n', ''), flush=True)
            except Exception as error:
                print(e.encoded({'ok': False, 'error': str(error)}).decode().replace('\n', ''), flush=True)
                break
    finally:
        if lease is not None:
            lease.cleanup()


if __name__ == '__main__':
    main()
