"""Narrow D1/D12 and typed CLI text substitutions; retain stdout/stderr order."""
import re
import evidence as e

SLF4J = 'SLF4J(I): Connected with provider of type [ch.qos.logback.classic.spi.LogbackServiceProvider]\n'
D1 = 'Crash reporting and usage analytics are not supported by this build; --bugsnag, --analyticsTracking, bugsnagReporting and analyticsTracking are ignored'
THREAD = re.compile(r'^((?:[A-Z] )?\d{2}:\d{2}:\d{2}\.\d{3}) \[[^\]]+\]')
# WP 8.8 native diagnostic prefix: `HH:mm:ss.SSS LEVEL marathon_<crate>::<module>: `.
# Only the time token of a complete prefix is normalized; the level, target, message,
# fields and continuation lines stay exact, and raw (pinned) lines never match it.
NATIVE = re.compile(r'^\d{2}:\d{2}:\d{2}\.\d{3} (?:TRACE|DEBUG|INFO|WARN|ERROR) marathon_[a-z_]+(?:::[a-z0-9_]+)*: ')
SYSTEM = '<com.malinskiy.marathon.Marathon> '
SYSTEM_NATIVE = 'marathon_core::marathon: '
SSHPASS = '/usr/bin/sshpass'
SSHPASS_FACT = 'host_usr_bin_sshpass'
SSHPASS_WARNING = (f"WARNING: sshpass binary not installed? Was looking for '{SSHPASS}'. "
                   'You can use environment variable RSYNC4J_SSHPASS to point to the binary.\n')


