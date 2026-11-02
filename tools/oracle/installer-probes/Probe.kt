import com.google.gson.*
import com.dd.plist.*
import com.malinskiy.marathon.apple.*
import com.malinskiy.marathon.apple.bin.AppleBinaryEnvironment
import com.malinskiy.marathon.apple.cmd.*
import com.malinskiy.marathon.apple.cmd.local.KotlinProcessHost
import com.malinskiy.marathon.apple.model.*
import com.malinskiy.marathon.apple.xctestrun.TestRootFactory
import com.malinskiy.marathon.config.vendor.VendorConfiguration
import com.malinskiy.marathon.config.Configuration
import com.malinskiy.marathon.config.vendor.apple.*
import com.malinskiy.marathon.config.vendor.apple.ios.*
import com.malinskiy.marathon.device.OperatingSystem
import kotlinx.coroutines.*
import kotlinx.coroutines.channels.*
import java.io.File
import java.lang.reflect.Proxy
import java.nio.charset.Charset
import java.time.Duration
import java.util.Base64
import kotlin.coroutines.*
import kotlin.coroutines.intrinsics.*

val gson = GsonBuilder().serializeNulls().disableHtmlEscaping().create()
class Session(val result: CommandResult): CommandSession {
    override val stdout: ReceiveChannel<String> = Channel<String>().apply { close() }
    override val stderr: ReceiveChannel<String> = Channel<String>().apply { close() }
    override val exitCode = CompletableDeferred(result.exitCode)
    override val isAlive = false
    override suspend fun await() = result
    override suspend fun drain() = Unit
    override fun terminate() = Unit
    override fun kill() = Unit
    override fun close() = Unit
}
fun JsonObject.s(k: String, default: String = "") = get(k)?.asString ?: default
fun JsonObject.b(k: String) = get(k)?.asBoolean ?: false
@OptIn(ExperimentalCoroutinesApi::class)
fun main(args: Array<String>) = kotlinx.coroutines.test.runTest {
    File(args[0]).readLines().forEach { line ->
        val started = testScheduler.currentTime
        val input = JsonParser.parseString(line).asJsonObject
        val root = File(args[1], input.s("id"))
        val sdk = Sdk.valueOf(input.s("sdk"))
        val commands = mutableListOf<Any>()
        val events = mutableListOf<Any>()
        val pushes = mutableListOf<Any>()
        val temporaries = mutableListOf<File>()
        var xml: String? = null
        var lipo = 0
        var attempts = 0
        val logs = mutableListOf<Any>()
        com.malinskiy.marathon.log.MarathonLogging.debug = true
        val logContext = org.slf4j.LoggerFactory.getILoggerFactory() as ch.qos.logback.classic.LoggerContext
        val appender = object: ch.qos.logback.core.AppenderBase<ch.qos.logback.classic.spi.ILoggingEvent>() {
            override fun append(event: ch.qos.logback.classic.spi.ILoggingEvent) {
                if(event.loggerName == "com.malinskiy.marathon.apple.AppleApplicationInstaller")
                    logs.add(linkedMapOf("logger" to event.loggerName,"level" to event.level.toString(),"message" to event.formattedMessage.replace(root.absolutePath,"<bundle>")))
            }
        }
        appender.context=logContext; appender.start()
        logContext.getLogger(org.slf4j.Logger.ROOT_LOGGER_NAME).addAppender(appender)
        val shared = SharedTransfers()
        val executor = object: CommandExecutor {
            override val host = KotlinProcessHost("local", Charsets.UTF_8)
            override val logger = mu.KotlinLogging.logger("probe.factory")
            override val connected = true
            override suspend fun execute(command: List<String>, timeout: Duration, idleTimeout: Duration, env: Map<String,String>, workdir: String?, charset: Charset): CommandSession {
                val request = linkedMapOf("argv" to command, "timeoutMillis" to timeout.toMillis(), "idleMillis" to idleTimeout.toMillis())
                commands.add(request); events.add(request)
                val stdout = when(command.first()) {
                    "xcrun" -> listOf("/Xcode/Platforms/${sdk.platformName}.platform")
                    "lipo" -> if (command.last() == "-archs") listOf(input.s(if(lipo++ % 2 == 0) "testArch" else "runnerArch", "arm64")) else emptyList()
                    "sh" -> when(input.s("type")) {
                        "detect-ui" -> listOf("XCUIApplication")
                        "detect-kif" -> listOf("KIFTestCase")
                        "detect-both" -> listOf("XCUIApplication", "KIFTestCase")
                        else -> emptyList()
                    }
                    else -> emptyList()
                }
                val mode = input.s("mode")
                if (command.first() == "test" && mode == "exists-throw") throw java.io.IOException("exists io")
                if(command.first()=="mkdir" && mode=="mkdir-cancel") throw CancellationException("mkdir cancelled")
                val exit = when {
                    command.first()=="mkdir" && mode=="mkdir-fail" -> 1
                    command.take(2)==listOf("test","-e") && command[2]==input.s("missing") -> if(input.b("unknown")) 2 else 1
                    input.s("missing").isNotEmpty() && command.any { it.contains(input.s("missing")) } -> 1
                    command.first() == "test" && (mode == "exists-missing" || mode == "exists-test-missing" && command.last().contains(".xctest/")) -> 1
                    command.first() == "test" && (mode == "exists-unknown" || mode == "exists-test-unknown" && command.last().contains(".xctest/")) -> 2
                    command.first().endsWith("PlistBuddy") && mode == "plist-false" -> 1
                    else -> 0
                }
                return Session(CommandResult(stdout, emptyList(), exit))
            }
            override fun close() = Unit
        }
        val configuration = Configuration.Builder("probe", File(root, "out")).build()
        val timeouts = TimeoutConfiguration(shell = Duration.ofSeconds(31), shellIdle = Duration.ofSeconds(32))
        val binary = AppleBinaryEnvironment(executor, configuration, timeouts, gson)
        lateinit var fm: RemoteFileManager
        val udid = input.s("udid", "a")
        val fake = Proxy.newProxyInstance(AppleDevice::class.java.classLoader, arrayOf(AppleDevice::class.java)) { _, method, a ->
            try { when (method.name) {
                "getStoragePath" -> "/tmp/marathon/$udid"
                "getUdid" -> udid
                "getSerialNumber" -> "$udid@local"
                "getSdk" -> sdk
                "getArch" -> Arch.valueOf(input.s("arch", "arm64"))
                "getOperatingSystem" -> OperatingSystem(input.s("os", "15.0"))
                "getRemoteFileManager" -> fm
                "getBinaryEnvironment" -> binary
                "executeWorkerCommand" -> {
                    @Suppress("UNCHECKED_CAST")
                    val command = a[0] as List<String>
                    @Suppress("UNCHECKED_CAST")
                    val continuation = a.last() as Continuation<CommandResult?>
                    val action: suspend () -> CommandResult? = { executor.safeExecute(command, timeouts.shell, timeouts.shellIdle, emptyMap(), null) }
                    action.startCoroutineUninterceptedOrReturn(continuation)
                }
                "pushShared" -> {
                    val src=a[0] as File; val dst=a[1] as String
                    @Suppress("UNCHECKED_CAST")
                    val continuation = a.last() as Continuation<Boolean>
                    val action: suspend () -> Boolean = {
                        shared.send("local",src,dst,{fm.exists(dst)},{
                            val push=linkedMapOf("kind" to "shared","src" to src.relativeTo(root).path,"dst" to dst)
                            pushes.add(push);events.add(push)
                            attempts++
                            when(input.s("mode")) {
                                "retry-success" -> attempts>2
                                "retry-fail" -> false
                                "retry-throw" -> throw java.io.IOException("transfer io")
                                "retry-cancel" -> throw CancellationException("transfer cancelled")
                                "delay-cancel" -> { currentCoroutineContext()[Job]!!.cancel(CancellationException("delay cancelled")); false }
                                "extras-false" -> !src.name.startsWith("first") && !src.name.startsWith("second")
                                else -> true
                            }
                        })
                    }
                    action.startCoroutineUninterceptedOrReturn(continuation)
                }
                "install" -> { events.add(linkedMapOf("install" to a[0])); input.s("mode")!="extras-false" }
                "pushGeneratedFile" -> {
                    val src = a[0] as File; temporaries.add(src)
                    xml = Base64.getEncoder().encodeToString(src.readBytes())
                    val push = linkedMapOf("kind" to "generated", "dst" to a[1], "existsDuringSend" to src.exists(), "suffix" to src.name.endsWith("xctestrun"))
                    pushes.add(push); events.add(push)
                    when(input.s("mode")) {
                        "push-false" -> false
                        "push-throw" -> throw java.io.IOException("push io")
                        "push-cancel" -> throw CancellationException("push cancelled")
                        "delete-fail" -> { src.delete(); src.mkdir(); File(src,"child").writeText("keep"); false }
                        else -> true
                    }
                }
                else -> error("unexpected ${method.name}")
            } } catch (e: Throwable) {
                @Suppress("UNCHECKED_CAST")
                val continuation = a.last() as Continuation<Any?>
                continuation.resumeWithException(e)
                COROUTINE_SUSPENDED
            }
        } as AppleDevice
        fm = RemoteFileManager(fake)
        var error: Any? = null
        try {
            val kind=input.s("type").takeUnless { it.startsWith("detect-") }?.let { TestType.valueOf(it) }
            val extra=if(input.s("mode").startsWith("extras")) listOf("first.app","wrong.APP",".app","second.app").map { File(root,it) } else null
            val config=AppleTestBundleConfiguration(application=if(input.get("raw")?.asBoolean!=false) File(root,input.s("app")) else null,testApplication=File(root,if(input.b("reuse")) input.s("runner") else input.s("test")),testType=kind,extraApplications=extra,derivedDataDir=if(input.b("derived")) root else null)
            if(input.b("derived")) config.validate() else {
                config.app=if(input.s("mode")=="no-app") null else File(root,input.s("app"))
                config.testApp=if(input.b("reuse")) File(root,input.s("runner")) else null
                config.xctest=File(root,input.s("test"))
            }
            val vendor: VendorConfiguration = if(input.s("mode")=="no-env") VendorConfiguration.StubVendorConfiguration else if(sdk==Sdk.MACOS) VendorConfiguration.MacosConfiguration(bundle=config) else VendorConfiguration.IOSConfiguration(bundle=if(input.s("mode")=="no-bundle") null else config,skipCopyingDerivedData=input.b("skip"))
            if(input.s("mode")=="no-result") vendor.javaClass.getDeclaredField("xcresult").apply { isAccessible=true }.set(vendor,null)
            if(input.b("requireOnly")) {
                fake.requireDistributedTestBundle(AppleTestBundle(config.app,config.testApp,config.xctest,sdk),{ com.malinskiy.marathon.exceptions.TestParsingException(it) })
            } else {
                val installer=object: AppleApplicationInstaller<AppleDevice>(vendor) {
                    override suspend fun afterInstall(device:AppleDevice,bundle:AppleTestBundle) {
                        if(!input.b("noop")) events.add(linkedMapOf("hook" to bundle.appId))
                        if(input.s("mode")=="hook-fail") throw java.io.IOException("hook io")
                    }
                }
                // A child job lets cancellation be captured without cancelling the corpus driver.
                supervisorScope { async { installer.prepareInstallation(fake,input.b("parser")) }.await() }
            }
        } catch(e:Throwable) {
            error = linkedMapOf("class" to e.javaClass.name, "message" to e.message?.replace(root.absolutePath,"<bundle>"), "reason" to (e as? com.malinskiy.marathon.apple.logparser.parser.DeviceFailureException)?.reason?.toString())
        }
        if(input.s("mode") == "writer-fail") System.setSecurityManager(null)
        println(gson.toJson(linkedMapOf("input" to input, "events" to events, "commands" to commands, "pushes" to pushes, "xml" to xml, "error" to error, "cleaned" to temporaries.all { !it.exists() }, "logs" to logs, "elapsedMillis" to (testScheduler.currentTime-started))))
        temporaries.forEach { it.deleteRecursively() }
        logContext.getLogger(org.slf4j.Logger.ROOT_LOGGER_NAME).detachAppender(appender)
    }
}
