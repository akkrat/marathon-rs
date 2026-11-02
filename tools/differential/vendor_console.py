"""Narrow D1/D12 and typed CLI text substitutions; retain stdout/stderr order."""
import re
import evidence as e

SLF4J = 'SLF4J(I): Connected with provider of type [ch.qos.logback.classic.spi.LogbackServiceProvider]\n'
D1 = 'Crash reporting and usage analytics are not supported by this build; --bugsnag, --analyticsTracking, bugsnagReporting and analyticsTracking are ignored'
THREAD = re.compile(r'^((?:[A-Z] )?\d{2}:\d{2}:\d{2}\.\d{3}) \[[^\]]+\]')
SYSTEM = '<com.malinskiy.marathon.Marathon> '
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
        if SYSTEM + 'System Information:\n' in line:
            e.require(not system_seen,'duplicate System Information block')
            system = system_seen = True
            result.append('System Information: {D12}\n')
            continue
        if system:
            if SYSTEM in line:
                key,sep,_ = line.split(SYSTEM,1)[1].partition(': ')
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
        if match:
            line = THREAD.sub(r'\1 [thread]',line,count=1)
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
    # after it until the work completes, between the caller's later lines. The
    # worker logs the wait before running the work (processWorker), and both the
    # owner's barrier (await) and finalization log only after the work completes,
    # so the window ends at the first of them. Each wait line moves to just after
    # its own batch's admission; it must lie in that window, and every other stdout
    # line keeps its order.
    admission = ' <c.m.marathon.apple.ArtifactProcessor> Artifact admission for '
    waiting = ' <c.m.marathon.apple.ArtifactProcessor> Artifact queue wait for '
    finalization = ' <c.m.marathon.apple.ArtifactProcessor> Artifact finalization took '
    barrier = ' <c.m.marathon.apple.ArtifactProcessor> Artifact barrier for '
    batch = lambda line, marker: line.split(marker, 1)[1].split(':', 1)[0].split(' took ', 1)[0]
    for line in [line for line in result if waiting in line]:
        key = batch(line, waiting)
        at = result.index(line)
        owners = [i for i, other in enumerate(result) if admission in other and batch(other, admission) == key]
        e.require(len(owners) == 1, 'artifact queue wait without exactly one admission')
        owner = key.split(', batch ', 1)[0]
        end = next((i for i, other in enumerate(result) if i > owners[0] and (finalization in other or (
            barrier in other and other.split(barrier, 1)[1].rsplit(' took ', 1)[0] == owner))), len(result))
        e.require(at + 1 == owners[0] or owners[0] < at < end, 'artifact queue wait outside its admission window')
        del result[at]
        result.insert(owners[0] if at < owners[0] else owners[0] + 1, line)
    # initialize launches the independent update consumer before returning.
    # Its two-line update event can precede or follow the caller's timing log.
    timing = ' <Timings> Device provider initialized in '
    updates = ' <AppleSimulatorProvider> Processing updates from '
    for i in range(len(result)-2):
        triple = result[i:i+3]
        if (timing in triple[0] and updates in triple[1] and triple[2].startswith('- Connected(target=Simulator(')):
            continue
        if (updates in triple[0] and triple[1].startswith('- Connected(target=Simulator(') and timing in triple[2]):
            result[i:i+3] = [triple[2],triple[0],triple[1]]
    return ''.join(result).encode()
