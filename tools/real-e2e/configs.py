"""JSON is a YAML subset; templates contain only public Marathon configuration keys."""
from pathlib import Path
from common import port, require


def render(scenario, host, products, tests, device):
    config = dict(name='real-e2e-' + scenario['id'], outputDir='report', debug=True,
                  analyticsTracking=False, bugsnagReporting=False, ignoreFailures=False,
                  poolingStrategy={'type': 'omni'}, sortingStrategy={'type': 'no-sorting'},
                  batchingStrategy={'type': 'fixed-size', 'size': 10, 'lastMileLength': 0},
                  flakinessStrategy={'type': 'ignore'}, retryStrategy={'type': 'no-retry'},
                  uncompletedTestRetryQuota=0, deviceInitializationTimeoutMillis=300000,
                  testBatchTimeoutMillis=600000, testOutputTimeoutMillis=180000,
                  screenRecordingPolicy='ON_ANY')
    if tests is not None:
        config['filteringConfiguration'] = {'allowlist': [{'type': 'fully-qualified-test-name', 'values': tests}]}
    vendor = {'type': scenario['vendor'], 'testParserConfiguration': {'type': scenario['parser']}}
    config['vendorConfiguration'] = vendor
    if scenario['vendor'] == 'Android':
        require(host['profile'] == 'linux-kvm', 'Android requires the Linux KVM profile')
        adb_port = port(host['adb_port'])
        sdk = str(Path(host['sdk']).resolve())
        screenshot = scenario['parser'] == 'remote'
        config['isCodeCoverageEnabled'] = True
        vendor.update(androidSdk=sdk, applicationApk=products['application'],
                      testApplicationApk=products['tests'], adbServers=[{'host': '127.0.0.1', 'port': adb_port}],
                      autoGrantPermission=True, applicationPmClear=True, testApplicationPmClear=True,
                      waitForDevicesTimeoutMillis=60000, allureConfiguration={'enabled': True},
                      testAccessConfiguration={'adb': False, 'grpc': False, 'console': False},
                      instrumentationArgs={'debug': 'false'},
                      timeoutConfiguration={'shell': 'PT2M', 'install': 'PT5M', 'pullFile': 'PT3M',
                                            'pushFile': 'PT3M', 'pushFolder': 'PT3M', 'screenrecorder': 'PT4M'},
                      screenRecordConfiguration={
                          'preferableRecorderType': 'screenshot' if screenshot else 'video',
                          'videoConfiguration': {'enabled': not screenshot},
                          'screenshotConfiguration': {'enabled': screenshot, 'width': 360, 'height': 640, 'delayMs': 500}},
                      fileSyncConfiguration={
                          'push': [{'path': 'fixtures/external-file', 'pathRoot': 'LOCAL_TMP'},
                                   {'path': 'fixtures/external-folder', 'pathRoot': 'LOCAL_TMP'}],
                          'pull': [{'relativePath': 'external-folder', 'pathRoot': 'LOCAL_TMP', 'aggregationMode': 'TEST_RUN'}]})
        if screenshot:
            vendor['testParserConfiguration']['instrumentationArgs'] = {
                'listener': 'com.malinskiy.adam.junit4.android.listener.TestAnnotationProducer'}
        return config, None
    require(host['profile'] in ('local-27', 'ssh-26.6'), 'Apple requires a Mac profile')
    vendor.update(bundle={'application': products['application'], 'testApplication': products['tests'],
                          'testType': scenario['test_type']},
                  devices='Marathondevices', hideRunnerOutput=False,
                  xcresult={'pullingPolicy': 'ALWAYS', 'remoteClean': True},
                  timeoutConfiguration={'shell': 'PT2M', 'shellIdle': 'PT2M', 'install': 'PT5M',
                                        'boot': 'PT3M', 'bootWait': 'PT4M', 'xcresulttool': 'PT5M'})
    if host['profile'] == 'local-27':
        transport = {'type': 'local'}
    else:
        ssh = host['ssh']
        transport = {'type': 'ssh', 'addr': '127.0.0.1', 'port': port(ssh['port']), 'checkReachability': False}
        vendor['ssh'] = {'authentication': {'type': 'publicKey', 'username': ssh['user'], 'key': ssh['key']},
                         'knownHostsPath': ssh['known_hosts']}
    if scenario['vendor'] == 'iOS':
        require(device.get('udid'), 'fresh owned simulator UDID required')
        target = {'type': 'simulator', 'udid': device['udid']}
        vendor.update(lifecycle={'shutdownUnused': False, 'onPrepare': [], 'onDispose': []},
                      workerConnection={'attempts': 1, 'lateJoin': False},
                      screenRecordConfiguration={
                          'preferableRecorderType': 'video' if scenario['id'] == 'ios-ui-video' else 'screenshot',
                          'videoConfiguration': {'enabled': scenario['id'] == 'ios-ui-video', 'codec': 'h264',
                                                 'transcoding': {'enabled': True, 'ffmpegPath': host.get('ffmpeg', 'ffmpeg'), 'size': 'hd720'}},
                          'screenshotConfiguration': {'enabled': scenario['id'] == 'ios-ui-screenshot'}})
    else:
        target = {'type': 'host'}
        if scenario['id'] == 'macos-permissions':
            vendor['permissions'] = {'grant': {'org.marathon.AppleWorld': ['accessibility']}}
    return config, {'workers': [{'transport': transport, 'devices': [target]}]}
