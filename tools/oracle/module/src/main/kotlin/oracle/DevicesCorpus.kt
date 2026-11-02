package oracle

import com.fasterxml.jackson.databind.ObjectMapper
import com.fasterxml.jackson.dataformat.yaml.YAMLFactory
import com.fasterxml.jackson.dataformat.yaml.YAMLGenerator
import com.fasterxml.jackson.module.kotlin.KotlinFeature
import com.fasterxml.jackson.module.kotlin.KotlinModule
import com.fasterxml.jackson.module.kotlin.readValue
import com.malinskiy.marathon.apple.configuration.AppleTarget
import com.malinskiy.marathon.apple.configuration.Marathondevices
import com.malinskiy.marathon.apple.configuration.Transport
import com.malinskiy.marathon.config.vendor.apple.SshAuthentication
import java.io.File

/**
 * WP 2.3b P2 (Marathondevices) corpus: each row binds through the P2 mapper
 * of `IosVendor.kt`/`MacosVendor.kt` (KotlinModule only; no
 * `SerializeModule`, no JavaTimeModule), exactly as `StaticTest` builds it.
 * `File` values render by their bound spelling (`File.path`), not
 * `absolutePath`, so the F4 relative-key behaviour is pinned (the process
 * working directory of the oracle must not leak into the table).
 */
object DevicesCorpus {
    fun generate(outDir: File) {
        val mapper = ObjectMapper(YAMLFactory().disable(YAMLGenerator.Feature.USE_NATIVE_TYPE_ID))
            .registerModule(
                KotlinModule.Builder()
                    .withReflectionCacheSize(512)
                    .configure(KotlinFeature.NullToEmptyCollection, false)
                    .configure(KotlinFeature.NullToEmptyMap, false)
                    .configure(KotlinFeature.NullIsSameAsDefault, false)
                    .configure(KotlinFeature.SingletonSupport, true)
                    .configure(KotlinFeature.StrictNullChecks, false)
                    .build()
            )
        val table = JsonlTable(
            "yaml-devices-corpus",
            "WP 2.3b: Marathondevices documents through the production P2 mapper; File values render by bound spelling",
            linkedMapOf(
                "id" to "row name",
                "input" to "the Marathondevices YAML document",
                "result" to "{ok: canonical Marathondevices} or {error: {class, message, causes}} or {ok: null} for a null document",
            ),
        )
        for ((id, input) in cases) {
            val result = try {
                val bound: Marathondevices? = mapper.readValue(input.trimIndent())
                mapOf("ok" to bound?.let { render(it) })
            } catch (t: Throwable) {
                mapOf("error" to errorJson(t))
            }
            table.add(linkedMapOf("id" to id, "input" to input.trimIndent(), "result" to result))
        }
        table.writeTo(outDir)
    }

    private fun render(devices: Marathondevices): Any = mapOf(
        "workers" to devices.workers?.map { worker ->
            if (worker == null) null else mapOf(
                "transport" to renderTransport(worker.transport),
                "devices" to worker.devices?.map { if (it == null) null else renderTarget(it) },
            )
        }
    )

    private fun renderTransport(transport: Transport?): Any? = when (transport) {
        null -> null
        Transport.Local -> mapOf("type" to "local")
        is Transport.Ssh -> mapOf(
            "type" to "ssh",
            "addr" to transport.addr,
            "port" to transport.port,
            "checkReachability" to transport.checkReachability,
            "authentication" to renderAuth(transport.authentication),
        )
    }

    private fun renderAuth(auth: SshAuthentication?): Any? = when (auth) {
        null -> null
        is SshAuthentication.PasswordAuthentication -> mapOf(
            "type" to "password", "username" to auth.username, "password" to auth.password,
        )
        is SshAuthentication.PublicKeyAuthentication -> mapOf(
            "type" to "publicKey", "username" to auth.username, "key" to auth.key?.path,
        )
    }

    private fun renderTarget(target: AppleTarget): Any = when (target) {
        is AppleTarget.Simulator -> mapOf("type" to "simulator", "udid" to target.udid)
        is AppleTarget.Physical -> mapOf("type" to "physical", "udid" to target.udid)
        is AppleTarget.SimulatorProfile -> mapOf(
            "type" to "simulatorProfile",
            "deviceType" to field(target, "deviceTypeId"),
            "runtime" to field(target, "runtimeId"),
            "newNamePrefix" to target.newNamePrefix,
            "fqDeviceType" to target.fullyQualifiedDeviceTypeId,
            "fqRuntime" to target.fullyQualifiedRuntimeId,
        )
        AppleTarget.Host -> mapOf("type" to "host")
    }

    private fun field(target: Any, name: String): Any? {
        val f = target.javaClass.getDeclaredField(name)
        f.isAccessible = true
        return f.get(target)
    }

    private const val SSH = "transport: {type: ssh, addr: node}\n    devices: [{type: host}]"

