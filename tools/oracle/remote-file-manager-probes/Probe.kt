import com.malinskiy.marathon.apple.AppleDevice
import com.malinskiy.marathon.apple.RemoteFileManager
import com.malinskiy.marathon.apple.cmd.CommandResult
import com.malinskiy.marathon.apple.model.Sdk
import com.malinskiy.marathon.config.vendor.apple.ios.Type
import com.malinskiy.marathon.test.Test
import com.malinskiy.marathon.test.TestBatch
import ch.qos.logback.classic.Logger
import ch.qos.logback.classic.spi.ILoggingEvent
import ch.qos.logback.core.read.ListAppender
import kotlinx.coroutines.*
import java.lang.reflect.Proxy
import java.io.IOException
import kotlin.coroutines.Continuation
import kotlin.coroutines.resumeWithException
import kotlin.coroutines.intrinsics.COROUTINE_SUSPENDED

// Integer UTF-16 units retain lone surrogates independently of the JSON encoder.
fun units(s: String) = s.map { it.code }
fun emit(value: Any) = println(com.google.gson.GsonBuilder().serializeNulls().disableHtmlEscaping().create().toJson(value))
fun main() = runBlocking {
    var storage = "/s"
    var sdk = Sdk.IPHONESIMULATOR
    var abi = "x86_64"
    var udid = "U"
    var mode = "success"
    val commands = mutableListOf<List<String>>()
    val fake = Proxy.newProxyInstance(AppleDevice::class.java.classLoader, arrayOf(AppleDevice::class.java)) { _, method, args ->
        when(method.name) {
            "getStoragePath" -> storage
            "getUdid" -> udid
            "getSdk" -> sdk
            "getAbi" -> abi
            "executeWorkerCommand" -> {
                @Suppress("UNCHECKED_CAST")
                commands.add(args[0] as List<String>)
                when(mode) {
                    "null" -> null
                    "io" -> {
                        @Suppress("UNCHECKED_CAST")
                        val continuation = args.last() as Continuation<CommandResult?>
                        continuation.resumeWithException(IOException("probe io"))
                        COROUTINE_SUSPENDED
                    }
                    "cancel" -> throw CancellationException("probe cancel")
                    else -> CommandResult(listOf(" stdout "), listOf(when(mode) { "blank" -> "\u00a0\t\n "; "stderr" -> " \u00a0problem\u00a0 "; else -> "" }), when(mode) { "nonzero" -> 1; "timeout" -> null; "143" -> 143; else -> 0 })
                }
            }
            else -> error("unexpected ${method.name}")
        }
    } as AppleDevice
    val logger = org.slf4j.LoggerFactory.getLogger("com.malinskiy.marathon.apple.RemoteFileManager") as Logger
    logger.isAdditive = false
    val appender = ListAppender<ILoggingEvent>().apply { start() }
    logger.addAppender(appender)
    for (s in listOf("/s", "/s/", "/s//", "")) {
        storage = s
        val fm = RemoteFileManager(fake)
        val paths = linkedMapOf(
            "directory" to fm.remoteDirectory(), "root" to fm.remoteTestRoot(),
            "shared" to fm.remoteSharedDirectory(), "media" to fm.remoteSharedMediaDirectory(),
            "xctestrun" to fm.remoteXctestrunFile(), "xctest" to fm.remoteXctestFile(),
            "runner" to fm.remoteTestRunnerApplication(), "parser" to fm.remoteXctestParserFile(),
            "app" to fm.remoteApplication(), "log" to fm.remoteLog(), "pid" to fm.remoteVideoPidfile(),
            "xcresult" to fm.remoteXcresultFile(TestBatch(emptyList(), "b/id/")),
            "xctestrunName" to fm.xctestrunFileName(), "appName" to fm.appUnderTestFileName(), "runnerName" to fm.testRunnerFileName()
        )
        Type.values().forEach { paths["screenshot/${it.value}"] = fm.remoteScreenshot("Q", it) }
        for (name in listOf("x", "x/", "/abs/", "x//", "", "/")) {
            paths["extra/$name"] = fm.remoteExtraApplication(name)
            paths["media/$name"] = fm.remoteMediaFile(name)
            paths["parent/$name"] = fm.parentOf(name)
            paths["join/$name"] = fm.joinPath(s, name, "/", "")
        }
        emit(mapOf("kind" to "paths", "storage" to s, "paths" to paths))
        for ((pkg, clazz, method) in listOf(Triple("", "C", "m"), Triple("\u00a0\t", "C", "m"), Triple("p", "é😀/C", "x\ud800?"))) {
            val t = Test(pkg, clazz, method, emptyList())
            for (batch in listOf("b/id", "q\udfff")) emit(mapOf("kind" to "video", "storage" to s, "pkg" to units(pkg), "clazz" to units(clazz), "method" to units(method), "batch" to units(batch), "video" to units(fm.remoteVideoForTest(t,batch)), "temp" to units(fm.remoteTempVideoForTest(t,batch))))
        }
    }
    storage = "/s"
    val fm = RemoteFileManager(fake)
    val operations: Map<String, suspend () -> Any?> = linkedMapOf(
        "mkdir" to { fm.createRemoteDirectory() }, "mkdir-explicit" to { fm.createRemoteDirectory("/d/") },
        "shared" to { fm.createRemoteSharedDirectory() }, "media" to { fm.createRemoteSharedMediaDirectory() },
        "remove" to { fm.removeRemoteDirectory() }, "keep" to { fm.removeRemoteDirectory(listOf("/x/a*", "/x/b/")) },
        "path" to { fm.removeRemotePath("/p") }, "copy" to { fm.copy("/a", "/b") },
        "copy-no" to { fm.copy("/a", "/b", false) }, "move" to { fm.move("/a", "/b") },
        "symlink" to { fm.symlink("/a", "/b") }, "symlink-no" to { fm.symlink("/a", "/b", false) },
        "exists" to { fm.exists("/p") }, "test" to { fm.test("/p") }
    )
    for (m in listOf("null", "success", "blank", "stderr", "nonzero", "timeout", "143", "io", "cancel")) {
        mode = m
        for ((name, action) in operations) {
            commands.clear(); appender.list.clear()
            var thrown: String? = null
            val result = try { val r=action(); if(r is Unit) "unit" else r } catch (e:Exception) { thrown=e.javaClass.name; null }
            emit(mapOf("kind" to "command", "operation" to name, "mode" to m, "argv" to commands, "result" to result, "thrown" to thrown, "logs" to appender.list.map { mapOf("logger" to it.loggerName, "level" to it.level.toString(), "message" to it.formattedMessage, "throwable" to it.throwableProxy?.className) }))
        }
    }
    val defaults=Class.forName("com.malinskiy.marathon.apple.AppleDevice\$DefaultImpls")
    val getArch=defaults.getMethod("getArch", AppleDevice::class.java)
    for (s in Sdk.values()) for(a in listOf("x86_64", "arm64", "unknown")) for(u in listOf("U", "U-D")) {
        sdk=s;abi=a;udid=u
        emit(mapOf("kind" to "arch", "sdk" to sdk.name, "abi" to abi, "udid" to udid, "arch" to getArch.invoke(null,fake).toString()))
    }
}
