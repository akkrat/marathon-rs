"""Copy pinned sources to a temporary compiler tree, adding observational probes only.
BatchAccounting uses the same virtual dispatcher as the scheduling suites; its latch
is replaced by consecutive sends before a dispatch. The source checkout is read-only.
"""
from pathlib import Path
import os
import re
import subprocess
ROOT = Path(os.environ.get('MARATHON_SOURCE', os.path.expanduser('~/src/marathon')))
K = ROOT / 'core/src/main/kotlin/com/malinskiy/marathon'
T = ROOT / 'core/src/test/kotlin/com/malinskiy/marathon/execution'

def sources(tmp):
    commit = subprocess.check_output(['git', '-C', str(ROOT), 'rev-parse', 'HEAD'], text=True).strip()
    if not commit.startswith('e47352f2'):
        raise RuntimeError('pool probes require the frozen e47352f2 source checkout')
    result=[]
    def put(name,s):
        p=tmp/name;p.write_text(s);result.append(p)
    put('StubDevice.kt',(ROOT/'vendor/vendor-test/src/main/kotlin/com/malinskiy/marathon/test/StubDevice.kt').read_text())
    pool=(K/'execution/DevicePoolActor.kt').read_text()
    pool=pool.replace('when (msg) {','oracle.pool.Trace.message("pool", poolId.name, msg)\n        when (msg) {',1)
    pool=pool.replace('val sent = queue.trySend(msg)', 'oracle.pool.Trace.message("queue", poolId.name, msg)\n        val sent = queue.trySend(msg)')
    pool=pool.replace('private suspend fun terminate() {','private suspend fun terminate() {\n        coroutineContext.job.invokeOnCompletion { oracle.pool.Trace.terminated(poolId.name) }')
    put('DevicePoolActor.kt',pool)
    for kind in ['Scheduling','BatchAccounting','SchedulingCharacterization']:
        s=(T/f'DevicePoolActor{kind}Test.kt').read_text()
        s=re.sub(r'import org.junit.jupiter.api.\w+\n','',s)
        s=s.replace('@Test','@oracle.pool.ProbeTest').replace('@AfterEach','')
        # Observe calls while retaining the source assertions and actual actors.
        s=s.replace('events.add("${testResult.test.method}', 'oracle.pool.Trace.tracks.add("${testResult.test.method} ${testResult.status} ${if (final) "final" else "non-final"} on ${device.serialNumber}")\n            events.add("${testResult.test.method}')
        s=s.replace('= runTest {','= runTest {\n        oracle.pool.Trace.clock = { testScheduler.currentTime }')
        s=s.replace('            runTest {','            runTest {\n                oracle.pool.Trace.clock = { testScheduler.currentTime }')
        if kind=='BatchAccounting':
            s='@file:OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)\n'+s
            s=s.replace('import kotlinx.coroutines.runBlocking','import kotlinx.coroutines.test.runTest\nimport kotlinx.coroutines.test.StandardTestDispatcher')
            s=s.replace('Executors.newSingleThreadExecutor().asCoroutineDispatcher()','StandardTestDispatcher()')
            s=s.replace('dispatcher.close()','')
            s=s.replace('= runBlocking {','= runTest(dispatcher) {\n        oracle.pool.Trace.clock = { testScheduler.currentTime }')
            s=s.replace('        dispatcher.executor.execute { release.await() }','')
        if kind=='SchedulingCharacterization':
            s=s.replace('rows.forEach { println("SCHEDULING_CHARACTERIZATION $it") }', 'rows.forEach { println("SCHEDULING_CHARACTERIZATION $it"); oracle.pool.Trace.characterization.add(listOf(it.devices.toLong(), it.tests.toLong(), it.firstWave.toLong(), it.makespanMillis, it.idleDeviceMillis)) }')
        put(f'{kind}.kt',s)
    put('Assertions.kt','''package org.amshove.kluent
infix fun <T> T.shouldBeEqualTo(other: T) { check(this == other) { "$this != $other" } }
infix fun <T> T.shouldNotBeEqualTo(other: T) { check(this != other) { "$this == $other" } }
infix fun <T> T.shouldBe(other: T) { check(this == other) { "$this != $other" } }
fun <T> Collection<T>.shouldBeEmpty() { check(isEmpty()) { "not empty: $this" } }
''')
    return result
