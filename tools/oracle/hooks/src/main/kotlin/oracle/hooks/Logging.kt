package oracle.hooks

import ch.qos.logback.classic.PatternLayout
import ch.qos.logback.classic.spi.ILoggingEvent
import ch.qos.logback.core.encoder.LayoutWrappingEncoder
import java.time.Instant

/** Library-owned clock values must cross the same seam as application-owned values. */
class HookPatternLayout : PatternLayout() {
    override fun doLayout(event: ILoggingEvent): String {
        if (!OracleHooks.active) return super.doLayout(event)
        val time = OracleHooks.currentTimeMillis("library.logback.wall")
        val message = event.formattedMessage
        return super.doLayout(object : ILoggingEvent by event {
            override fun getTimeStamp(): Long = time
            override fun getInstant(): Instant = Instant.ofEpochMilli(time)
            override fun getNanoseconds(): Int = 0
            override fun getFormattedMessage(): String = message
        })
    }
}

/** The CLI's default PatternLayoutEncoder, with the layout above. */
class HookPatternLayoutEncoder : LayoutWrappingEncoder<ILoggingEvent>() {
    var pattern: String = ""
    override fun start() {
        layout = HookPatternLayout().also {
            it.context = context
            it.pattern = pattern
            it.start()
        }
        super.start()
    }
}