def normalize(data, values, implementation, stream, facts=None):
    text = data.decode('utf-8')
    if stream == 'stderr':
        if implementation == 'jvm':
            e.require(text.count(SLF4J) == 1,'missing/duplicate JVM provider line')
            text = text.replace(SLF4J,'',1)
        if facts is not None and SSHPASS_FACT in facts:
            # Both implementations warn exactly when the capture host lacks the default sshpass; the
            # JVM goldens come from such a host, a hosted Linux runner has one.
            missing = not facts[SSHPASS_FACT]
            e.require(text.count(SSHPASS_WARNING) == int(missing), 'sshpass warning does not match the capture host')
            text = text.replace(SSHPASS_WARNING, '', 1)
        return text.encode()
    result = []
    system = False; system_seen = False
    for line in text.splitlines(keepends=True):
        if SYSTEM + 'System Information:\n' in line or SYSTEM_NATIVE + 'System Information:\n' in line:
            e.require(not system_seen,'duplicate System Information block')
            system = system_seen = True
            result.append('System Information: {D12}\n')
            continue
        if system:
            marker = SYSTEM if SYSTEM in line else SYSTEM_NATIVE if SYSTEM_NATIVE in line else None
            if marker is not None:
                key,sep,_ = line.split(marker,1)[1].partition(': ')
                e.require(sep and re.fullmatch(r'[A-Za-z0-9_.]+',key),'unknown system fact format')
                continue
            system = False
        if line.rstrip().endswith(D1):
            e.require(implementation == 'native','unexpected JVM telemetry replacement')
            continue
        if re.search(r'(?:c\.m\.m\.e\.NoopExceptionsReporter - Noop exceptions reporter started|<c\.m\.m\.exceptions\.NoopExceptionsReporter> Noop exceptions reporter finished)\n$',line):
            e.require(implementation == 'jvm','unexpected native Noop reporter')
            continue
        # Client-specific SSH entropy/handshake logging is diagnostic, just like
        # encrypted handshake packets. Do not discard arbitrary SSH loggers.
        if '<n.s.sshj.transport.random.JCERandom>' in line:
            e.require(implementation == 'jvm' and re.search(r'> (Creating new SecureRandom\.|Random creation took \d+ ms)\n$',line),
                      'unknown SSH runtime line')
            continue
        if '<c.m.m.a.c.r.s.s.c.PerformanceDefaultConfig>' in line:
            e.require(implementation == 'jvm' and '> Available Ciphers [' in line and line.endswith(']\n'),
                      'unknown SSH cipher diagnostic')
            continue
        match = THREAD.match(line)
        native = NATIVE.match(line)
        if match or native:
            if match:
                line = THREAD.sub(r'\1 [thread]',line,count=1)
            else:
                line = '<time>' + line[12:]
            line = line.replace(values['sandbox']+'/', '{sandbox}/')
            line = line.replace(values['safe_serial'],'{safe_serial}').replace(values['serial'],'{serial}')
            line = line.replace('127.0.0.1:' + values['port'],'127.0.0.1:{port}')
            line = line.replace('D.0.0.1:' + values['port'],'D.0.0.1:{port}')
            line = line.replace('port=' + values['port'] + ',', 'port={port},')
        elif re.match(r'^\d+% \| \[omni\]-\[',line):
            line = line.replace('['+values['serial']+']','[{serial}]')
        result.append(line)
    # submit logs its admission after launching an independent artifact worker
    # (ArtifactProcessor.start: scope.launch), which logs its queue wait whenever
    # it gets a transfer permit: just before that admission line, or anywhere
    # after it until finalization joins the work, between the caller's later
    # lines. The owner's barrier line cannot bound the window: await snapshots the
    # pending work under the lock but logs outside it, and the line names no batch,
    # so an await that began before this admission may log between the admission
    # and the wait. Each wait line moves to just after its own batch's admission;
    # it must lie in that window, and every other stdout line keeps its order.
    # The same three markers in the WP 8.8 native spelling; the message after the
    # target is identical, so batch extraction below is shared.
    admissions = [' <c.m.marathon.apple.ArtifactProcessor> Artifact admission for ',
                  'marathon_apple::artifact_processor: Artifact admission for ']
    waitings = [' <c.m.marathon.apple.ArtifactProcessor> Artifact queue wait for ',
                'marathon_apple::artifact_processor: Artifact queue wait for ']
    finalizations = [' <c.m.marathon.apple.ArtifactProcessor> Artifact finalization took ',
                     'marathon_apple::artifact_processor: Artifact finalization took ']
    marker = lambda line, variants: next((v for v in variants if v in line), None)
    batch = lambda line, variants: line.split(marker(line, variants), 1)[1].split(':', 1)[0].split(' took ', 1)[0]
    for line in [line for line in result if marker(line, waitings) is not None]:
        key = batch(line, waitings)
        at = result.index(line)
        owners = [i for i, other in enumerate(result) if marker(other, admissions) is not None and batch(other, admissions) == key]
        e.require(len(owners) == 1, 'artifact queue wait without exactly one admission')
        end = next((i for i, other in enumerate(result) if marker(other, finalizations) is not None), len(result))
        e.require(at + 1 == owners[0] or owners[0] < at < end, 'artifact queue wait outside its admission window')
        del result[at]
        result.insert(owners[0] if at < owners[0] else owners[0] + 1, line)
    # initialize launches the independent update consumer before returning.
    # Its two-line update event can precede or follow the caller's timing log.
    timings = [' <Timings> Device provider initialized in ',
               'marathon_core::timing: Device provider initialized in ',
               'marathon_ios::device::timing: Device provider initialized in ']
    updatess = [' <AppleSimulatorProvider> Processing updates from ',
                'marathon_ios::device::simulator_provider: Processing updates from ']
    timing = lambda line: marker(line, timings) is not None
    updates = lambda line: marker(line, updatess) is not None
    for i in range(len(result)-2):
        triple = result[i:i+3]
        if (timing(triple[0]) and updates(triple[1]) and triple[2].startswith('- Connected(target=Simulator(')):
            continue
        if (updates(triple[0]) and triple[1].startswith('- Connected(target=Simulator(') and timing(triple[2])):
            result[i:i+3] = [triple[2],triple[0],triple[1]]
    # The Android provider launches its device-tracking job (providerJob) before returning, so its
    # CONNECTED lines can also precede the caller's timing log: the timing line moves ahead of
    # the run of CONNECTED lines just before it.
    connected = lambda line: (' <AdamDeviceProvider> Device ' in line or 'marathon_android::provider: Device ' in line) and line.rstrip('\n').endswith(' changed state to CONNECTED')
    for i, line in enumerate(result):
        if timing(line):
            start = i
            while start > 0 and connected(result[start-1]):
                start -= 1
            result[start:i+1] = [line] + result[start:i]
            break
    return ''.join(result).encode()
