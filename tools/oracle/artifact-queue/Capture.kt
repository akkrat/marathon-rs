@file:OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class, kotlin.time.ExperimentalTime::class)
package com.malinskiy.marathon.apple

import ch.qos.logback.classic.Logger
import ch.qos.logback.classic.spi.ILoggingEvent
import ch.qos.logback.core.AppenderBase
import com.google.gson.Gson
import org.junit.jupiter.api.extension.*
import org.junit.jupiter.api.Test
import org.slf4j.LoggerFactory
import kotlinx.coroutines.*
import kotlinx.coroutines.test.*
import java.nio.file.Files
import java.io.File

object ArtifactEvidence {
    @Volatile var scenario = "probe"
    @Synchronized fun emit(fields: Map<String, Any?>) {
        File(System.getProperty("artifact.evidence")).appendText(Gson().toJson(mapOf("scenario" to scenario) + fields) + "\n")
    }
}
class ArtifactCapture : BeforeEachCallback, AfterEachCallback {
    private val appender = object : AppenderBase<ILoggingEvent>() {
        override fun append(e: ILoggingEvent) {
            ArtifactEvidence.emit(mapOf("kind" to "log", "level" to e.level.toString(), "logger" to e.loggerName,
                "message" to e.formattedMessage, "throwableClass" to e.throwableProxy?.className, "throwableMessage" to e.throwableProxy?.message))
        }
    }
    override fun beforeEach(context: ExtensionContext) {
        ArtifactEvidence.scenario = context.requiredTestMethod.name
        System.setProperty("artifact.scenario", ArtifactEvidence.scenario)
        appender.start()
        (LoggerFactory.getLogger(Logger.ROOT_LOGGER_NAME) as Logger).addAppender(appender)
    }
    override fun afterEach(context: ExtensionContext) {
        (LoggerFactory.getLogger(Logger.ROOT_LOGGER_NAME) as Logger).detachAppender(appender)
        appender.stop()
    }
}
@org.junit.jupiter.api.extension.ExtendWith(ArtifactCapture::class)
class ArtifactEvidenceTest {
    @Test fun witnesses() = runTest {
        val p = ArtifactProcessor(2, StandardTestDispatcher(testScheduler), clock = { testScheduler.currentTime * 1_000_000 })
        for (s in listOf("first", "second")) p.protect("owner", s) {}
        p.release("owner", "first")
        p.protect("owner", "first") {}
        ArtifactEvidence.emit(mapOf("kind" to "kept", "sources" to p.keptSources("owner")))
        p.closeAndJoin()
        try { p.submit("owner", "closed", onUndelivered = { throw IllegalStateException("callback escaped") }) {} }
        catch (e: Exception) { ArtifactEvidence.emit(mapOf("kind" to "closedCallback", "failure" to e.toString())) }
        val tmp = Files.createTempDirectory("artifact49")
        val source = Files.createDirectory(tmp.resolve("source"))
        val destination = Files.createDirectory(tmp.resolve("destination"))
        for ((name, src) in listOf("exists" to source, "missing" to tmp.resolve("missing"))) {
            try { Files.move(src, destination) } catch (e: Exception) {
                ArtifactEvidence.emit(mapOf("kind" to name, "failure" to e.toString().replace(tmp.toString(), "<tmp>")))
            }
        }
        tmp.toFile().deleteRecursively()
        val measured = ArtifactProcessor(2, StandardTestDispatcher(testScheduler), clock = { testScheduler.currentTime * 1_000_000 })
        measured.submit("clock", "batch") { delay(1000) }
        val barrier = async { measured.await("clock") }
        val before = testScheduler.currentTime
        barrier.await()
        ArtifactEvidence.emit(mapOf("kind" to "measureClock", "injectedElapsedMillis" to testScheduler.currentTime - before))
        measured.closeAndJoin()
    }
}
