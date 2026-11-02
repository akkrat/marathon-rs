package com.malinskiy.marathon.apple.ios

import ch.qos.logback.classic.Logger
import ch.qos.logback.classic.spi.ILoggingEvent
import ch.qos.logback.core.AppenderBase
import com.google.gson.Gson
import org.junit.jupiter.api.extension.*
import org.slf4j.LoggerFactory
import java.io.File

class SimulatorCapture : BeforeEachCallback, AfterEachCallback {
    private fun emit(row: Any) { synchronized(File::class.java) { File(System.getProperty("simulator.evidence")).appendText(Gson().toJson(row) + "\n") } }
    private val appender = object : AppenderBase<ILoggingEvent>() {
        override fun append(e: ILoggingEvent) {
            emit(mapOf("scenario" to System.getProperty("simulator.scenario"), "kind" to "log", "logger" to e.loggerName, "level" to e.level.toString(), "message" to e.formattedMessage, "throwableClass" to e.throwableProxy?.className, "throwableMessage" to e.throwableProxy?.message))
        }
    }
    override fun beforeEach(context: ExtensionContext) {
        System.setProperty("simulator.scenario", context.requiredTestMethod.name)
        appender.start()
        (LoggerFactory.getLogger(Logger.ROOT_LOGGER_NAME) as Logger).addAppender(appender)
    }
    override fun afterEach(context: ExtensionContext) {
        emit(mapOf("kind" to "testEnd", "scenario" to context.requiredTestMethod.name, "failure" to context.executionException.orElse(null)?.toString()))
        (LoggerFactory.getLogger(Logger.ROOT_LOGGER_NAME) as Logger).detachAppender(appender)
        appender.stop()
    }
}
