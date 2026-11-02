package oracle

import com.fasterxml.jackson.annotation.JsonInclude
import com.fasterxml.jackson.databind.ObjectMapper
import com.fasterxml.jackson.databind.SerializationFeature
import com.fasterxml.jackson.dataformat.yaml.YAMLFactory
import com.fasterxml.jackson.dataformat.yaml.YAMLGenerator
import com.fasterxml.jackson.datatype.jsr310.JavaTimeModule
import com.fasterxml.jackson.module.kotlin.KotlinFeature
import com.fasterxml.jackson.module.kotlin.KotlinModule
import com.malinskiy.marathon.config.Configuration
import com.malinskiy.marathon.config.LogicalConfigurationValidator
import com.malinskiy.marathon.config.exceptions.ConfigurationException
import com.malinskiy.marathon.config.serialization.ConfigurationFactory
import com.malinskiy.marathon.config.serialization.time.InstantTimeProvider
import com.malinskiy.marathon.config.serialization.yaml.SerializeModule
import org.apache.commons.text.StringSubstitutor
import org.apache.commons.text.lookup.StringLookupFactory
import java.io.File
import java.nio.file.Files
import java.nio.file.LinkOption
import java.security.MessageDigest
import java.time.Instant
import java.util.TreeMap
import kotlin.system.exitProcess

private const val REEXEC_PROPERTY = "oracle.reexec"

/**
 * `config-dump <Marathonfile> [--env-file F] [--now <ISO instant>]`: parses a Marathonfile the way the CLI does, runs the
 * logical validation of the run path, and prints the outcome as one JSON document (docs/design/config-dump-format.md).
 */
fun configDump(args: List<String>) {
    var file: String? = null
    var envFile: String? = null
    var now: Instant? = null
    val childArgs = mutableListOf<String>()
    var i = 0
    while (i < args.size) {
        when (val a = args[i]) {
            "--env-file" -> envFile = args.getOrNull(++i) ?: usage("--env-file needs a value")
            "--now" -> {
                val v = args.getOrNull(++i) ?: usage("--now needs a value")
                now = Instant.parse(v)
                childArgs += listOf(a, v)
            }
            else -> {
                if (a.startsWith("--") || file != null) usage("unexpected argument $a")
                file = a
                childArgs += a
            }
        }
        i++
    }
    if (file == null) usage("missing Marathonfile")

    if (envFile != null && System.getProperty(REEXEC_PROPERTY) == null) {
        exitProcess(reexecWithEnvironment(readEnvFile(File(envFile)), childArgs))
    }

    print(Json.pretty(dumpConfig(File(file), now)))
}

private fun usage(message: String): Nothing {
    System.err.println("config-dump: $message")
    System.err.println("usage: config-dump <Marathonfile> [--env-file F] [--now <ISO instant>]")
    exitProcess(2)
}

/** KEY=VALUE lines; blank lines and lines starting with # are skipped; the value is everything after the first `=`. */
fun readEnvFile(file: File): Map<String, String> = file.readLines()
    .filter { it.isNotBlank() && !it.startsWith("#") }
    .associate {
        val eq = it.indexOf('=')
        require(eq > 0) { "not a KEY=VALUE line in ${file.path}: $it" }
        it.substring(0, eq) to it.substring(eq + 1)
    }

/** Runs this JVM's main class again with exactly [env] as the environment, so that System.getenv sees nothing else. */
private fun reexecWithEnvironment(env: Map<String, String>, args: List<String>): Int {
    val java = ProcessHandle.current().info().command().orElseThrow { IllegalStateException("cannot find the java executable") }
    val command = listOf(
        java, "-D$REEXEC_PROPERTY=1", "-cp", System.getProperty("java.class.path"), "oracle.MainKt", "config-dump"
    ) + args
    val pb = ProcessBuilder(command).inheritIO()
    pb.environment().clear()
    pb.environment().putAll(env)
    return pb.start().waitFor()
}

class FixedTimeProvider(private val now: Instant) : InstantTimeProvider {
    override fun referenceTime(): Instant = now
}

/**
 * The mapper of [ConfigurationFactory]'s default constructor argument, with the time provider replaced. Kept in step with
 * marathon by [checkMapperMatchesMarathon].
 */
fun marathonfileMapper(timeProvider: InstantTimeProvider, marathonfileDir: File): ObjectMapper = ObjectMapper(
    YAMLFactory()
        .disable(YAMLGenerator.Feature.USE_NATIVE_TYPE_ID)
).apply {
    setSerializationInclusion(JsonInclude.Include.NON_NULL)
    registerModule(SerializeModule(timeProvider, marathonfileDir))
    registerModule(
        KotlinModule.Builder()
            .withReflectionCacheSize(512)
            .configure(KotlinFeature.NullToEmptyCollection, false)
            .configure(KotlinFeature.NullToEmptyMap, false)
            .configure(KotlinFeature.NullIsSameAsDefault, false)
            .configure(KotlinFeature.SingletonSupport, true)
            .configure(KotlinFeature.StrictNullChecks, false)
            .build()
    )
    registerModule(JavaTimeModule())
    configure(SerializationFeature.WRITE_DATES_AS_TIMESTAMPS, false)
    configure(SerializationFeature.WRITE_DURATIONS_AS_TIMESTAMPS, false)
}

