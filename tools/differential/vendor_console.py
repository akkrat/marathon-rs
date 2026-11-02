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
    # submit logs its admission after launching an independent artifact worker.
    # The worker can log its queue wait on either side of that admission. Only
    # this adjacent pair is unordered; all other stdout bytes retain order.
    admission = ' <c.m.marathon.apple.ArtifactProcessor> Artifact admission for '
    waiting = ' <c.m.marathon.apple.ArtifactProcessor> Artifact queue wait for '
    for i in range(len(result)-1):
        if ((admission in result[i] and waiting in result[i+1]) or
                (waiting in result[i] and admission in result[i+1])):
            pair = result[i:i+2]
            result[i:i+2] = sorted(pair,key=lambda line: waiting in line)
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
