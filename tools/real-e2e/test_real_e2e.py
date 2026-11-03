#!/usr/bin/env python3
"""Offline guard/adapter tests. Commands are doubles or owned Python children."""
import copy
import importlib.machinery
import importlib.util
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from types import SimpleNamespace
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
import common as c
import configs
import worker as w
import build
REPO = c.ROOT
loader = importlib.machinery.SourceFileLoader('real_driver', str(c.HERE / 'run'))
spec = importlib.util.spec_from_loader(loader.name, loader)
driver = importlib.util.module_from_spec(spec); loader.exec_module(driver)


class Guards(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.host = dict(profile='local-27', worker_id='worker-role', worker_home=str(Path.home()), harness_sha256=c.harness_hashes(),
                         java_home='/unexecuted/jdk', watchdog_seconds=1800, runtime='runtime', device_type='type')
        self.scenario = c.SCENARIOS['ios-unit-nm']
        self.calls = []
        self.devices = {'FOREIGN': dict(runtime='runtime', name='user simulator', state='Booted')}
        self.patches = [patch.object(w, 'ROOT', self.root), patch.object(w, 'LOCK', self.root / 'lock'),
                        patch.object(w, 'WORKER_ROOT', self.root / 'marathon'),
                        patch.object(w.platform, 'system', return_value='Darwin'),
                        patch.object(w, 'simulator_inventory', side_effect=lambda: copy.deepcopy(self.devices)),
                        patch.object(w, 'command', side_effect=self.command)]
        for p in self.patches:
            p.start()
        self.addCleanup(self.temp.cleanup)
        for p in self.patches:
            self.addCleanup(p.stop)

    def command(self, argv, **_):
        self.calls.append(argv)
        if argv[:3] == ['xcrun', 'simctl', 'create']:
            udid = 'AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE'
            self.devices[udid] = dict(runtime='runtime', name=argv[3], state='Shutdown')
            return udid + '\n'
        if argv[:3] == ['xcrun', 'simctl', 'delete']:
            self.devices.pop(argv[3]); return ''
        if argv == ['ps', '-axo', 'pid=,comm=']:
            return '42 /usr/bin/python3\n'
        if argv == ['xcodebuild', '-version']:
            return 'Xcode 27.1\nBuild version fixture\n'
        if argv == ['csrutil', 'status']:
            return 'System Integrity Protection status: enabled.\n'
        if argv[0] == 'sqlite3':
            return '12\n'
        return 'fixture\n'

    def lease(self, scenario=None):
        return w.Lease(self.host, scenario or self.scenario, 'a' * 32)

    def test_occupied_worker_is_never_deleted_or_reserved(self):
        w.WORKER_ROOT.mkdir(); (w.WORKER_ROOT / 'foreign').write_bytes(b'keep')
        lease = self.lease()
        with self.assertRaisesRegex(c.e.Invalid, 'occupied'):
            lease.prepare()
        lease.cleanup()
        self.assertEqual((w.WORKER_ROOT / 'foreign').read_bytes(), b'keep')
        self.assertFalse(self.calls)
        self.assertFalse(w.LOCK.exists())

    def test_linked_worker_root_is_rejected(self):
        w.WORKER_ROOT.symlink_to(self.root / 'missing')
        with self.assertRaisesRegex(c.e.Invalid, 'occupied'):
            self.lease().prepare()
        self.assertTrue(w.WORKER_ROOT.is_symlink())

    def test_exclusive_lock_does_not_replace_foreign_owner(self):
        w.LOCK.mkdir(); c.save(w.LOCK / 'owner.json', {'token': 'foreign'})
        with self.assertRaisesRegex(c.e.Invalid, 'reserved'):
            self.lease().prepare()
        self.assertEqual(c.e.load(w.LOCK / 'owner.json'), {'token': 'foreign'})

    def test_fresh_simulator_owned_cleanup_retains_runner_leftovers(self):
        lease = self.lease(); prepared = lease.prepare()
        w.WORKER_ROOT.mkdir(); (w.WORKER_ROOT / 'a19-temp.mp4').write_bytes(b'original')
        result = lease.cleanup()
        self.assertTrue(result['complete'])
        self.assertEqual((lease.root / 'worker-tree/a19-temp.mp4').read_bytes(), b'original')
        self.assertEqual(set(self.devices), {'FOREIGN'})
        self.assertIn(['xcrun', 'simctl', 'delete', prepared['udid']], self.calls)
        self.assertFalse(w.LOCK.exists())
        self.assertEqual(lease.cleanup(), result)

    def test_unexpected_recovery_allocation_fails_scope_and_is_not_deleted(self):
        lease = self.lease(); lease.prepare()
        self.devices['UNEXPECTED'] = dict(runtime='runtime', name='unowned', state='Booted')
        result = lease.cleanup()
        self.assertFalse(result['complete'])
        self.assertTrue(w.LOCK.exists())
        self.assertIn('UNEXPECTED', self.devices)
        self.assertNotIn(['xcrun', 'simctl', 'delete', 'UNEXPECTED'], self.calls)

    def test_unattributed_worker_process_retains_files_and_reservation(self):
        lease = self.lease(); lease.prepare()
        w.WORKER_ROOT.mkdir(); (w.WORKER_ROOT / 'partial').write_bytes(b'evidence')
        with patch.object(w, 'active_tools', return_value={'900': '/usr/bin/xcodebuild'}):
            result = lease.cleanup()
        self.assertFalse(result['complete'])
        self.assertEqual((w.WORKER_ROOT / 'partial').read_bytes(), b'evidence')
        self.assertTrue(w.LOCK.exists())

    def test_foreign_simulator_state_change_is_not_restored(self):
        lease = self.lease(); lease.prepare()
        self.devices['FOREIGN']['state'] = 'Shutdown'
        self.assertFalse(lease.cleanup()['complete'])
        self.assertEqual(self.devices['FOREIGN']['state'], 'Shutdown')

    def test_failed_preflight_has_no_owned_resources_to_reclaim(self):
        lease = self.lease()
        with patch.object(w, 'command', side_effect=RuntimeError('missing Xcode')):
            with self.assertRaises(RuntimeError):
                lease.prepare()
        lease.cleanup()
        self.assertFalse(w.LOCK.exists())

    def test_recovery_rejects_live_or_reused_supervisor_pid(self):
        lease = self.lease(); lease.prepare()
        with patch.object(w.os, 'kill', return_value=None) as kill:
            with self.assertRaisesRegex(c.e.Invalid, 'recorded process still exists'):
                w.Lease.recover(lease.run_id)
        self.assertTrue(all(call.args[1] == 0 for call in kill.call_args_list))
        lease.cleanup()

    def test_recovery_reclaims_only_journalled_simulator_after_supervisor_exit(self):
        lease = self.lease(); lease.prepare()
        with patch.object(w.os, 'kill', side_effect=ProcessLookupError):
            recovered = w.Lease.recover(lease.run_id)
        self.assertTrue(recovered.cleanup()['complete'])
        self.assertEqual(set(self.devices), {'FOREIGN'})

    def test_post_reservation_race_never_moves_foreign_root(self):
        lease = self.lease()
        real_save = w.save
        def raced_save(path, value):
            real_save(path, value)
            if path == w.LOCK / 'owner.json':
                w.WORKER_ROOT.mkdir(); (w.WORKER_ROOT / 'foreign').write_text('keep')
        with patch.object(w, 'save', side_effect=raced_save):
            with self.assertRaisesRegex(c.e.Invalid, 'became occupied'):
                lease.prepare()
        self.assertFalse(lease.cleanup()['complete'])
        self.assertEqual((w.WORKER_ROOT / 'foreign').read_text(), 'keep')

    def test_mac_requires_readable_both_tcc_databases_and_no_state_change(self):
        lease = self.lease(c.SCENARIOS['macos-permissions']); lease.prepare()
        with patch.object(w, 'security', return_value={'sip': 'changed', 'tcc': {'system': 12, 'user': 13}}):
            result = lease.cleanup()
        self.assertFalse(result['complete'])
        self.assertTrue(w.LOCK.exists())
        self.assertTrue(all('-readonly' in x for x in self.calls if x[0] == 'sqlite3'))
        self.assertFalse(any('UPDATE' in str(x) or 'INSERT' in str(x) for x in self.calls))

    @unittest.skipUnless(sys.platform == 'darwin', 'TCC databases exist only on macOS')
    def test_tcc_permission_error_blocks_instead_of_becoming_zero(self):
        with patch.object(w, 'command', side_effect=["System Integrity Protection status: enabled.\n", c.e.Invalid('denied')]):
            with self.assertRaisesRegex(c.e.Invalid, 'denied'):
                w.security(self.host['worker_home'])

    def test_a18_guard_uses_complete_lines(self):
        with patch.object(w, 'command', side_effect=['disabled\n']):
            with self.assertRaisesRegex(c.e.Invalid, 'writes would be reachable'):
                w.security(self.host['worker_home'])
        # The per-user database directory must exist, whatever the machine running the test has.
        with tempfile.TemporaryDirectory() as home:
            (Path(home) / 'Library/Application Support/com.apple.TCC').mkdir(parents=True)
            with patch.object(w, 'command', side_effect=['System Integrity Protection status: disabled.\n', '1\n', '2\n']), \
                    patch.object(Path, 'exists', return_value=True):
                self.assertEqual(w.security(home)['tcc'], {'system': 1, 'user': 2})

    def test_default_adb_ports_foreign_and_offline_serials_rejected(self):
        for value in (5037, 0, -1, 65536, '5040', True):
            with self.assertRaises(c.e.Invalid):
                c.port(value)
        for devices in ({}, {'emulator-owned': 'offline'}, {'phone': 'device'},
                        {'emulator-owned': 'device', 'phone': 'device'}):
            with self.assertRaises(c.e.Invalid):
                w.owned_devices(devices, 'emulator-owned')
        w.owned_devices({'emulator-owned': 'device'}, 'emulator-owned')

    def test_host_config_refuses_non_loopback_and_default_forward(self):
        host = dict(self.host, profile='ssh-26.6', ssh={'host': 'unapproved', 'port': 5037})
        path = self.root / 'host.json'; c.save(path, host); path.chmod(0o600)
        with self.assertRaisesRegex(c.e.Invalid, 'loopback'):
            c.private_config(path)
        host['ssh']['host'] = '127.0.0.1'; c.save(path, host)
        with self.assertRaisesRegex(c.e.Invalid, 'port'):
            c.private_config(path)

    def test_android_preflight_requires_exclusive_reservation(self):
        lease = self.lease(c.SCENARIOS['android-local-video-sync'])
        with patch.object(w.platform, 'system', return_value='Linux'), patch.object(Path, 'exists', return_value=False):
            with self.assertRaisesRegex(c.e.Invalid, 'KVM'):
                lease.preflight()
        self.assertFalse(any('start-server' in x for x in self.calls))

    def test_port_probe_ignores_time_wait_but_not_a_listener(self):
        import socket
        listener = socket.socket(); listener.bind(('127.0.0.1', 0)); listener.listen()
        number = listener.getsockname()[1]
        with self.assertRaisesRegex(c.e.Invalid, 'in use'):
            w.require_port_free(number)
        client = socket.create_connection(('127.0.0.1', number))
        server, _ = listener.accept()
        server.close(); client.close(); listener.close()  # the closing side keeps the port in TIME_WAIT
        w.require_port_free(number)

    def test_private_adb_server_listens_without_a_hostname(self):
        argv = w.adb_server_argv('/sdk', 45037)
        self.assertEqual(argv[1:], ['-L', 'tcp:45037', 'nodaemon', 'server'])
        with self.assertRaises(c.e.Invalid):
            w.adb_server_argv('/sdk', 5037)

    def reserved_android(self, ready):
        lease = w.Lease(dict(self.host, adb_port=45037, emulator_port=5584), c.SCENARIOS['android-local-video-sync'], 'a' * 32)
        w.LOCK.mkdir(); c.save(w.LOCK / 'owner.json', {'token': lease.run_id})
        (lease.root / 'avds').mkdir()
        lease.locked = lease.baseline_accepted = True
        if ready:
            lease.event('android-ready', serial='emulator-5584')
        return lease

    def test_android_start_up_failure_before_ready_releases_the_reservation(self):
        lease = self.reserved_android(ready=False)
        with patch.object(w, 'adb_devices', side_effect=ConnectionRefusedError('refused')):
            result = lease.cleanup()
        self.assertTrue(result['complete'], result)
        self.assertFalse(w.LOCK.exists())

    def test_cleanup_waits_for_the_emulator_kill_helper(self):
        lease = self.reserved_android(ready=False)
        lease.event('process-created', label='emulator', pid=4242, argv=['emulator'])
        rows = ['101 /sdk/emulator/emulator -kill 4242 -sleep 20\n', '100 /usr/bin/python3\n']
        sleeps = []
        with patch.object(w, 'adb_devices', side_effect=ConnectionRefusedError('refused')), \
                patch.object(w, 'command', side_effect=rows), patch.object(w.time, 'sleep', side_effect=sleeps.append):
            result = lease.cleanup()
        self.assertTrue(result['complete'], result)
        self.assertEqual(sleeps, [1])

    def test_a_lingering_emulator_kill_helper_keeps_the_reservation(self):
        lease = self.reserved_android(ready=False)
        lease.event('process-created', label='emulator', pid=4242, argv=['emulator'])
        ticks = iter([0, 0, 61])
        with patch.object(w, 'adb_devices', side_effect=ConnectionRefusedError('refused')), \
                patch.object(w, 'command', return_value='101 emulator -kill 4242 -sleep 20\n'), \
                patch.object(w.time, 'sleep'), patch.object(w.time, 'monotonic', side_effect=lambda: next(ticks)):
            result = lease.cleanup()
        self.assertFalse(result['complete'])
        self.assertTrue(w.LOCK.exists())

    def spawn_orphaning_child(self, lease):
        """An owned child that exits at once, leaving a descendant in its process group.

        The child is only looked at (`poll` does not reap it), as the harness does.
        """
        code = 'import subprocess,sys; p = subprocess.Popen([sys.executable, "-I", "-c", "import time; time.sleep(60)"]); print(p.pid, flush=True)'
        child = lease.spawn([sys.executable, '-I', '-c', code], dict(os.environ), 'owned-test')
        deadline = time.monotonic() + 60
        while child.poll() is None and time.monotonic() < deadline:
            time.sleep(.02)
        descendant = int((lease.root / 'owned-test.stdout').read_text().strip())
        def reap():
            try:
                os.killpg(child.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            try:
                child.wait(timeout=10)
            except Exception:
                pass
        self.addCleanup(reap)
        os.kill(descendant, 0)
        return child, descendant

    def test_cleanup_reclaims_a_descendant_that_outlives_its_child(self):
        # R1-sol-4: the leader's exit status said nothing about the emulator-style helper left in its group.
        lease = self.lease(); lease.prepare()
        child, descendant = self.spawn_orphaning_child(lease)
        self.assertIsNone(child.returncode)  # looking at the leader did not reap it
        result = lease.cleanup()
        self.assertTrue(result['complete'], result)
        self.assertFalse(w.LOCK.exists())
        self.assertIsNotNone(child.returncode)
        with self.assertRaises(ProcessLookupError):
            os.kill(descendant, 0)

    def test_a_descendant_that_survives_reclamation_keeps_the_reservation(self):
        lease = self.lease(); lease.prepare()
        child, _ = self.spawn_orphaning_child(lease)
        clock = [0]
        def monotonic():
            clock[0] += 4
            return clock[0]
        with patch.object(w, 'group_members', return_value=[child.pid + 1]), patch.object(w.os, 'killpg') as killpg, \
                patch.object(w.time, 'sleep'), patch.object(w.time, 'monotonic', side_effect=monotonic):
            result = lease.cleanup()
        self.assertFalse(result['complete'])
        self.assertTrue(any('still has running members' in error for error in result['errors']), result)
        self.assertTrue(w.LOCK.exists())
        self.assertEqual([call.args for call in killpg.call_args_list], [(child.pid, signal.SIGTERM), (child.pid, signal.SIGKILL)])
        self.assertIsNone(child.returncode)  # still unreaped, so its group id stays reserved

    def test_a_reaped_leader_whose_id_was_reused_is_never_signalled(self):
        # R2-sol-1: the numeric pid of a child that was already reaped says nothing about who owns that group now.
        old = w.OwnedChild([sys.executable, '-I', '-c', 'pass'], start_new_session=True)
        old.wait(timeout=10)
        foreign = subprocess.Popen([sys.executable, '-I', '-c', 'import time; time.sleep(60)'], start_new_session=True)
        self.addCleanup(lambda: (os.killpg(foreign.pid, signal.SIGKILL), foreign.wait(timeout=10)))
        old.pid = foreign.pid  # the operating system recycled the number
        lease = self.lease(); lease.prepare()
        lease.children.append(old)
        with patch.object(w.os, 'killpg') as killpg:
            with self.assertRaisesRegex(c.e.Invalid, 'cannot establish ownership'):
                w.reclaim_group(old, grace=0)
            result = lease.cleanup()
        self.assertEqual(killpg.call_args_list, [])
        self.assertFalse(result['complete'])
        self.assertTrue(w.LOCK.exists())
        os.kill(foreign.pid, 0)

    def test_a_leader_reaped_behind_the_harness_is_not_owned_either(self):
        lease = self.lease(); lease.prepare()
        child = lease.spawn([sys.executable, '-I', '-c', 'pass'], dict(os.environ), 'reaped')
        deadline = time.monotonic() + 60
        while child.poll() is None and time.monotonic() < deadline:
            time.sleep(.02)
        os.waitpid(child.pid, 0)  # someone else collected it
        child.poll()
        self.assertTrue(child.lost)
        with patch.object(w.os, 'killpg') as killpg:
            result = lease.cleanup()
        self.assertEqual(killpg.call_args_list, [])
        self.assertFalse(result['complete'])

    def test_ownership_lost_during_reclamation_stops_before_any_signal(self):
        # R3-sol-2: no poll before reclaim_group, so it is reclamation's own poll that finds the leader gone.
        lease = self.lease(); lease.prepare()
        child = lease.spawn([sys.executable, '-I', '-c', 'pass'], dict(os.environ), 'reaped-unpolled')
        os.waitpid(child.pid, 0)  # someone else collected it before the harness looked
        self.assertFalse(child.lost)
        with patch.object(w, 'group_members', return_value=[child.pid + 1]), patch.object(w.os, 'killpg') as killpg:
            with self.assertRaisesRegex(c.e.Invalid, 'lost ownership'):
                w.reclaim_group(child, grace=0)
            result = lease.cleanup()
        self.assertEqual(killpg.call_args_list, [])
        self.assertFalse(result['complete'])
        self.assertTrue(w.LOCK.exists())

    def test_recovery_rejects_a_recorded_process_group_that_still_has_members(self):
        lease = self.lease(); lease.prepare()
        child, descendant = self.spawn_orphaning_child(lease)
        lease.event('process-created', label='owned-test', pid=child.pid, argv=['x'])
        lease.children.clear()  # as after a supervisor crash: no Popen handle, only the journal
        real_kill = os.kill
        def kill(pid, sig):  # the supervisor (this process) and the dead child count as gone; the group does not
            if pid in (os.getpid(), child.pid):
                raise ProcessLookupError
            real_kill(pid, sig)
        with patch.object(w.os, 'kill', side_effect=kill), \
                self.assertRaisesRegex(c.e.Invalid, 'process group still has members'):
            w.Lease.recover(lease.run_id)
        os.killpg(child.pid, signal.SIGKILL)
        child.wait(timeout=10)
        deadline = time.monotonic() + 60
        while w.group_alive(child.pid) and time.monotonic() < deadline:
            time.sleep(.05)
        lease.cleanup()

    def test_recovering_a_ready_android_run_needs_no_live_server(self):
        lease = self.reserved_android(ready=True)
        lease.event('reserved', baseline='absent')
        lease.event('process-created', label='adb', pid=2 ** 22 + 12345, argv=['adb'])
        c.save(w.LOCK / 'owner.json', {'token': lease.run_id, 'pid': 2 ** 22 + 12346})
        with patch.object(w, 'adb_devices', side_effect=ConnectionRefusedError('refused')):
            result = w.Lease.recover(lease.run_id).cleanup()
        self.assertTrue(result['complete'], result)
        self.assertFalse(w.LOCK.exists())

    def test_foreign_inventory_is_named_in_the_error(self):
        with self.assertRaisesRegex(c.e.Invalid, r"saw \{'phone': 'device'\}"):
            w.owned_devices({'phone': 'device'}, 'emulator-owned')

    def test_boot_must_stay_quiet_for_a_window_before_ready(self):
        lease = self.reserved_android(ready=False)
        clock = [0.0]
        seen = iter([{'emulator-5584': 'device'}] * 5 + [{'emulator-5584': 'offline'}] + [{'emulator-5584': 'device'}] * 40)
        def sleep(seconds): clock[0] += seconds
        with patch.object(w, 'adb_devices', side_effect=lambda _: next(seen)), \
                patch.object(w.time, 'sleep', side_effect=sleep), patch.object(w.time, 'monotonic', side_effect=lambda: clock[0]):
            lease.wait_stable('emulator-5584', deadline=1000, window=20)
        self.assertEqual(clock[0], 25)  # the blip at t=5 restarts the window, which then needs twenty quiet seconds
        self.assertEqual([x['saw'] for x in lease.events if x['event'] == 'android-unstable'], [{'emulator-5584': 'offline'}])

    def test_boot_that_never_settles_hits_the_deadline(self):
        lease = self.reserved_android(ready=False)
        clock = [0.0]
        with patch.object(w, 'adb_devices', return_value={'emulator-5584': 'offline'}), \
                patch.object(w.time, 'sleep', side_effect=lambda s: clock.__setitem__(0, clock[0] + s)), \
                patch.object(w.time, 'monotonic', side_effect=lambda: clock[0]):
            with self.assertRaisesRegex(c.e.Invalid, 'boot deadline'):
                lease.wait_stable('emulator-5584', deadline=30, window=20)

    def test_android_inspection_failure_after_ready_keeps_the_reservation(self):
        lease = self.reserved_android(ready=True)
        with patch.object(w, 'adb_devices', side_effect=ConnectionRefusedError('refused')):
            result = lease.cleanup()
        self.assertFalse(result['complete'])
        self.assertTrue(w.LOCK.exists())


class UsbGuard(unittest.TestCase):
    def sysfs(self, root, interfaces):
        usb = root / 'devices'
        usb.mkdir()
        (usb / 'usb1').mkdir()
        for name, descriptor in interfaces.items():
            directory = usb / name
            directory.mkdir()
            if descriptor is not None:
                for file, value in zip(('bInterfaceClass', 'bInterfaceSubClass', 'bInterfaceProtocol'), descriptor):
                    (directory / file).write_text(value + '\n')
        return usb

    def test_desktop_without_adb_interfaces_passes(self):
        with tempfile.TemporaryDirectory() as directory:
            usb = self.sysfs(Path(directory), {'1-0:1.0': ('09', '00', '00'), '1-2:1.0': ('e0', '01', '01'),
                                               '1-3:1.0': ('08', '06', '50'), '1-4:1.0': ('ff', '00', '00'),
                                               '1-5:1.0': ('ff', '42', '00')})
            w.require_no_adb_usb(usb)

    def test_any_adb_interface_blocks(self):
        with tempfile.TemporaryDirectory() as directory:
            usb = self.sysfs(Path(directory), {'1-2:1.0': ('08', '06', '50'), '1-3:1.0': ('03', '00', '00'),
                                               '1-3:1.1': ('FF', '42', '01')})
            with self.assertRaisesRegex(c.e.Invalid, 'ADB interface'):
                w.require_no_adb_usb(usb)

    def test_unreadable_sysfs_blocks(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with self.assertRaisesRegex(c.e.Invalid, 'cannot inspect'):
                w.require_no_adb_usb(root / 'absent')
            # An interface whose descriptor files are missing cannot be classified.
            usb = self.sysfs(root, {'1-2:1.0': None})
            with self.assertRaisesRegex(c.e.Invalid, 'cannot read'):
                w.require_no_adb_usb(usb)


class Preparation(unittest.TestCase):
    def test_ledger_and_unchanged_fixtures(self):
        driver.validate()

    def test_all_scenario_templates_have_exact_safe_targets_and_features(self):
        host = dict(profile='local-27', sdk='/fake-sdk', adb_port=6000)
        for scenario in c.SCENARIOS.values():
            host['profile'] = scenario['profiles'][0]
            config, devices = configs.render(scenario, host, {'application': 'assets/app', 'tests': 'assets/tests'},
                                              ['pkg.' + x for x in scenario['methods']], {'udid': 'owned'})
            self.assertFalse(config['ignoreFailures'])
            self.assertFalse(config['analyticsTracking'])
            self.assertTrue(config['debug'])
            self.assertEqual(config['retryStrategy']['type'], 'no-retry')
            vendor = config['vendorConfiguration']
            if scenario['vendor'] == 'iOS':
                self.assertFalse(vendor['lifecycle']['shutdownUnused'])
                self.assertEqual(devices['workers'][0]['devices'], [{'type': 'simulator', 'udid': 'owned'}])
                self.assertEqual(vendor['xcresult']['pullingPolicy'], 'ALWAYS')
            if scenario['vendor'] == 'Android':
                self.assertEqual(vendor['adbServers'], [{'host': '127.0.0.1', 'port': 6000}])
                self.assertTrue(all(x['pathRoot'] == 'LOCAL_TMP' for x in vendor['fileSyncConfiguration']['push']))
                self.assertEqual(vendor['fileSyncConfiguration']['pull'][0]['aggregationMode'], 'TEST_RUN')

    def test_private_client_sdk_never_starts_a_server(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            sdk = Path(driver.private_cli_sdk(root))
            tripwire = root / 'attempt'
            result = subprocess.run([str(sdk / 'platform-tools/adb'), '-P', '6000', 'start-server'],
                                    env={'MRS_ADB_TRIPWIRE': str(tripwire)}, capture_output=True)
            self.assertEqual(result.returncode, 74)
            self.assertEqual(tripwire.read_text(), '-P\n6000\nstart-server\n')

    def test_jvm_selection_hash_and_omissions_are_not_inferred(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'discovery.yml'; path.write_bytes(b'JVM raw discovery')
            scenario = c.SCENARIOS['ios-unit-nm']
            selection = {'tests': ['pkg.' + x for x in scenario['methods']], 'review': 'review-ref',
                         'discovery_sha256': c.e.sha(path.read_bytes()), 'excluded_methods': {}}
            self.assertEqual(c.check_selection(scenario, selection, path), selection['tests'])
            selection['tests'].pop()
            with self.assertRaisesRegex(c.e.Invalid, 'omission'):
                c.check_selection(scenario, selection, path)
            selection['excluded_methods'] = {'SampleTests#testSkipping': 'not found'}
            with self.assertRaisesRegex(c.e.Invalid, 'Apple workload'):
                c.check_selection(scenario, selection, path)

    def test_build_recipe_uses_real_signing_and_no_real_test_execution(self):
        for vendor in ('iOS', 'macOS', 'Android'):
            recipe = build.recipes(vendor, Path('/scratch'), {})
            text = str(recipe)
            self.assertNotIn('CODE_SIGNING_ALLOWED=NO', text)
            self.assertNotIn('test-without-building', text)
            self.assertNotIn('simctl', text)
        self.assertIn('generic/platform=iOS Simulator', str(build.recipes('iOS', '/scratch', {})))

    def test_android_build_pins_only_buildsrc_for_jdk21_with_an_init_script(self):
        ((cwd, argv),) = build.recipes('Android', Path('/scratch'), {})
        self.assertEqual(cwd, Path('/scratch/sample/android-app'))
        self.assertEqual(argv[argv.index('--init-script') + 1], '/scratch/' + build.ANDROID_INIT_SCRIPT)
        script = (c.HERE / 'templates' / build.ANDROID_INIT_SCRIPT).read_text()
        self.assertIn("rootProject.name == 'buildSrc'", script)
        self.assertIn("jvmTarget = '17'", script)

    def test_android_build_gives_gradle_a_larger_heap_in_the_private_home(self):
        self.assertRegex(build.ANDROID_GRADLE_PROPERTIES, r'^org\.gradle\.jvmargs=-Xmx[3-9]g\n$')

    def test_macos_build_signs_locally_despite_the_fixture_project_disabling_signing(self):
        # The sample project sets CODE_SIGNING_ALLOWED=NO; only a command-line setting overrides it.
        project = (c.ROOT / 'sample/macos-app/AppleWorld.xcodeproj/project.pbxproj').read_text()
        self.assertIn('CODE_SIGNING_ALLOWED = NO;', project)
        ((_, argv),) = build.recipes('macOS', Path('/scratch'), {})
        self.assertIn('CODE_SIGNING_ALLOWED=YES', argv)
        self.assertIn('CODE_SIGN_IDENTITY=-', argv)
        for vendor in ('iOS', 'Android'):
            self.assertNotIn('CODE_SIGN_IDENTITY=-', str(build.recipes(vendor, Path('/scratch'), {})))

    def test_supervisor_timeout_is_harness_failure_and_reaps_owned_child(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            result = c.execute([sys.executable, '-I', '-c', 'import time; time.sleep(20)'], root, dict(os.environ), root / 'run', timeout=.1)
            self.assertTrue(result['watchdog'])
            self.assertTrue(result['joined'])
            with self.assertRaises(ProcessLookupError):
                os.kill(result['pid'], 0)

    def test_cancel_waits_for_observed_marker_and_preserves_streams(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            code = 'import signal,time; signal.signal(signal.SIGINT,lambda *_:exit(37)); print("START",flush=True); time.sleep(20)'
            result = c.execute([sys.executable, '-I', '-c', code], root, dict(os.environ), root / 'run', timeout=60, marker=b'START')
            self.assertTrue(result['marker_seen'])
            self.assertEqual(result['exit'], 37)
            self.assertFalse(result['watchdog'])
            self.assertEqual((root / 'run/stdout').read_bytes(), b'START\n')
            self.assertEqual((root / 'run/stderr').read_bytes(), b'')

    def test_inventory_guard_failure_terminates_cli_and_keeps_evidence(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with self.assertRaisesRegex(c.e.Invalid, 'foreign'):
                c.execute([sys.executable, '-I', '-c', 'import time;time.sleep(20)'], root, dict(os.environ), root / 'run',
                          guard=lambda: c.require(False, 'foreign serial'))
            process = c.e.load(root / 'run/process.json')
            self.assertTrue(process['interrupted'])
            self.assertTrue(process['joined'])

    def test_report_adapter_retains_empty_directories_modes_links_and_all_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); reports = root / 'reports'; reports.mkdir()
            (reports / 'empty').mkdir(); (reports / 'file').write_bytes(b'payload')
            (reports / 'link').symlink_to('file')
            outcome = dict(cli=1, fake=None, harness=0, watchdog=False, joined=True, unused=[], unmatched=[])
            contract = dict(format='marathon-differential/contract@1', scenario='artifact', roles={}, paths=['reports'], normalization={}, outcome=outcome)
            provenance = {'terminal_observation': outcome}
            c.e.write_record(root / 'left', contract, 'native', {'reports': reports}, {'stdout': b'ok', 'stderr': b'failure'}, outcome, provenance)
            c.e.write_record(root / 'right', contract, 'native', {'reports': reports}, {'stdout': b'ok', 'stderr': b'failure'}, outcome, provenance)
            c.e.compare(root / 'left', root / 'right', contract)
            record = c.e.load(root / 'right/evidence.json')
            record['entries'] = [x for x in record['entries'] if x['path'] != 'reports/empty']
            c.save(root / 'right/evidence.json', record)
            with self.assertRaisesRegex(c.e.Invalid, 'differences'):
                c.e.compare(root / 'left', root / 'right', contract)

    def test_real_commands_require_explicit_approval(self):
        result = subprocess.run([sys.executable, '-I', str(c.HERE / 'run'), 'preflight', '--host', '/nonexistent',
                                 '--scenario', 'ios-unit-nm', '--output', '/nonexistent'], capture_output=True)
        self.assertEqual(result.returncode, 2)
        self.assertIn(b'--approved', result.stderr)

    def test_whole_capture_pair_uses_production_argv_and_shared_comparator_with_fakes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); inputs = root / 'inputs'; (inputs / 'assets').mkdir(parents=True)
            (inputs / 'assets/app').write_bytes(b'app'); (inputs / 'assets/tests').write_bytes(b'tests')
            c.save(inputs / 'products.json', {'application': 'assets/app', 'tests': 'assets/tests'})
            c.save(inputs / 'build.json', {'source': c.e.SOURCE, 'products': ['fake-input-manifest'],
                   'commands': ['fake-build'], 'profile': 'local-27', 'worker_id': 'worker-role'})
            (inputs / 'discovery.yml').write_bytes(b'JVM-discovery-double')
            scenario = c.SCENARIOS['ios-unit-nm']
            c.save(inputs / 'selection.json', {'tests': ['pkg.' + x for x in scenario['methods']],
                   'review': 'offline-double', 'excluded_methods': {},
                   'discovery_sha256': c.e.sha((inputs / 'discovery.yml').read_bytes())})
            binary = root / 'install/bin/marathon'; binary.parent.mkdir(parents=True)
            binary.write_text('#!' + sys.executable + "\nfrom pathlib import Path\nimport sys\np=Path('report/test_result');p.mkdir(parents=True)\n(p/'raw.json').write_text('{}')\nprint('Marathon execution failed',file=sys.stderr)\nsys.exit(1)\n")
            binary.chmod(0o755)
            (root / 'install/lib').mkdir(); (root / 'install/lib/double.jar').write_bytes(b'not-a-real-jar')
            host = dict(profile='local-27', worker_id='worker-role', sorting_review='test',
                        jvm=str(binary), native=str(binary), java_home='/fake-jdk', watchdog_seconds=600,
                        jvm_reference=str(reference_record(root / 'reference.json', c.FROZEN, {'double.jar': c.e.sha(b'not-a-real-jar')})))
            calls = []
            class DoubleWorker:
                def __init__(self, *_): pass
                def __enter__(self): return self
                def __exit__(self, *_): pass
                def call(self, op, **kwargs):
                    calls.append(op)
                    if op == 'prepare':
                        return {'udid': kwargs['run_id'], 'before': {'xcode': 'fake'}}
                    if op == 'cleanup': return {'complete': True}
                    return {}
            from types import SimpleNamespace
            args = SimpleNamespace(scenario=scenario['id'], side='jvm', discover=False, output=root / 'pair', inputs=inputs, source=None)
            with patch.object(driver, 'Worker', DoubleWorker), patch.object(driver, 'java_version', return_value='JBR 21.0.10 double'):
                driver.capture(args, host)
                args.side = 'native'; driver.capture(args, host)
            pair = root / 'pair'
            c.e.compare(pair / 'jvm/evidence', pair / 'native/evidence', c.e.load(pair / 'contract.json'))
            self.assertEqual(calls.count('prepare'), 2)
            self.assertEqual(calls.count('cleanup'), 2)
            process = c.e.load(pair / 'jvm/capture.json')['process']
            self.assertEqual(process['argv'][1], 'run')
            self.assertIn('--analyticsTracking', process['argv'])
            self.assertEqual(process['exit'], 1)
            (inputs / 'assets/app').write_bytes(b'mutated')
            with self.assertRaises(c.e.Invalid):
                driver.capture(args, host)

    def fake_java(self, root, banner):
        (root / 'bin').mkdir()
        java = root / 'bin/java'
        java.write_text('#!/bin/sh\necho \'%s\' >&2\n' % banner)
        java.chmod(0o755)
        return root

    def test_linux_profile_accepts_any_jdk21_and_records_its_version(self):
        with tempfile.TemporaryDirectory() as tmp:
            home = self.fake_java(Path(tmp), 'openjdk version "21.0.12" 2026-07-21')
            self.assertIn('21.0.12', driver.java_version(home, 'linux-kvm'))

    def test_linux_profile_rejects_other_jdk_majors(self):
        with tempfile.TemporaryDirectory() as tmp:
            home = self.fake_java(Path(tmp), 'openjdk version "17.0.12" 2026-07-21')
            with self.assertRaisesRegex(c.e.Invalid, 'JDK 21 required'):
                driver.java_version(home, 'linux-kvm')

    def test_apple_profiles_keep_the_jbr_pin(self):
        with tempfile.TemporaryDirectory() as tmp:
            home = self.fake_java(Path(tmp), 'openjdk version "21.0.12" 2026-07-21')
            for profile in ('local-27', 'ssh-26.6'):
                with self.assertRaisesRegex(c.e.Invalid, 'JBR 21.0.10 required'):
                    driver.java_version(home, profile)
        with tempfile.TemporaryDirectory() as tmp:
            home = self.fake_java(Path(tmp), 'openjdk version "21.0.10" 2026-07-21')
            self.assertIn('21.0.10', driver.java_version(home, 'local-27'))

    def test_missing_qualified_policy_cannot_pass_release(self):
        result = subprocess.run([sys.executable, '-I', str(c.HERE / 'run'), 'compare', '/nonexistent',
                                 '--mismatch', '/nonexistent', '--release'], capture_output=True)
        self.assertEqual(result.returncode, 2)
        self.assertIn(b'blocked', result.stderr)


def reference_record(path, sha, jars):
    """A reference record as `run reference` writes it, for a fake distribution (the real PATCH_FILE hash)."""
    patches = {'0001-oracle-module.patch': c.e.sha((c.ROOT / c.PATCH_FILE).read_bytes())}
    receipt = dict(format=c.RECEIPT_FORMAT, variant='reference', source=sha, head=sha, verified_tree='1' * 40,
                   patches=patches, jars=jars, gradle_exit=0, java='double')
    c.save(path, dict(format=c.REFERENCE_FORMAT, source=sha, source_kind=c.source_kind(sha), jars=jars, receipt=receipt, patches=patches))
    return path


def release_build(sha, **changes):
    record = dict(source=sha, source_kind='release', products=['p'], commands=['c'],
                  samples=dict(origin='kotlin', paths={'sample/ios-app': 'a' * 40}, frozen={'sample/ios-app': 'a' * 40}, matches_frozen=True))
    record.update(changes)
    return record


class SourceSelection(unittest.TestCase):
    """WP 7.5: the frozen baseline is the default; the release source is explicit and one capture never mixes sources."""

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.kotlin = self.root / 'kotlin'
        self.frozen = self.commit({'sample/ios-app/a.swift': 'one', 'sample/android-app/b.kt': 'two', 'core/x.kt': 'v1'})
        self.release = self.commit({'core/x.kt': 'v2'})
        patches = [patch.object(c, 'FROZEN', self.frozen), patch.object(driver, 'FROZEN', self.frozen),
                   patch.object(c, 'release_sha', return_value=self.release), patch.object(driver, 'release_sha', return_value=self.release)]
        for p in patches:
            p.start(); self.addCleanup(p.stop)

    def git(self, *argv):
        return subprocess.run(['git', '-C', str(self.kotlin), '-c', 'user.name=t', '-c', 'user.email=t@example.invalid',
                               *argv], capture_output=True, text=True, check=True).stdout.strip()

    def commit(self, files):
        if not self.kotlin.exists():
            self.kotlin.mkdir(); self.git('init', '--quiet')
        for name, text in files.items():
            (self.kotlin / name).parent.mkdir(parents=True, exist_ok=True)
            (self.kotlin / name).write_text(text)
        self.git('add', '-A'); self.git('commit', '--quiet', '-m', 'c')
        return self.git('rev-parse', 'HEAD')

    def test_frozen_is_the_default_and_release_is_always_explicit(self):
        import inspect
        self.assertEqual(inspect.signature(build.build).parameters['kind'].default, 'frozen')
        calls = []
        for extra, wanted in (([], 'frozen'), (['--source', 'release'], 'release')):
            argv = ['run', 'build', '--host', 'h', '--vendor', 'iOS', '--output', 'o', '--approved', *extra]
            with patch.object(sys, 'argv', argv), patch.object(driver, 'private_config', return_value={}), \
                    patch.object(driver.builds, 'build', side_effect=lambda *a: calls.append(a)):
                driver.main()
            self.assertEqual(calls[-1][3], wanted)
        self.assertEqual(c.source_sha('frozen'), self.frozen)
        self.assertEqual(c.source_sha('release'), self.release)
        with self.assertRaisesRegex(c.e.Invalid, 'unknown'):
            c.source_sha('latest')

    def test_the_pinned_release_file_names_a_full_sha_other_than_the_baseline(self):
        text = (c.ROOT / 'docs/release-source.sha').read_text().strip()
        self.assertRegex(text, r'^[0-9a-f]{40}$')
        self.assertNotEqual(text, 'e47352f25cbc50333900d5d040a5544ff1224501')

    def test_a_checkout_without_the_selected_commit_is_rejected(self):
        self.assertEqual(c.verified_source('frozen', str(self.kotlin)), self.frozen)
        self.assertEqual(c.verified_source('release', str(self.kotlin)), self.release)
        other = self.root / 'other'; other.mkdir()
        subprocess.run(['git', '-C', str(other), 'init', '--quiet'], check=True)
        for kind in c.SOURCE_KINDS:
            with self.assertRaisesRegex(c.e.Invalid, 'does not hold'):
                c.verified_source(kind, str(other))

    def test_release_build_records_tree_hashes_that_match_the_frozen_samples(self):
        source, samples = c.select_build_source('iOS', str(self.kotlin), 'release')
        self.assertEqual(source, self.release)
        tree = self.git('rev-parse', self.frozen + ':sample/ios-app')
        self.assertEqual(samples, dict(origin='kotlin', paths={'sample/ios-app': tree}, frozen={'sample/ios-app': tree}, matches_frozen=True))
        self.assertEqual(c.select_build_source('Android', str(self.kotlin), 'frozen')[0], self.frozen)

    def test_a_release_that_changes_the_sample_cannot_be_built(self):
        self.commit({'sample/android-app/b.kt': 'changed'})
        newest = self.git('rev-parse', 'HEAD')
        with patch.object(c, 'release_sha', return_value=newest):
            with self.assertRaisesRegex(c.e.Invalid, 'changed the sample'):
                c.select_build_source('Android', str(self.kotlin), 'release')
            self.assertTrue(c.select_build_source('iOS', str(self.kotlin), 'release')[1]['matches_frozen'])

    def test_a_release_build_needs_the_checkout_but_a_frozen_macos_build_does_not(self):
        with self.assertRaisesRegex(c.e.Invalid, 'kotlin_source is required'):
            c.select_build_source('macOS', None, 'release')
        with self.assertRaisesRegex(c.e.Invalid, 'kotlin_source is required'):
            c.select_build_source('iOS', None, 'frozen')
        source, samples = c.select_build_source('macOS', None, 'frozen')
        self.assertEqual((source, samples['origin']), (self.frozen, 'repository'))
        self.assertEqual(samples['paths'], {'sample/macos-app': c.git_output('-C', str(c.ROOT), 'rev-parse', 'HEAD:sample/macos-app')})

    def release_build(self, **changes):
        return release_build(self.release, **changes)

    def test_build_record_source_must_be_known_consistent_and_verified(self):
        self.assertEqual(c.build_source(dict(source=self.frozen)), self.frozen)  # a 7.2 record has no other field
        self.assertEqual(c.build_source(dict(source=self.frozen, source_kind='frozen')), self.frozen)
        self.assertEqual(c.build_source(self.release_build()), self.release)
        for bad in (dict(source='f' * 40), dict(source_kind='frozen'), dict(samples=None),
                    dict(samples=dict(paths={'sample/ios-app': 'a' * 40}, matches_frozen=False))):
            with self.assertRaises(c.e.Invalid):
                c.build_source(self.release_build(**bad))
        with self.assertRaises(c.e.Invalid):
            c.build_source(dict(source=self.frozen, source_kind='release'))

    def test_an_explicit_source_is_an_assertion_about_the_build_record(self):
        frozen, release = dict(source=self.frozen), self.release_build()
        self.assertEqual(driver.expect_source(None, frozen), self.frozen)
        self.assertEqual(driver.expect_source('frozen', frozen), self.frozen)
        self.assertEqual(driver.expect_source('release', release), self.release)
        with self.assertRaisesRegex(c.e.Invalid, 'bound to the frozen source, not release'):
            driver.expect_source('release', frozen)
        with self.assertRaisesRegex(c.e.Invalid, 'bound to the release source, not frozen'):
            driver.expect_source('frozen', release)

    def test_stage_inputs_rejects_a_build_of_the_other_source(self):
        build_root = self.root / 'b'; (build_root / 'derived').mkdir(parents=True)
        c.save(build_root / 'build.json', self.release_build(products_path=str(build_root / 'derived'), products=c.inventory(build_root / 'derived')))
        args = SimpleNamespace(build=str(build_root), application='x', tests='y', output=str(self.root / 'out'), source='frozen')
        with self.assertRaisesRegex(c.e.Invalid, 'bound to the release source'):
            driver.stage_inputs(args)
        self.assertFalse((self.root / 'out').exists())

    def distribution(self, name, jar=b'jar'):
        home = self.root / name
        (home / 'bin').mkdir(parents=True); (home / 'lib').mkdir()
        (home / 'bin/marathon').write_text('#!/bin/sh\n'); (home / 'lib/core.jar').write_bytes(jar)
        return home

    def oracle_patch(self):
        """A reference patch that applies to every synthetic commit, and a ROOT that holds it at PATCH_FILE."""
        scratch = self.root / 'scratch-repo'
        if not scratch.exists():
            scratch.mkdir(); subprocess.run(['git', '-C', str(scratch), 'init', '--quiet'], check=True)
            (scratch / 'oracle').mkdir(); (scratch / 'oracle/new.txt').write_text('hook\n')
            subprocess.run(['git', '-C', str(scratch), 'add', 'oracle/new.txt'], check=True)
            patch_file = self.root / 'root' / c.PATCH_FILE
            patch_file.parent.mkdir(parents=True)
            patch_file.write_bytes(subprocess.run(['git', '-C', str(scratch), 'diff', '--cached'], check=True, capture_output=True).stdout)
        return self.root / 'root'

    def oracle_clone(self, sha, receipt=True, jar=b'jar'):
        """tools/oracle/build[-<sha>]/marathon-src as build.sh leaves it: HEAD at sha, installDist, and its receipt."""
        clone = self.root / 'oracle' / ('build' if sha == self.frozen else 'build-' + sha) / 'marathon-src'
        clone.parent.mkdir(parents=True)
        subprocess.run(['git', 'clone', '--quiet', str(self.kotlin), str(clone)], check=True)
        subprocess.run(['git', '-C', str(clone), 'checkout', '--quiet', '--detach', sha], check=True)
        install = clone / 'cli/build/install/marathon'
        (install / 'lib').mkdir(parents=True); (install / 'lib/core.jar').write_bytes(jar)
        if receipt:
            self.write_receipt(clone, sha)
        return clone

    def write_receipt(self, clone, sha):
        spec = importlib.util.spec_from_file_location('build_receipt', REPO / 'tools/oracle/build-receipt.py')
        module = importlib.util.module_from_spec(spec); spec.loader.exec_module(module)
        patch_file = self.oracle_patch() / c.PATCH_FILE
        value = module.receipt('reference', sha, clone, c.tree_with_patches(clone, sha, [patch_file]), [patch_file], 'java double')
        c.save(clone.parent / 'receipt-reference.json', value)

    def reference(self, kind, sha, clone=None, copy=None):
        with patch.object(c, 'ROOT', self.oracle_patch()), patch.object(driver, 'ROOT', self.oracle_patch()):
            clone = clone or self.oracle_clone(sha)
            self.count = getattr(self, 'count', 0) + 1
            copy = copy or self.distribution('dist-%s-%d' % (kind, self.count))
            out = self.root / ('ref-' + kind + '.json')
            driver.write_reference(SimpleNamespace(source=kind, oracle_clone=str(clone), jvm=str(copy / 'bin/marathon'), output=str(out)))
        return out, copy

    def test_the_jvm_reference_is_bound_to_the_builders_receipt_and_to_its_jars(self):
        out, copy = self.reference('release', self.release)
        record = c.e.load(out)
        self.assertEqual((record['source'], record['receipt']['source'], record['receipt']['gradle_exit']), (self.release, self.release, 0))
        self.assertEqual(record['receipt']['jars'], record['jars'])
        host = dict(jvm=str(copy / 'bin/marathon'), jvm_reference=str(out))
        with patch.object(c, 'ROOT', self.oracle_patch()):
            self.assertEqual(c.jvm_reference_source(host), self.release)
            (copy / 'lib/core.jar').write_bytes(b'rebuilt from another commit')
            with self.assertRaisesRegex(c.e.Invalid, 'differs from its reference'):
                c.jvm_reference_source(host)
            (copy / 'lib/core.jar').write_bytes(b'jar')
            (copy / 'lib/extra.jar').write_bytes(b'x')
            with self.assertRaisesRegex(c.e.Invalid, 'differs from its reference'):
                c.jvm_reference_source(host)
            (copy / 'lib/extra.jar').unlink()
            forged = dict(record, receipt=dict(record['receipt'], gradle_exit=1)); c.save(self.root / 'forged.json', forged)
            with self.assertRaisesRegex(c.e.Invalid, 'successful build'):
                c.jvm_reference_source(dict(host, jvm_reference=str(self.root / 'forged.json')))

    def test_a_release_checkout_holding_frozen_jars_is_not_a_reference(self):
        # R1-sol-1: a release checkout with a copied frozen installDist used to be accepted.
        with self.assertRaisesRegex(c.e.Invalid, 'no builder receipt'):
            self.reference('release', self.release, clone=self.oracle_clone(self.release, receipt=False, jar=b'frozen jars'),
                           copy=self.distribution('frozen-dist', b'frozen jars'))

    def test_jars_swapped_in_after_the_build_no_longer_match_its_receipt(self):
        clone = self.oracle_clone(self.release)  # built, receipt of b'jar'
        (clone / 'cli/build/install/marathon/lib/core.jar').write_bytes(b'frozen jars')
        with self.assertRaisesRegex(c.e.Invalid, 'installed jars differ from the builder receipt'):
            self.reference('release', self.release, clone=clone, copy=self.distribution('stale', b'frozen jars'))
        clone2 = self.oracle_clone(self.frozen)
        with self.assertRaisesRegex(c.e.Invalid, 'distribution differs from the builder receipt'):
            self.reference('frozen', self.frozen, clone=clone2, copy=self.distribution('other', b'another build'))

    def test_a_receipt_of_another_source_or_tree_is_rejected(self):
        clone = self.oracle_clone(self.release, receipt=False)
        frozen_clone = self.oracle_clone(self.frozen)
        target = clone.parent / 'receipt-reference.json'
        target.write_bytes((frozen_clone.parent / 'receipt-reference.json').read_bytes())  # the frozen build's receipt
        with self.assertRaisesRegex(c.e.Invalid, 'receipt is for another source'):
            self.reference('release', self.release, clone=clone)
        receipt = c.e.load(target)
        receipt.update(source=self.release, head=self.release)  # relabelled, but it still names the frozen tree
        target.write_bytes(c.e.encoded(receipt))
        with self.assertRaisesRegex(c.e.Invalid, 'another source tree'):
            self.reference('release', self.release, clone=clone)

    def test_a_reference_cannot_be_written_for_the_wrong_clone_or_distribution(self):
        clone = self.oracle_clone(self.release)
        # the release clone at the frozen kind: wrong directory name and wrong HEAD
        with self.assertRaisesRegex(c.e.Invalid, 'not the one build.sh keeps'):
            self.reference('frozen', self.frozen, clone=clone)
        subprocess.run(['git', '-C', str(clone), 'checkout', '--quiet', '--detach', self.frozen], check=True)
        with self.assertRaisesRegex(c.e.Invalid, 'not at the selected source'):
            self.reference('release', self.release, clone=clone)

    def test_build_script_deletes_the_old_receipt_before_gradle_and_writes_the_new_one_after_the_smoke_test(self):
        text = (c.ROOT / 'tools/oracle/build.sh').read_text()
        delete, gradle = text.index('rm -f "$BUILD/receipt-$VARIANT.json"'), text.index('./gradlew')
        smoke, write = text.index('marathon" version'), text.rindex('build-receipt.py')
        self.assertLess(delete, gradle)
        self.assertLess(smoke, write)
        for fallible in ('"$JAVA_HOME/bin/java"', 'git clone', 'git -C "$SRC" checkout', 'git -C "$SRC" apply', 'verifying the clone'):
            self.assertLess(delete, text.index(fallible), fallible)  # R2-sol-5: before any fallible preparation
        self.assertIn('--verified-tree "$expected_tree"', text)

    def test_a_capture_always_needs_a_reference_and_never_accepts_the_other_one(self):
        rel, rel_copy = self.reference('release', self.release)
        frz, frz_copy = self.reference('frozen', self.frozen)
        host = lambda out, copy: dict(jvm=str(copy / 'bin/marathon'), jvm_reference=str(out))
        with patch.object(c, 'ROOT', self.oracle_patch()):
            self.assertEqual(c.require_reference_source(host(rel, rel_copy), self.release), self.release)
            self.assertEqual(c.require_reference_source(host(frz, frz_copy), self.frozen), self.frozen)
            for source in (self.frozen, self.release):  # R1-sol-2: the frozen source is no exception any more
                with self.assertRaisesRegex(c.e.Invalid, 'needs a jvm_reference'):
                    c.require_reference_source(dict(jvm=str(rel_copy / 'bin/marathon')), source)
            with self.assertRaisesRegex(c.e.Invalid, 'not bound to the selected source'):
                c.require_reference_source(host(frz, frz_copy), self.release)
            with self.assertRaisesRegex(c.e.Invalid, 'not bound to the selected source'):
                c.require_reference_source(host(rel, rel_copy), self.frozen)


class SourceCapture(unittest.TestCase):
    """The same, through whole fake captures; the evidence layer knows only the two real source SHAs."""

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.frozen, self.release = c.FROZEN, c.release_sha()

    def world(self, kind, jvm_reference):
        """Fake inputs and a fake CLI as in the whole-capture test, bound to `kind`."""
        self.worlds = getattr(self, 'worlds', 0) + 1
        root = self.root / ('world-%s-%d' % (kind, self.worlds)); inputs = root / 'inputs'; (inputs / 'assets').mkdir(parents=True)
        (inputs / 'assets/app').write_bytes(b'app'); (inputs / 'assets/tests').write_bytes(b'tests')
        c.save(inputs / 'products.json', {'application': 'assets/app', 'tests': 'assets/tests'})
        build_record = dict(products=['fake'], commands=['fake'], profile='local-27', worker_id='worker-role')
        build_record.update(release_build(self.release, products=['fake'], commands=['fake']) if kind == 'release' else dict(source=self.frozen))
        c.save(inputs / 'build.json', build_record)
        (inputs / 'discovery.yml').write_bytes(b'JVM-discovery-double')
        scenario = c.SCENARIOS['ios-unit-nm']
        c.save(inputs / 'selection.json', {'tests': ['pkg.' + x for x in scenario['methods']], 'review': 'offline-double',
               'excluded_methods': {}, 'discovery_sha256': c.e.sha((inputs / 'discovery.yml').read_bytes())})
        binary = root / 'install/bin/marathon'; binary.parent.mkdir(parents=True)
        binary.write_text('#!' + sys.executable + "\nfrom pathlib import Path\nimport sys\np=Path('report/test_result');p.mkdir(parents=True)\n(p/'raw.json').write_text('{}')\nsys.exit(1)\n")
        binary.chmod(0o755)
        (root / 'install/lib').mkdir(); (root / 'install/lib/core.jar').write_bytes(b'jar')
        host = dict(profile='local-27', worker_id='worker-role', jvm=str(binary), native=str(binary), java_home='/fake-jdk', watchdog_seconds=600)
        if jvm_reference:
            host['jvm_reference'] = str(jvm_reference)
        return root, inputs, host, scenario

    def run_capture(self, root, inputs, host, scenario, side, source=None):
        class DoubleWorker:
            def __init__(self, *_): pass
            def __enter__(self): return self
            def __exit__(self, *_): pass
            def call(self, op, **kwargs):
                return {'prepare': {'udid': kwargs.get('run_id'), 'before': {'xcode': 'fake'}}, 'cleanup': {'complete': True}}.get(op, {})
        args = SimpleNamespace(scenario=scenario['id'], side=side, discover=False, output=root / 'pair', inputs=inputs, source=source)
        with patch.object(driver, 'Worker', DoubleWorker), patch.object(driver, 'java_version', return_value='JBR 21.0.10 double'):
            driver.capture(args, host)
        return root / 'pair'

    def reference_for(self, root, sha):
        """A reference record for the world's own fake distribution."""
        return reference_record(root / ('reference-' + sha[:8] + '.json'), sha, {'core.jar': c.e.sha(b'jar')})

    def test_a_release_capture_binds_one_source_into_capture_contract_and_evidence(self):
        root, inputs, host, scenario = self.world('release', None)
        host['jvm_reference'] = str(self.reference_for(root, self.release))
        pair = self.run_capture(root, inputs, host, scenario, 'jvm', source='release')
        self.run_capture(root, inputs, host, scenario, 'native', source='release')
        for side in ('jvm', 'native'):
            capture = c.e.load(pair / side / 'capture.json')
            self.assertEqual((capture['source'], capture['source_kind'], capture['jvm_reference_source']), (self.release, 'release', self.release))
            record = c.e.load(pair / side / 'evidence/evidence.json')
            self.assertEqual((record['source'], record['provenance']['source_sha']), (self.release, self.release))
        self.assertEqual(c.e.load(pair / 'contract.json')['source'], self.release)
        c.e.compare(pair / 'jvm/evidence', pair / 'native/evidence', c.e.load(pair / 'contract.json'))

    def test_a_frozen_capture_keeps_the_seven_two_contract_and_envelope(self):
        root, inputs, host, scenario = self.world('frozen', None)
        host['jvm_reference'] = str(self.reference_for(root, self.frozen))
        pair = self.run_capture(root, inputs, host, scenario, 'jvm')
        self.assertNotIn('source', c.e.load(pair / 'contract.json'))
        record = c.e.load(pair / 'jvm/evidence/evidence.json')
        self.assertEqual(record['source'], self.frozen)
        self.assertNotIn('source_sha', record['provenance'])
        capture = c.e.load(pair / 'jvm/capture.json')
        self.assertEqual((capture['source'], capture['jvm_reference_source']), (self.frozen, self.frozen))

    def test_a_capture_mixing_sources_or_lacking_a_reference_is_rejected_before_anything_runs(self):
        # release inputs, JVM distribution without any reference record (and, R1-sol-2, so for frozen inputs)
        for kind in ('release', 'frozen'):
            root, inputs, host, scenario = self.world(kind, None)
            with self.assertRaisesRegex(c.e.Invalid, 'needs a jvm_reference'):
                self.run_capture(root, inputs, host, scenario, 'jvm')
        # release inputs, JVM distribution recorded for the frozen source
        root, inputs, host, scenario = self.world('release', None)
        host['jvm_reference'] = str(self.reference_for(root, self.frozen))
        with self.assertRaisesRegex(c.e.Invalid, 'not bound to the selected source'):
            self.run_capture(root, inputs, host, scenario, 'jvm')
        # frozen inputs, JVM distribution recorded for the release
        root, inputs, host, scenario = self.world('frozen', None)
        host['jvm_reference'] = str(self.reference_for(root, self.release))
        with self.assertRaisesRegex(c.e.Invalid, 'not bound to the selected source'):
            self.run_capture(root, inputs, host, scenario, 'jvm')
        # explicit assertion against the inputs
        host['jvm_reference'] = str(self.reference_for(root, self.frozen))
        with self.assertRaisesRegex(c.e.Invalid, 'bound to the frozen source, not release'):
            self.run_capture(root, inputs, host, scenario, 'jvm', source='release')
        self.assertFalse((root / 'pair').exists() and any((root / 'pair').iterdir()))

    def test_the_native_side_cannot_follow_a_jvm_capture_of_another_source(self):
        root, inputs, host, scenario = self.world('frozen', None)
        host['jvm_reference'] = str(self.reference_for(root, self.frozen))
        pair = self.run_capture(root, inputs, host, scenario, 'jvm')
        capture = c.e.load(pair / 'jvm/capture.json'); capture['source'] = self.release
        c.save(pair / 'jvm/capture.json', capture)
        with self.assertRaisesRegex(c.e.Invalid, 'mixes Kotlin sources'):
            self.run_capture(root, inputs, host, scenario, 'native')


SWEEP = c.HERE / 'sweep'
sweep_spec = importlib.util.spec_from_file_location('sweep_plan', SWEEP / 'plan.py')
SweepPlan = importlib.util.module_from_spec(sweep_spec); sweep_spec.loader.exec_module(SweepPlan)


class SweepScripts(unittest.TestCase):
    """The committed sweep scripts, with every host-touching command replaced by a recording stub."""

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.log = self.root / 'stub.log'
        self.tool = self.root / 'run-stub.py'
        self.tool.write_text(
            "import os, sys\nfrom pathlib import Path\nargv = sys.argv[1:]\n"
            "open(os.environ['STUB_LOG'], 'a').write(' '.join(argv) + '\\n')\n"
            "def opt(name): return argv[argv.index(name) + 1]\n"
            "if argv[0] == 'capture-pair':\n"
            "    out = Path(opt('--output'))\n"
            "    if '--discover' in argv:\n"
            "        (out / 'discovery/in/report').mkdir(parents=True); (out / 'discovery/in/report/discovery.yml').write_text('stub discovery')\n"
            "    sys.exit(int(os.environ.get('STUB_CAPTURE_RC', '0')))\n"
            "if argv[0] == 'stage-inputs': Path(opt('--output')).mkdir(parents=True)\n"
            "if argv[0] == 'preflight':\n"
            "    out = Path(opt('--output')); out.mkdir(parents=True)   # like the real tool, an existing directory is an error\n"
            "    (out / 'preflight.json').write_text('{}')\n"
            "if argv[0] == 'build':\n"
            "    out = Path(opt('--output')); out.mkdir(parents=True)\n"
            "    (out / 'build.json').write_text(os.environ['STUB_BUILD'])\n"
            "if argv[0] == 'compare': Path(opt('--mismatch')).mkdir(parents=True)\n")
        self.load = self.root / 'load.sh'
        self.set_load(1)
        self.ps = self.root / 'ps-stub.sh'
        self.ps.write_text('#!/bin/sh\necho "1 /sbin/init"\n'); self.ps.chmod(0o755)
        self.host = self.root / 'host.json'
        c.save(self.host, dict(profile='local-27'))
        self.runs = self.root / 'runs'; self.runs.mkdir()
        self.env = dict(os.environ, MRS_RUN_TOOL=str(self.tool), MRS_PY=sys.executable, STUB_LOG=str(self.log),
                        MRS_LOAD_CMD=str(self.load), MRS_PS=str(self.ps), MRS_PGREP='false', MRS_PKILL='true', MRS_GUARD_TRIES='2',
                        MRS_GUARD_PAUSE='0', MRS_LEFTOVER_PATHS=str(self.root / 'absent'), MRS_ALLOW_DIRTY='1',
                        MRS_HOST=str(self.host), MRS_RUNS=str(self.runs), MRS_OLD=str(self.root / 'old'),
                        STUB_BUILD=json.dumps(self.build_record()))

    NATIVE = 'b' * 40

    def build_record(self, **changes):
        sha = c.release_sha()
        record = dict(source=sha, source_kind='release', native_source=self.NATIVE,
                      samples=dict(matches_frozen=True, paths={'sample/ios-app': 't' * 40}))
        record.update(changes)
        return record

    def set_load(self, value):
        self.load.write_text('#!/bin/sh\necho %s\n' % value); self.load.chmod(0o755)

    def sweep(self, *argv, **env):
        return subprocess.run(['bash', str(SWEEP / 'sweep.sh'), *argv], env=dict(self.env, **env), capture_output=True, text=True)

    def calls(self):
        return self.log.read_text().splitlines() if self.log.exists() else []

    def test_a_requalified_release_verdict_is_written_next_to_the_retained_one(self):
        # 7.5 closure: decision 7 changed the projection, so the pairs are compared again under a new policy
        pair = self.root / 'pair'; pair.mkdir()
        (pair / 'release.json').write_text('retained')
        def release(*extra):
            argv = ['run', 'compare', str(pair), '--release', '--policy', 'p.json', '--mismatch', str(self.root / 'm'), *extra]
            with patch.object(sys, 'argv', argv), patch.object(driver.outcomes, 'release', return_value=dict(status='passed')) as called:
                driver.main()
            return called
        with self.assertRaisesRegex(c.e.Invalid, 'retain it'):
            release()
        self.assertEqual((pair / 'release.json').read_text(), 'retained')
        for bad in ('../release.json', 'verdict.json', 'release-d7.txt', 'release-a/b.json'):
            with self.assertRaisesRegex(c.e.Invalid, 'named release'):
                release('--verdict', bad)
        release('--verdict', 'release-d7.json')
        self.assertEqual(c.e.load(pair / 'release-d7.json')['status'], 'passed')
        self.assertEqual((pair / 'release.json').read_text(), 'retained')
        with self.assertRaisesRegex(c.e.Invalid, 'retain it'):
            release('--verdict', 'release-d7.json')

    def test_every_script_parses_and_uses_strict_mode(self):
        for path in sorted(SWEEP.glob('*.sh')):
            self.assertEqual(subprocess.run(['bash', '-n', str(path)], capture_output=True).returncode, 0, path.name)
            if path.name != 'common.sh':
                self.assertIn('set -euo pipefail', path.read_text(), path.name)

    def test_a_failed_load_guard_stops_before_the_capture(self):
        # R1-sol-5: the guard used to fail open
        self.set_load(25)
        result = self.sweep('prepare', 'ios-unit-nm')
        self.assertEqual(result.returncode, 3, result.stderr)
        self.assertIn('not capturing', result.stderr)
        self.assertEqual([x.split()[0] for x in self.calls()], ['stage-inputs'])  # staging is not a capture; nothing after it ran

    def test_a_stray_yes_blocks_the_capture_even_at_low_load(self):
        result = self.sweep('native', 'macos-cancel', '1', MRS_PGREP='true')
        self.assertEqual(result.returncode, 3, result.stderr)
        self.assertEqual(self.calls(), [])

    def test_a_failed_capture_status_is_propagated(self):
        result = self.sweep('native', 'macos-cancel', '1', STUB_CAPTURE_RC='42')
        self.assertEqual(result.returncode, 42, result.stderr)

    def test_a_leftover_after_a_successful_capture_fails_the_step(self):
        leftover = self.root / 'leftover'; leftover.write_text('x')
        result = self.sweep('native', 'macos-cancel', '1', MRS_LEFTOVER_PATHS=str(leftover))
        self.assertEqual(result.returncode, 4, result.stderr)
        self.assertIn('leftover', result.stderr)
        # a failing capture keeps its own status even when something is left behind
        result = self.sweep('native', 'macos-cancel', '2', MRS_LEFTOVER_PATHS=str(leftover), STUB_CAPTURE_RC='42')
        self.assertEqual(result.returncode, 42)

    def test_prepare_binds_everything_to_the_release_and_the_scenario_table(self):
        result = self.sweep('prepare', 'ios-ui-video')
        self.assertEqual(result.returncode, 0, result.stderr)
        stage, capture = self.calls()
        self.assertIn('stage-inputs --source release --build %s/ios-build --application Debug-iphonesimulator/sample-app.app'
                      ' --tests Debug-iphonesimulator/sample-appUITests-Runner.app' % self.runs, stage)
        self.assertIn('capture-pair --host %s --scenario ios-ui-video --side jvm --discover --source release' % self.host, capture)
        self.assertTrue(capture.endswith('--approved'))
        self.assertEqual((self.runs / 'ios-ui-video-inputs/discovery.yml').read_text(), 'stub discovery')

    def test_jvm_step_makes_a_draft_and_two_jvm_pairs_and_qualify_is_its_own_step(self):
        old = self.root / 'old'
        draft = old / SweepPlan.get('macos-cancel', 'old_draft'); draft.parent.mkdir(parents=True)
        c.save(draft, dict(format='f', review='old', rules=[], jvm_runs=[1], projection_sha256='x'))
        result = self.sweep('jvm', 'macos-cancel', 'review text')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual([x.split()[0] for x in self.calls()], ['capture-pair', 'capture-pair'])
        self.assertEqual(self.sweep('qualify', 'macos-cancel').returncode, 0)
        self.assertEqual(self.calls()[-1].split()[0], 'qualify')
        self.assertEqual(c.e.load(self.runs / 'macos-cancel-draft.json')['review'], 'review text')
        self.assertNotIn('jvm_runs', c.e.load(self.runs / 'macos-cancel-draft.json'))

    def test_jvmpair_captures_one_more_jvm_side_through_the_same_guard(self):
        self.set_load(25)
        self.assertEqual(self.sweep('jvmpair', 'macos-cancel', '3').returncode, 3)
        self.set_load(1)
        self.assertEqual(self.sweep('jvmpair', 'macos-cancel', '3').returncode, 0)
        self.assertIn('--side jvm --source release', self.calls()[0])

    def test_compare_never_reuses_a_mismatch_directory(self):
        self.assertEqual(self.sweep('compare', 'macos-cancel', '1').returncode, 0)
        self.assertEqual(self.sweep('compare', 'macos-cancel', '1').returncode, 0)
        first, second = self.calls()
        self.assertTrue(first.endswith('macos-cancel-native-mismatch-1-1') and second.endswith('macos-cancel-native-mismatch-1-2'))

    def test_the_sweep_refuses_a_dirty_worktree_and_relative_paths(self):
        env = dict(self.env); env.pop('MRS_ALLOW_DIRTY')
        dirty = c.git_output('-C', str(c.ROOT), 'status', '--porcelain')
        if dirty:  # only meaningful while the tree is dirty; a clean tree is the sweep's normal state
            result = subprocess.run(['bash', str(SWEEP / 'sweep.sh'), 'gates'], env=env, capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
        result = self.sweep('audit', MRS_RUNS='relative/runs')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('absolute', result.stderr)

    def test_the_scenario_table_matches_the_ledger(self):
        table = SweepPlan.plan()['scenarios']
        runnable = {s for s, v in c.SCENARIOS.items() if s != 'macos-ui-xctest'}
        self.assertEqual(set(table), runnable)  # macos-ui-xctest stays blocked (U103)
        for name, row in table.items():
            self.assertIn(row['profile'], c.SCENARIOS[name]['profiles'], name)
            self.assertTrue(row['old_selection'].endswith('selection.json') and row['old_draft'].endswith('-outcomes-r3.json'), name)
        self.assertEqual(SweepPlan.scenarios('linux-kvm'), ['android-local-video-sync', 'android-remote-screenshots'])

    def test_selection_is_rebound_to_the_new_discovery_and_must_list_every_test(self):
        inputs = self.root / 'inputs'; inputs.mkdir()
        old = self.root / 'selection-old.json'
        c.save(old, dict(discovery_sha256='old', tests=['pkg.Cls#testOne', 'Other#testTwo'], excluded_methods={}, review='7.2'))
        (inputs / 'discovery.yml').write_text('- pkg: pkg\n  class: Cls\n  method: testOne\n- class: Other\n  method: testTwo\n')
        SweepPlan.selection(old, inputs, 'compared with the 7.2 discovery: identical')
        value = c.e.load(inputs / 'selection.json')
        self.assertEqual(value['discovery_sha256'], c.e.sha((inputs / 'discovery.yml').read_bytes()))
        self.assertEqual(value['tests'], ['pkg.Cls#testOne', 'Other#testTwo'])
        (inputs / 'selection.json').unlink()
        (inputs / 'discovery.yml').write_text('- class: Cls\n  method: testOne\n')
        with self.assertRaisesRegex(SweepPlan.Stop, 'does not list'):
            SweepPlan.selection(old, inputs, 'review')
        with self.assertRaisesRegex(SweepPlan.Stop, 'review text'):
            SweepPlan.selection(old, inputs, '  ')

    def test_draft_drops_the_stale_qualification_and_keeps_the_rules(self):
        old = self.root / 'r3.json'
        c.save(old, dict(format='f', review='old', deviations=[], jvm_runs=[{'pair': '/x'}], projection_sha256='h',
                         rules=[dict(id='ui-video', evidence=dict(jvm_a='a'), expected={})]))
        SweepPlan.draft(old, self.root / 'draft.json', 'sweep draft')
        value = c.e.load(self.root / 'draft.json')
        self.assertEqual((value['review'], 'jvm_runs' in value, 'projection_sha256' in value), ('sweep draft', False, False))
        self.assertEqual(value['rules'], [dict(id='ui-video', expected={})])
        with self.assertRaisesRegex(SweepPlan.Stop, 'retained'):
            SweepPlan.draft(old, self.root / 'draft.json', 'again')

    def test_worker_host_config_uses_absolute_paths_under_the_worker_directory_and_mode_0600(self):
        # R1-sol-6: WK was relative on the Mac and never set on the worker
        base = self.root / 'base.json'
        c.save(base, dict(profile='linux-kvm', adb_port=45037, jvm='/old/jvm', native='relative/native', image='img'))
        wk = self.root / 'mrs-real-e2e-7.5'
        SweepPlan.host_config(base, wk, self.root / 'host-out.json')
        value = c.e.load(self.root / 'host-out.json')
        self.assertEqual(value['jvm'], str(wk / 'jvm/marathon/bin/marathon'))
        self.assertEqual(value['jvm_reference'], str(wk / 'jvm/reference.json'))
        for key in ('kotlin_source', 'jvm', 'jvm_reference', 'native', 'worker_checkout'):
            self.assertTrue(Path(value[key]).is_absolute(), key)
        self.assertEqual((value['adb_port'], value['image']), (45037, 'img'))
        self.assertEqual((self.root / 'host-out.json').stat().st_mode & 0o777, 0o600)
        with self.assertRaisesRegex(SweepPlan.Stop, 'absolute'):
            SweepPlan.host_config(base, Path('mrs-real-e2e-7.5'), self.root / 'other.json')

    def audit_pair(self, name, side, source, build_source, native=NATIVE, **extra):
        d = self.runs / name / side; (d / 'in').mkdir(parents=True)
        capture = dict(source=source, source_kind=c.source_kind(source), jvm_reference_source=source, native_source=native)
        capture.update(extra)
        c.save(d / 'capture.json', capture)
        c.save(d / 'in/build.json', self.build_record(source=build_source, source_kind=c.source_kind(build_source), native_source=native))

    def audit_build(self, name='ios-build', native=NATIVE):
        (self.runs / name).mkdir(exist_ok=True)
        c.save(self.runs / name / 'build.json', self.build_record(native_source=native))

    def test_build_and_capture_audit_reject_anything_not_bound_to_the_release(self):
        sha = c.release_sha()
        self.audit_build()
        self.audit_pair('s-pair-1', 'jvm', sha, sha); self.audit_pair('s-pair-1', 'native', sha, sha)
        SweepPlan.audit(self.runs, sha)
        self.audit_pair('s-pair-2', 'jvm', sha, c.FROZEN)  # release labels over a frozen build record
        with self.assertRaisesRegex(SweepPlan.Stop, '1 capture'):
            SweepPlan.audit(self.runs, sha)
        with self.assertRaisesRegex(SweepPlan.Stop, 'no build record'):
            SweepPlan.audit(self.root / 'empty', sha)
        frozen_build = self.root / 'build.json'
        c.save(frozen_build, dict(source=c.FROZEN, source_kind='frozen'))
        with self.assertRaisesRegex(SweepPlan.Stop, 'not bound'):
            SweepPlan.check_build(frozen_build, sha)

    def test_the_audit_compares_with_the_native_commit_the_build_records_name_not_with_head(self):
        # 7.5 sweep: HEAD moves with the offline commits that follow the captures
        sha = c.release_sha()
        self.audit_build('ios-build'); self.audit_build('macos-build')
        self.audit_pair('s-pair-1', 'jvm', sha, sha); self.audit_pair('s-pair-1', 'native', sha, sha)
        c.save(self.runs / 's-pair-1/release.json', {})
        result = self.sweep('audit')   # the repository's HEAD is not NATIVE
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('native commit (from the build records): ' + self.NATIVE, result.stdout)
        self.assertIn('released s-pair-1/release.json', result.stdout)
        # an explicit expectation must still agree with the records
        with self.assertRaisesRegex(SweepPlan.Stop, 'build records name native commit'):
            SweepPlan.audit(self.runs, sha, c.git_output('-C', str(c.ROOT), 'rev-parse', 'HEAD').strip())

    def test_the_audit_rejects_a_capture_of_another_native_commit_and_disagreeing_build_records(self):
        sha = c.release_sha()
        self.audit_build()
        self.audit_pair('s-pair-1', 'jvm', sha, sha)
        self.audit_pair('s-pair-1', 'native', sha, sha, native='c' * 40)   # captured by another native commit
        with self.assertRaisesRegex(SweepPlan.Stop, '1 capture'):
            SweepPlan.audit(self.runs, sha)
        self.assertNotEqual(self.sweep('audit').returncode, 0)
        # a jvm capture is held to the same commit
        shutil.rmtree(self.runs / 's-pair-1'); self.audit_pair('s-pair-1', 'native', sha, sha)
        self.audit_pair('s-pair-1', 'jvm', sha, sha, native='c' * 40)
        with self.assertRaisesRegex(SweepPlan.Stop, '1 capture'):
            SweepPlan.audit(self.runs, sha)
        # the retained build record inside a capture must agree as well
        shutil.rmtree(self.runs / 's-pair-1'); self.audit_pair('s-pair-1', 'native', sha, sha); self.audit_pair('s-pair-1', 'jvm', sha, sha)
        c.save(self.runs / 's-pair-1/jvm/in/build.json', self.build_record(native_source='c' * 40))
        with self.assertRaisesRegex(SweepPlan.Stop, '1 capture'):
            SweepPlan.audit(self.runs, sha)
        c.save(self.runs / 's-pair-1/jvm/in/build.json', self.build_record())
        SweepPlan.audit(self.runs, sha)
        self.audit_build('macos-build', native='c' * 40)
        with self.assertRaisesRegex(SweepPlan.Stop, 'different native commits'):
            SweepPlan.audit(self.runs, sha)
        for value in (None, 'short', 42):
            c.save(self.runs / 'macos-build/build.json', self.build_record(native_source=value))
            with self.assertRaisesRegex(SweepPlan.Stop, 'no native commit'):
                SweepPlan.audit(self.runs, sha)

    def stub(self, name, body):
        path = self.root / name
        path.write_text('#!/bin/sh\n' + body + '\n'); path.chmod(0o755)
        return path

    def test_a_failing_process_query_refuses_the_capture(self):
        # R2-sol-3: pgrep exit 2 / a missing pgrep / an unreadable load used to let the capture run
        for label, env in (('pgrep status 2', dict(MRS_PGREP=str(self.stub('pgrep2', 'exit 2')))),
                           ('missing pgrep', dict(MRS_PGREP=str(self.root / 'no-such-pgrep'))),
                           ('load query fails', dict(MRS_LOAD_CMD=str(self.stub('badload', 'exit 1')))),
                           ('load not a number', dict(MRS_LOAD_CMD=str(self.stub('oddload', 'echo n/a'))))):
            result = self.sweep('native', 'macos-cancel', '1', **env)
            self.assertEqual(result.returncode, 5, label + result.stderr)
            self.assertEqual(self.calls(), [], label)

    def test_pgrep_no_match_is_the_only_status_that_clears_the_guard(self):
        self.assertEqual(self.sweep('native', 'macos-cancel', '1', MRS_PGREP=str(self.stub('pgrep1', 'exit 1'))).returncode, 0)
        self.assertEqual(len(self.calls()), 1)

    def test_a_failing_process_inventory_fails_the_leftover_check_on_linux(self):
        def leftovers(**env):
            return subprocess.run(['bash', '-c', '. "%s/common.sh"; sweep_leftovers' % SWEEP], capture_output=True, text=True,
                                  env=dict(self.env, MRS_UNAME='Linux', **env))
        self.assertEqual(leftovers().returncode, 0)
        failed = leftovers(MRS_PS=str(self.stub('ps2', 'exit 2')))
        self.assertEqual(failed.returncode, 5)
        self.assertIn('process inventory', failed.stderr)
        self.assertEqual(leftovers(MRS_PS=str(self.stub('psq', 'echo "77 qemu-system-x86_64 -m 2048"'))).returncode, 1)
        self.assertEqual(leftovers(MRS_PS=str(self.stub('psadb', 'echo "78 adb -L tcp:45037 fork-server"'))).returncode, 1)
        result = self.sweep('native', 'macos-cancel', '1', MRS_UNAME='Linux', MRS_PS=str(self.stub('ps3', 'exit 3')))
        self.assertNotEqual(result.returncode, 0)  # a successful capture with an unknown inventory is not a success

    def preflights_and_builds(self):
        return [x.split()[0] + ':' + x.split('--output')[1].split()[0].rsplit('/', 1)[1] for x in self.calls()
                if x.split()[0] in ('preflight', 'build')]

    def test_build_is_resumable_per_platform(self):
        # 7.5 sweep: the macOS part could not be run after the iOS one, `rt build` refused the existing iOS output
        self.assertEqual(self.sweep('build', 'ios-build').returncode, 0)
        self.assertEqual(self.preflights_and_builds(), ['preflight:preflight-ios-build', 'build:ios-build'])
        self.log.write_text('')
        result = self.sweep('build')    # iOS is done, macOS is not
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.preflights_and_builds(), ['preflight:preflight-macos-build', 'build:macos-build'])
        self.assertIn('ios-build is already built', result.stdout)
        self.log.write_text('')
        result = self.sweep('build')    # everything is done: only the records are checked
        self.assertEqual((result.returncode, self.preflights_and_builds()), (0, []))

    def test_build_with_a_directory_limits_the_step_and_rejects_an_unknown_one(self):
        self.assertEqual(self.sweep('build', 'macos-build').returncode, 0)
        self.assertEqual(self.preflights_and_builds(), ['preflight:preflight-macos-build', 'build:macos-build'])
        self.assertFalse((self.runs / 'ios-build').exists())
        result = self.sweep('build', 'android-build')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('no build directory android-build', result.stderr)

    def test_build_reuses_a_passed_preflight_and_stops_on_a_blocked_one_or_an_interrupted_build(self):
        (self.runs / 'preflight-ios-build').mkdir()
        (self.runs / 'preflight-ios-build/preflight.json').write_text('{}')
        self.assertEqual(self.sweep('build', 'ios-build').returncode, 0)
        self.assertEqual(self.preflights_and_builds(), ['build:ios-build'])
        blocked = self.runs / 'preflight-macos-build'; blocked.mkdir()   # e.g. the TCC refusal: no preflight.json
        (blocked / 'worker.stderr').write_text('authorization denied')
        result = self.sweep('build', 'macos-build')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('without preflight.json', result.stderr)
        self.assertEqual(self.preflights_and_builds(), ['build:ios-build'])   # nothing new, and the evidence is not touched
        self.assertEqual((blocked / 'worker.stderr').read_text(), 'authorization denied')
        (self.runs / 'macos-build').mkdir()                                    # a build that was interrupted
        (blocked / 'preflight.json').write_text('{}')
        result = self.sweep('build', 'macos-build')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('without a build record', result.stderr)

    def test_build_still_checks_the_record_of_a_finished_directory(self):
        self.assertEqual(self.sweep('build', 'ios-build').returncode, 0)
        c.save(self.runs / 'ios-build/build.json', self.build_record(source=c.FROZEN, source_kind='frozen'))
        result = self.sweep('build', 'ios-build')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('not bound to the release source', result.stderr)

    def test_the_stage_archive_is_built_without_appledouble_entries_and_refused_with_one(self):
        # 7.5 sweep: macOS tar added `._*` members that the worker hashed as jars
        self.assertNotEqual(subprocess.run(['bash', '-c', '. "%s/common.sh"; sweep_reject_appledouble "$1" x.tar' % SWEEP, '_',
                                            'marathon/lib/a.jar\nmarathon/lib/._a.jar'], capture_output=True, text=True).returncode, 0)
        for listing, rejected in (('marathon/lib/a.jar\nmarathon/bin/marathon', False), ('marathon/lib/dot._x.jar', False),
                                  ('marathon/._x/y', True), ('._marathon', True)):
            result = subprocess.run(['bash', '-c', '. "%s/common.sh"; sweep_reject_appledouble "$1" x.tar' % SWEEP, '_', listing],
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode != 0, rejected, listing + result.stderr)
        world = self.stage_world()
        result = self.stage(world)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('COPYFILE_DISABLE=1', (world / 'tar.log').read_text())
        self.assertEqual(self.entries(world / 'stage/jvm.tar'), ['marathon/', 'marathon/bin/', 'marathon/bin/marathon', 'marathon/lib/', 'marathon/lib/a.jar'])
        # a stub tar that ignores COPYFILE_DISABLE, as a macOS tar with extended attributes would
        shutil.rmtree(world / 'stage')
        result = self.stage(world, STUB_APPLEDOUBLE='1')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("AppleDouble entry 'marathon/lib/._a.jar'", result.stderr)
        self.assertFalse((world / 'stage/frozen.pack').exists() and (world / 'stage/reference.json').exists())

    def entries(self, tar):
        return sorted(subprocess.run(['tar', '-tf', str(tar)], capture_output=True, text=True).stdout.split())

    def stage_world(self):
        """A copy of the sweep scripts under a scratch repository root, with stub git and a recording tar."""
        world = self.root / 'world'
        for rel in ('tools/real-e2e/sweep', 'docs'):
            (world / rel).mkdir(parents=True)
        for name in ('linux-stage.sh', 'common.sh', 'plan.py', 'plan.json'):
            shutil.copy(SWEEP / name, world / 'tools/real-e2e/sweep' / name)
        shutil.copy(c.ROOT / 'docs/release-source.sha', world / 'docs/release-source.sha')
        lib = world / 'install/marathon/lib'; (lib).mkdir(parents=True)
        (lib / 'a.jar').write_bytes(b'jar'); (world / 'install/marathon/bin').mkdir()
        (world / 'install/marathon/bin/marathon').write_text('#!/bin/sh\n')
        clone = world / 'tools/oracle' / ('build-' + c.release_sha()) / 'marathon-src/cli/build'
        clone.mkdir(parents=True); (clone / 'install').symlink_to(world / 'install')
        bin_dir = world / 'bin'; bin_dir.mkdir()
        self.stub('world/bin/git', r'''case "$*" in
  *"bundle create"*) for last; do :; done; echo bundle > "$5";;
  *rev-parse*) echo 3333333333333333333333333333333333333333;;
  *pack-objects*) cat > /dev/null; echo pack;;
  *) :;;
esac''')
        real_tar = subprocess.run(['which', 'tar'], capture_output=True, text=True).stdout.strip()
        # STUB_APPLEDOUBLE: the archive gains a `._` member afterwards, as with a tar that does not honour COPYFILE_DISABLE
        append = ("import sys, tarfile\n"
                  "with tarfile.open(sys.argv[1], 'a') as t: t.addfile(tarfile.TarInfo('marathon/lib/._a.jar'))\n")
        (world / 'append.py').write_text(append)
        self.stub('world/bin/tar', 'echo "COPYFILE_DISABLE=${COPYFILE_DISABLE:-unset} tar $*" >> "%s"\n%s "$@" || exit $?\n'
                  '[ -z "${STUB_APPLEDOUBLE:-}" ] || { [ "$1" = -C ] && %s -I "%s" "$4"; }\nexit 0'
                  % (world / 'tar.log', real_tar, sys.executable, world / 'append.py'))
        old = self.root / 'old'
        for name in SweepPlan.scenarios('linux-kvm'):
            for key in ('old_selection', 'old_draft'):
                (old / SweepPlan.get(name, key)).parent.mkdir(parents=True, exist_ok=True)
                (old / SweepPlan.get(name, key)).write_text('{}')
        (world / 'runs').mkdir()
        (world / 'runs/reference.json').write_text('{}')
        c.save(world / 'host.json', dict(profile='local-27', kotlin_source=str(world / 'kotlin')))
        return world

    def stage(self, world, **extra):
        env = dict(self.env, **extra, PATH=str(world / 'bin') + os.pathsep + os.environ['PATH'], MRS_HOST=str(world / 'host.json'),
                   MRS_RUNS=str(world / 'runs'), MRS_OLD=str(self.root / 'old'))
        return subprocess.run(['bash', str(world / 'tools/real-e2e/sweep/linux-stage.sh'), str(world / 'stage')],
                              env=env, capture_output=True, text=True)

    def test_the_worker_install_refuses_a_distribution_with_appledouble_entries(self):
        wk, result = self.install_world(jvm_entries=('marathon/lib/._a.jar',))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('AppleDouble entry', result.stderr)
        self.assertFalse((wk / 'host.json').exists())
        self.assertFalse((wk / 'jvm/marathon').exists())
        self.assertFalse([x for x in self.calls() if x.startswith('cargo')])

    def install_world(self, jvm_entries=(), **env):
        """A worker directory with stub git/cargo; real tar, python and bash."""
        wk = self.root / 'wk'
        for d in ('checkout/docs', 'stage'):
            (wk / d).mkdir(parents=True, exist_ok=True)
        (wk / 'checkout/docs/release-source.sha').write_text(c.release_sha() + '\n')
        for name in ('frozen.pack', 'release.pack'):
            (wk / 'stage' / name).write_bytes(b'pack')
        (wk / 'stage/reference.json').write_text('{}')
        import tarfile
        with tarfile.open(wk / 'stage/jvm.tar', 'w') as t:
            t.add(self.root, arcname='marathon', recursive=False)
            for name in jvm_entries:
                t.addfile(tarfile.TarInfo(name))
        sample = self.root / 'sample.tar'
        with tarfile.open(sample, 'w') as t:
            for name in ('sample/android-app/', 'sample/android-app/a.kt', 'sample/android-app/b.kt'):
                info = tarfile.TarInfo(name); info.type = tarfile.DIRTYPE if name.endswith('/') else tarfile.REGTYPE; t.addfile(info)
        bin_dir = self.root / 'bin'; bin_dir.mkdir(exist_ok=True)
        self.stub('bin/git', r'''echo "git $*" >> "$STUB_LOG"
[ "$1" = -C ] && shift 2
case "$1" in
  archive) [ -n "${STUB_ARCHIVE_RC:-}" ] && exit "$STUB_ARCHIVE_RC"; cat "$STUB_TAR";;
  unpack-objects) cat > /dev/null;;
  rev-parse) case "$*" in *e47352f2*) echo "${STUB_FROZEN_TREE:-2222222222222222222222222222222222222222}";; *) echo 3333333333333333333333333333333333333333;; esac;;
esac''')
        self.stub('bin/cargo', 'echo "cargo $*" >> "$STUB_LOG"; mkdir -p "$CARGO_TARGET_DIR/release"; echo bin > "$CARGO_TARGET_DIR/release/marathon"')
        base = self.root / 'base-host.json'; c.save(base, dict(profile='linux-kvm', adb_port=45037))
        merged = dict(self.env, PATH=str(bin_dir) + os.pathsep + os.environ['PATH'], WK=str(wk), RUSTUP_TOOLCHAIN='stable', STUB_TAR=str(sample),
                      STUB_FROZEN_TREE='3333333333333333333333333333333333333333')
        merged.update(env)
        env = merged
        return wk, subprocess.run(['bash', str(SWEEP / 'worker-install.sh'), str(base)], env=env, capture_output=True, text=True)

    def test_the_worker_install_writes_its_config_only_after_a_verified_sample(self):
        wk, result = self.install_world()
        self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
        self.assertEqual((wk / 'host.json').stat().st_mode & 0o777, 0o600)
        self.assertRegex(result.stdout, r'entries: +3')
        log = self.calls()
        self.assertLess(max(i for i, x in enumerate(log) if x.startswith('git') and ' archive ' in x), min(i for i, x in enumerate(log) if x.startswith('cargo')))

    def test_a_failing_sample_archive_stops_the_install_before_cargo_and_the_config(self):
        # R2-sol-4: git archive exiting 42 inside an echo substitution used to print "0 entries" and carry on
        wk, result = self.install_world(STUB_ARCHIVE_RC='42')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('git archive failed', result.stderr)
        self.assertFalse((wk / 'host.json').exists())
        self.assertFalse([x for x in self.calls() if x.startswith('cargo')])

    def test_an_empty_or_unequal_sample_stops_the_install(self):
        empty = self.root / 'empty.tar'; empty.write_bytes(b'')
        wk, result = self.install_world(STUB_TAR=str(empty))
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((wk / 'host.json').exists())
        self.assertFalse([x for x in self.calls() if x.startswith('cargo')])

    def test_a_different_sample_tree_stops_the_install(self):
        self.log.write_text('')
        wk, result = self.install_world(STUB_FROZEN_TREE='4444444444444444444444444444444444444444')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('differs between the frozen and the release source', result.stderr)
        self.assertFalse((wk / 'host.json').exists())

    def oracle_copy(self):
        oracle = self.root / 'tools/oracle'
        oracle.mkdir(parents=True)
        for name in ('build.sh', 'build-receipt.py'):
            (oracle / name).write_bytes((c.ROOT / 'tools/oracle' / name).read_bytes()); (oracle / name).chmod(0o755)
        return oracle

    def test_early_build_failures_remove_the_stale_receipt_of_their_variant_only(self):
        # R2-sol-5: tool-check, clone and source failures happen before Gradle; the old receipt must not survive them
        oracle, sha = self.oracle_copy(), c.release_sha()
        build = oracle / ('build-' + sha); build.mkdir()
        java = self.root / 'jdk/bin/java'; java.parent.mkdir(parents=True)
        java.write_text('#!/bin/sh\necho "openjdk version stub" >&2\n'); java.chmod(0o755)
        for label, env in (('no JDK', dict(JAVA_HOME=str(self.root / 'no-jdk'))),
                           ('source unavailable', dict(JAVA_HOME=str(self.root / 'jdk'), MARATHON_KT=str(self.root / 'no-source')))):
            (build / 'receipt-reference.json').write_text('stale'); (build / 'receipt-patched.json').write_text('other variant')
            result = subprocess.run(['bash', str(oracle / 'build.sh'), 'reference'], capture_output=True, text=True,
                                    env=dict(os.environ, ORACLE_SOURCE_SHA=sha, **env))
            self.assertNotEqual(result.returncode, 0, label)
            self.assertFalse((build / 'receipt-reference.json').exists(), label)
            self.assertTrue((build / 'receipt-patched.json').exists(), label)

    def test_the_receipt_helper_is_tracked_and_only_build_directories_are_ignored(self):
        # R2-sol-1: the helper sat under an ignore rule, so a fresh checkout lacked it
        def ignored(path):
            return subprocess.run(['git', '-C', str(c.ROOT), 'check-ignore', '-q', path]).returncode == 0
        self.assertFalse(ignored('tools/oracle/build-receipt.py'))
        self.assertTrue(ignored('tools/oracle/build-%s/receipt-reference.json' % c.release_sha()))
        self.assertTrue(ignored('tools/oracle/build/marathon-src'))
        listed = subprocess.run(['git', '-C', str(c.ROOT), 'ls-files', '--error-unmatch', 'tools/oracle/build-receipt.py',
                                 'tools/real-e2e/legacy-captures.json'], capture_output=True, text=True)
        self.assertEqual(listed.returncode, 0, listed.stderr)

    def test_the_runbook_blocks_call_the_committed_scripts(self):
        text = (c.ROOT / 'docs/wp/7.5-findings.md').read_text()
        runbook = text[text.index('## Closure sweep runbook'):]
        for step in ('gates', 'reference', 'build', 'prepare', 'selection', 'jvm', 'qualify', 'native', 'compare', 'audit'):
            self.assertTrue('$S ' + step in runbook, 'runbook lacks the step ' + step)
        self.assertTrue('guard()' not in runbook and 'cap.sh' not in runbook)  # the guard lives in sweep/common.sh, tested here


if __name__ == '__main__':
    unittest.main()