/** Fails when [marathonfileMapper] no longer configures the same features and modules as marathon's own default mapper. */
fun checkMapperMatchesMarathon(dir: File) {
    val field = ConfigurationFactory::class.java.getDeclaredField("mapper").apply { isAccessible = true }
    val theirs = field.get(ConfigurationFactory(marathonfileDir = dir)) as ObjectMapper
    val ours = marathonfileMapper(FixedTimeProvider(Instant.EPOCH), dir)
    fun fingerprint(m: ObjectMapper) = listOf(
        m.registeredModuleIds.map { it.toString() }.sorted(),
        m.deserializationConfig.deserializationFeatures,
        m.serializationConfig.serializationFeatures,
        m.serializationConfig.defaultPropertyInclusion.toString(),
        (m.factory as YAMLFactory).formatGeneratorFeatures,
        (m.factory as YAMLFactory).formatParserFeatures,
        com.fasterxml.jackson.databind.MapperFeature.values().filter { m.isEnabled(it) }.map { it.name },
    )
    check(fingerprint(theirs) == fingerprint(ours)) {
        "the oracle's Marathonfile mapper differs from marathon's: ${fingerprint(theirs)} vs ${fingerprint(ours)}"
    }
}

class ParseOutcome(val stage: String, val error: Throwable?, val config: Configuration?)

/**
 * Stage (a) as the CLI runs it (ApplicationView.execute): the isFile check, then [ConfigurationFactory.parse] with the
 * Marathonfile's canonical parent as `marathonfileDir` and both tracking flags true. Then stage (b), the
 * [LogicalConfigurationValidator] that `Marathon.runAsync` calls first.
 *
 * [now] replaces `InstantTimeProviderImpl` through the factory's mapper argument; [substitutor] replaces the environment
 * lookup (used by the YAML corpus only).
 */
fun parseAndValidate(file: File, now: Instant?, substitutor: StringSubstitutor? = null): ParseOutcome {
    val config = try {
        if (!file.isFile) throw ConfigurationException("No config ${file.absolutePath} present")
        val dir = file.canonicalFile.parentFile
        val mapper = now?.let {
            checkMapperMatchesMarathon(dir)
            marathonfileMapper(FixedTimeProvider(it), dir)
        }
        val factory = when {
            mapper == null && substitutor == null -> ConfigurationFactory(
                marathonfileDir = dir, analyticsTracking = true, bugsnagReporting = true,
            )
            else -> ConfigurationFactory(
                marathonfileDir = dir,
                mapper = mapper ?: marathonfileMapper(com.malinskiy.marathon.config.serialization.time.InstantTimeProviderImpl(), dir),
                environmentVariableSubstitutor = substitutor
                    ?: StringSubstitutor(StringLookupFactory.INSTANCE.environmentVariableStringLookup()),
                analyticsTracking = true,
                bugsnagReporting = true,
            )
        }
        factory.parse(file)
    } catch (t: Throwable) {
        return ParseOutcome("parse", t, null)
    }
    return try {
        LogicalConfigurationValidator().validate(config)
        ParseOutcome("ok", null, config)
    } catch (t: Throwable) {
        ParseOutcome("logical", t, config)
    }
}

fun dumpConfig(file: File, now: Instant?): Map<String, Any?> {
    val dir = file.absoluteFile.parentFile.let { if (it.exists()) it.canonicalFile else it }
    val before = snapshot(dir)
    val outcome = parseAndValidate(file, now)
    val after = snapshot(dir)
    return linkedMapOf(
        "stage" to outcome.stage,
        "error" to outcome.error?.let { errorJson(it) },
        "config" to outcome.config?.let { Canonical.render(it) },
        "fsEffects" to fsEffects(before, after),
    )
}

data class Entry(val kind: String, val size: Long?, val sha256: String?)

/** Every path under [root], relative to it, without following symbolic links. */
fun snapshot(root: File): Map<String, Entry> {
    val out = TreeMap<String, Entry>()
    if (!root.isDirectory) return out
    val rootPath = root.toPath()
    Files.walk(rootPath).use { stream ->
        stream.forEach { p ->
            if (p == rootPath) return@forEach
            val rel = rootPath.relativize(p).joinToString("/")
            out[rel] = when {
                Files.isSymbolicLink(p) -> Entry("symlink", null, Files.readSymbolicLink(p).toString())
                Files.isDirectory(p, LinkOption.NOFOLLOW_LINKS) -> Entry("dir", null, null)
                else -> Entry("file", Files.size(p), sha256(Files.readAllBytes(p)))
            }
        }
    }
    return out
}

fun sha256(bytes: ByteArray): String = MessageDigest.getInstance("SHA-256").digest(bytes).joinToString("") { "%02x".format(it) }

fun fsEffects(before: Map<String, Entry>, after: Map<String, Entry>): List<Map<String, Any?>> =
    (before.keys + after.keys).toSortedSet().mapNotNull { path ->
        val b = before[path]
        val a = after[path]
        val op = when {
            b == null -> "created"
            a == null -> "deleted"
            a != b -> "modified"
            else -> return@mapNotNull null
        }
        val e = a ?: b!!
        linkedMapOf("op" to op, "path" to path, "kind" to e.kind, "size" to e.size, "sha256" to e.sha256)
    }
