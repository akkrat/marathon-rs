import com.google.gson.*
import com.dd.plist.*
import com.malinskiy.marathon.apple.*
import com.malinskiy.marathon.apple.bin.AppleBinaryEnvironment
import com.malinskiy.marathon.apple.cmd.*
import com.malinskiy.marathon.apple.cmd.local.KotlinProcessHost
import com.malinskiy.marathon.apple.model.*
import com.malinskiy.marathon.apple.xctestrun.TestRootFactory
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
fun main(args: Array<String>) = runBlocking {
    File(args[0]).readLines().forEach { line ->
        val input = JsonParser.parseString(line).asJsonObject
        val root = File(args[1], input.s("id"))
        val sdk = Sdk.valueOf(input.s("sdk"))
        val commands = mutableListOf<Any>()
        val events = mutableListOf<Any>()
        val pushes = mutableListOf<Any>()
        val temporaries = mutableListOf<File>()
        var xml: String? = null
        var lipo = 0
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
                    else -> emptyList()
                }
                val mode = input.s("mode")
                if (command.first() == "test" && mode == "exists-throw") throw java.io.IOException("exists io")
                val exit = when {
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
                "getUdid", "getSerialNumber" -> udid
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
                    val push = linkedMapOf("kind" to "shared", "src" to (a[0] as File).relativeTo(root).path, "dst" to a[1])
                    pushes.add(push); events.add(push)
                    !(input.s("mode") == "push-app-false" && pushes.size == 1 || input.s("mode") == "push-test-false" && pushes.size == 2)
                }
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
            val bundle = AppleTestBundle(File(root,input.s("app")), if(input.b("reuse")) File(root,input.s("runner")) else null, File(root,input.s("test")), sdk)
            if(input.b("noApp")) AppleTestBundle::class.java.getDeclaredField("application").apply { isAccessible = true }.set(bundle,null)
            fun env(key:String):Map<String,String> {
                val result = input.getAsJsonArray(key)?.associateTo(linkedMapOf()) { val a=it.asJsonArray; a[0].asString to a[1].asString } ?: linkedMapOf()
                if (input.s("surrogateField") == key) result["K"] = "\ud800"
                return result
            }
            val result = XcresultConfiguration(attachments=AttachmentsConfiguration(Lifetime.valueOf(input.s("systemLifetime","DELETE_ON_SUCCESS")), Lifetime.valueOf(input.s("userLifetime","KEEP_ALWAYS"))), preferredScreenCaptureFormat=input.get("capture")?.asString?.let { ScreenCaptureFormat.valueOf(it) })
            if(input.s("mode") == "writer-fail") {
                System.setSecurityManager(object: SecurityManager() {
                    var writes = 0
                    override fun checkPermission(permission: java.security.Permission) = Unit
                    override fun checkWrite(file: String) {
                        if(file.endsWith("xctestrun")) {
                            if(writes++ == 0) temporaries.add(File(file))
                            else throw SecurityException("writer denied")
                        }
                    }
                })
            }
            TestRootFactory(fake,XctestrunEnvConfiguration(env("appEnv"),env("testEnv")),result,input.b("skip")).generate(TestType.valueOf(input.s("type")),bundle,input.b("parser"))
        } catch(e:Throwable) {
            error = linkedMapOf("class" to e.javaClass.name, "message" to e.message?.replace(root.absolutePath,"<bundle>"), "reason" to (e as? com.malinskiy.marathon.apple.logparser.parser.DeviceFailureException)?.reason?.toString())
        }
        if(input.s("mode") == "writer-fail") System.setSecurityManager(null)
        println(gson.toJson(linkedMapOf("input" to input, "events" to events, "commands" to commands, "pushes" to pushes, "xml" to xml, "error" to error, "cleaned" to temporaries.all { !it.exists() })))
        temporaries.forEach { it.deleteRecursively() }
    }
}