    private val cases: List<Pair<String, String>> = listOf(
        "sample-ok" to """
            workers:
              - transport: {type: local}
                devices: [{type: simulator, udid: XXX}, {type: physical, udid: YYY}]
              - transport: {type: ssh, addr: node-1, port: 44, authentication: {type: password, username: u, password: p}}
                devices: [{type: simulatorProfile, deviceType: "com.apple.CoreSimulator.SimDeviceType.iPhone-X", runtime: "com.apple.CoreSimulator.SimRuntime.iOS-16-2", newNamePrefix: prefix}]
            """,
        "workers-missing" to "{}",
        "workers-null" to "workers:",
        "workers-scalar" to "workers: hello",
        "workers-empty" to "workers: []",
        "workers-null-element" to "workers: [null]",
        "worker-transport-missing" to "workers: [{devices: [{type: host}]}]",
        "worker-transport-null" to "workers: [{transport: null, devices: [{type: host}]}]",
        "worker-devices-missing" to "workers: [{transport: {type: local}}]",
        "worker-devices-null" to """
            workers:
              - transport: {type: ssh, addr: node}
                devices:
            """,
        "worker-devices-empty" to "workers: [{transport: {type: local}, devices: []}]",
        "worker-devices-null-element" to "workers: [{transport: {type: local}, devices: [null]}]",
        "worker-unknown-key" to "workers: [{transport: {type: local}, devices: [], bogus: 1}]",
        "top-unknown-key" to "workers: []\nbogus: 1",
        "ssh-addr-missing" to "workers: [{transport: {type: ssh}, devices: []}]",
        "ssh-addr-null" to "workers: [{transport: {type: ssh, addr: null}, devices: []}]",
        "ssh-addr-number" to "workers: [{transport: {type: ssh, addr: 123}, devices: []}]",
        "ssh-port-missing" to "workers:\n  - $SSH",
        "ssh-port-null" to "workers: [{transport: {type: ssh, addr: node, port: null}, devices: []}]",
        "ssh-port-string" to "workers: [{transport: {type: ssh, addr: node, port: \"44\"}, devices: []}]",
        "ssh-port-float" to "workers: [{transport: {type: ssh, addr: node, port: 44.5}, devices: []}]",
        "ssh-check-null" to "workers: [{transport: {type: ssh, addr: node, checkReachability: null}, devices: []}]",
        "ssh-auth-null" to "workers: [{transport: {type: ssh, addr: node, authentication: null}, devices: []}]",
        "ssh-unknown-key" to "workers: [{transport: {type: ssh, addr: node, bogus: 1}, devices: []}]",
        "local-extra-key" to "workers: [{transport: {type: local, bogus: 1}, devices: []}]",
        "host-extra-key" to "workers: [{transport: {type: local}, devices: [{type: host, bogus: 1}]}]",
        "transport-type-unknown" to "workers: [{transport: {type: carrierPigeon}, devices: []}]",
        "transport-type-missing" to "workers: [{transport: {addr: node}, devices: []}]",
        "transport-type-late" to "workers: [{transport: {addr: node, type: ssh}, devices: []}]",
        "transport-type-null" to "workers: [{transport: {type: null, addr: node}, devices: []}]",
        "device-type-unknown" to "workers: [{transport: {type: local}, devices: [{type: tablet}]}]",
        "device-type-late" to "workers: [{transport: {type: local}, devices: [{udid: X, type: simulator}]}]",
        "sim-udid-missing" to "workers: [{transport: {type: local}, devices: [{type: simulator}]}]",
        "sim-udid-null" to "workers: [{transport: {type: local}, devices: [{type: simulator, udid: null}]}]",
        "sim-udid-number" to "workers: [{transport: {type: local}, devices: [{type: simulator, udid: 123}]}]",
        "device-unknown-key" to "workers: [{transport: {type: local}, devices: [{type: simulator, udid: X, bogus: 1}]}]",
        "profile-devicetype-missing" to "workers: [{transport: {type: local}, devices: [{type: simulatorProfile}]}]",
        "profile-devicetype-null" to "workers: [{transport: {type: local}, devices: [{type: simulatorProfile, deviceType: null}]}]",
        "profile-runtime-missing" to "workers: [{transport: {type: local}, devices: [{type: simulatorProfile, deviceType: \"iPhone X\"}]}]",
        "profile-runtime-null" to "workers: [{transport: {type: local}, devices: [{type: simulatorProfile, deviceType: \"iPhone X\", runtime: null}]}]",
        "profile-prefix-null" to "workers: [{transport: {type: local}, devices: [{type: simulatorProfile, deviceType: \"iPhone X\", newNamePrefix: null}]}]",
        "key-relative" to "workers: [{transport: {type: ssh, addr: node, authentication: {type: publicKey, username: u, key: \"relative/dir/key.pem\"}}, devices: []}]",
        "key-blank" to "workers: [{transport: {type: ssh, addr: node, authentication: {type: publicKey, username: u, key: \"  \"}}, devices: []}]",
        "key-empty" to "workers: [{transport: {type: ssh, addr: node, authentication: {type: publicKey, username: u, key: \"\"}}, devices: []}]",
        "key-number" to "workers: [{transport: {type: ssh, addr: node, authentication: {type: publicKey, username: u, key: 123}}, devices: []}]",
        "key-null" to "workers: [{transport: {type: ssh, addr: node, authentication: {type: publicKey, username: u, key: null}}, devices: []}]",
        "key-missing" to "workers: [{transport: {type: ssh, addr: node, authentication: {type: publicKey, username: u}}, devices: []}]",
        "key-padded" to "workers: [{transport: {type: ssh, addr: node, authentication: {type: publicKey, username: u, key: \"  /abs/key  \"}}, devices: []}]",
        "auth-username-missing" to "workers: [{transport: {type: ssh, addr: node, authentication: {type: password, password: p}}, devices: []}]",
        "auth-unknown-key" to "workers: [{transport: {type: ssh, addr: node, authentication: {type: password, username: u, password: p, bogus: 1}}, devices: []}]",
        "doc-empty" to "",
        "doc-null" to "null",
        "doc-scalar" to "hello",
        "doc-list" to "- a",
        "doc-syntax-error" to "workers: [",
        "doc-syntax-lex" to "*undefined-alias",
    )
}
