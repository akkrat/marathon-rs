package oracle

import com.malinskiy.marathon.config.AnalyticsConfiguration
import com.malinskiy.marathon.config.CIConfiguration
import com.malinskiy.marathon.config.Configuration
import com.malinskiy.marathon.config.FilteringConfiguration
import com.malinskiy.marathon.config.OutputConfiguration
import com.malinskiy.marathon.config.TestFilterConfiguration
import com.malinskiy.marathon.config.analytics.Defaults
import com.malinskiy.marathon.config.strategy.BatchingStrategyConfiguration
import com.malinskiy.marathon.config.strategy.ExecutionStrategyConfiguration
import com.malinskiy.marathon.config.strategy.FlakinessStrategyConfiguration
import com.malinskiy.marathon.config.strategy.PoolingStrategyConfiguration
import com.malinskiy.marathon.config.strategy.RetryStrategyConfiguration
import com.malinskiy.marathon.config.strategy.ShardingStrategyConfiguration
import com.malinskiy.marathon.config.strategy.SortingStrategyConfiguration
import java.io.File
import java.time.Instant

/** WP 2.2 schema witnesses: bind one YAML document to one marathon config type through the P1 mapper. */
object SchemaCorpus {
    fun generate(out: File) {
        val table = JsonlTable(
            out.nameWithoutExtension,
            "WP 2.2 schema corpus: section-level binding through marathon's Marathonfile mapper (profile P1, time provider fixed at 2026-10-12T00:00:00Z, marathonfile dir /). result is {ok: canonical} | {nullDocument: true} | {error: {class, message, causes}}",
            linkedMapOf(
                "id" to "row id",
                "target" to "marathon config type bound by the row",
                "input" to "the YAML text",
                "result" to "{ok: canonical value} | {nullDocument: true} | {error: {class, message, causes}}",
            ),
        )
        for ((target, cls) in targets) {
            for ((suffix, input) in rowsByTarget.getValue(target)) {
                val result = bind(input, cls)
                table.add(linkedMapOf("id" to "$target-$suffix", "target" to target, "input" to input, "result" to result))
            }
        }
        // jtime checked Instant+Duration witnesses (WP 2.2 S7): pure java.time, same fixed now.
        val now = Instant.parse("2026-10-12T00:00:00Z")
        for ((suffix, duration) in listOf(
            "minus-hour" to "-PT1H", "minus-30d" to "-P30D", "plus-nanos" to "PT0.000000001S",
            "huge" to "PT2562047788015215H", "max-plus" to "P106751991167300D",
        )) {
            val result = try {
                mapOf("ok" to now.plus(java.time.Duration.parse(duration)).toString())
            } catch (t: Throwable) {
                mapOf("error" to errorJson(t))
            }
            table.add(linkedMapOf("id" to "InstantPlus-$suffix", "target" to "InstantPlus", "input" to duration, "result" to result))
        }
        // JsonNode.toString rendering witnesses for the probability message (S3).
        val mapper = mapper()
        for ((suffix, input) in listOf(
            "string" to "x: \"bogus\"", "int" to "x: 42", "float" to "x: 1.5", "bool" to "x: true",
            "null" to "x: ~", "array" to "x: [1, \"a\"]", "object" to "x: {a: 1, b: \"c\"}",
            "quoted" to "x: \"a\\\"b\"", "big" to "x: 9223372036854775808",
            "control" to "x: \"a\\u001fb\\u001bc\\u0001\"", "control-key" to "x: {\"k\\u001f\": 1}",
        )) {
            val node = mapper.readTree(input).get("x")
            table.add(
                linkedMapOf(
                    "id" to "NodeString-$suffix", "target" to "NodeString", "input" to input,
                    "result" to mapOf("ok" to node.toString()),
                ),
            )
        }
        table.writeTo(out.parentFile.also { it.mkdirs() }).renameTo(out)
    }

    private fun mapper() = marathonfileMapper(FixedTimeProvider(Instant.parse("2026-10-12T00:00:00Z")), File("/"))

    private fun bind(input: String, cls: Class<*>): Map<String, Any?> = try {
        val v = mapper().readValue(input, cls)
        if (v == null) mapOf("nullDocument" to true) else mapOf("ok" to Canonical.render(v))
    } catch (t: Throwable) {
        mapOf("error" to errorJson(t))
    }

    private val targets: List<Pair<String, Class<*>>> = listOf(
        "Configuration" to Configuration::class.java,
        "AnalyticsConfiguration" to AnalyticsConfiguration::class.java,
        "RetentionPolicy" to AnalyticsConfiguration.InfluxDbConfiguration.RetentionPolicyConfiguration::class.java,
        "RetentionPolicy2" to AnalyticsConfiguration.InfluxDb2Configuration.RetentionPolicyConfiguration::class.java,
        "Defaults" to Defaults::class.java,
        "OutputConfiguration" to OutputConfiguration::class.java,
        "ExecutionStrategy" to ExecutionStrategyConfiguration::class.java,
        "CIConfiguration" to CIConfiguration::class.java,
        "FilteringConfiguration" to FilteringConfiguration::class.java,
        "TestFilterConfiguration" to TestFilterConfiguration::class.java,
        "Pooling" to PoolingStrategyConfiguration::class.java,
        "Sharding" to ShardingStrategyConfiguration::class.java,
        "Sorting" to SortingStrategyConfiguration::class.java,
        "Batching" to BatchingStrategyConfiguration::class.java,
        "Flakiness" to FlakinessStrategyConfiguration::class.java,
        "Retry" to RetryStrategyConfiguration::class.java,
    )

    private val influx = "type: influxdb\nurl: u\nuser: u\npassword: p\ndbName: d"
    private val graphite = "type: graphite\nhost: h"
    private val rp = "name: n\nduration: 1d\nshardDuration: 1h\nreplicationFactor: 1\nisDefault: true"

    private val rowsByTarget: Map<String, List<Pair<String, String>>> = mapOf(
        "Configuration" to listOf(
            "minimal" to "name: n\noutputDir: ./out",
            "missing-name" to "outputDir: ./out",
            "missing-output" to "name: n",
            "null-name" to "name: ~\noutputDir: ./out",
            "null-output" to "name: n\noutputDir: ~",
            "null-bool" to "name: n\noutputDir: ./out\ndebug: ~",
            "null-long" to "name: n\noutputDir: ./out\ntestBatchTimeoutMillis: ~",
            "null-section" to "name: n\noutputDir: ./out\nfilteringConfiguration: ~",
            "null-analytics" to "name: n\noutputDir: ./out\nanalyticsConfiguration: ~",
            "null-list" to "name: n\noutputDir: ./out\nincludeSerialRegexes: ~",
            "null-list-elem" to "name: n\noutputDir: ./out\nincludeSerialRegexes: [~]",
            "unknown" to "name: n\noutputDir: ./out\nzzz: 1",
            "screen" to "name: n\noutputDir: ./out\nscreenRecordingPolicy: ON_ANY",
            "screen-null" to "name: n\noutputDir: ./out\nscreenRecordingPolicy: ~",
            "screen-bad" to "name: n\noutputDir: ./out\nscreenRecordingPolicy: bogus",
            "ci-auto" to "name: n\noutputDir: ./out\nciConfiguration: {type: auto}",
            "tracking" to "name: n\noutputDir: ./out\nanalyticsTracking: false\nbugsnagReporting: false",
            "empty-doc" to "{}",
            "null-vendor" to "name: n\noutputDir: ./out\nvendorConfiguration: ~",
            "null-output-config" to "name: n\noutputDir: ./out\noutputConfiguration: ~",
            "bad-serial-regex" to "name: n\noutputDir: ./out\nincludeSerialRegexes: ['[abc']",
        ),
        "AnalyticsConfiguration" to listOf(
            "disabled" to "{type: disabled}",
            "disabled-extra" to "{type: disabled, zzz: 1}",
            "disabled-scalar" to "disabled",
            "influx-min" to influx,
            "influx-readonly" to "$influx\nreadOnly: true",
            "influx-null-url" to "type: influxdb\nurl: ~\nuser: u\npassword: p\ndbName: d",
            "influx-num-url" to "type: influxdb\nurl: 42\nuser: u\npassword: p\ndbName: d",
            "influx-obj-url" to "type: influxdb\nurl: {a: 1}\nuser: u\npassword: p\ndbName: d",
            "influx-missing-user" to "type: influxdb\nurl: u\npassword: p\ndbName: d",
            "influx-rp-null" to "$influx\nretentionPolicyConfiguration: ~",
            "influx-defaults-null" to "$influx\ndefaults: ~",
            "influx-defaults-unknown" to "$influx\ndefaults: {zzz: 1}",
            "influx-defaults" to "$influx\ndefaults: {successRate: 0.1, duration: PT40S, observedDurationMinTests: 20}",
            "influx-observed-empty" to "$influx\ndefaults: {observedDurationMinTests: \"\"}",
            "influx-observed-blank" to "$influx\ndefaults: {observedDurationMinTests: \" \"}",
            "influx-observed-nullstr" to "$influx\ndefaults: {observedDurationMinTests: \"null\"}",
            "influx2-observed-empty" to "type: influxdb2\nurl: u\ntoken: t\norganization: o\nbucket: b\ndefaults: {observedDurationMinTests: \"\"}",
            "influx2-observed-blank" to "type: influxdb2\nurl: u\ntoken: t\norganization: o\nbucket: b\ndefaults: {observedDurationMinTests: \" \"}",
            "influx2-observed-nullstr" to "type: influxdb2\nurl: u\ntoken: t\norganization: o\nbucket: b\ndefaults: {observedDurationMinTests: \"null\"}",
            "graphite-observed-empty" to "$graphite\ndefaults: {observedDurationMinTests: \"\"}",
            "graphite-observed-blank" to "$graphite\ndefaults: {observedDurationMinTests: \" \"}",
            "graphite-observed-nullstr" to "$graphite\ndefaults: {observedDurationMinTests: \"null\"}",
            "influx-rp-partial" to "$influx\nretentionPolicyConfiguration: {name: n}",
            "influx-scalar" to "x",
            "influx2-min" to "type: influxdb2\nurl: u\ntoken: t\norganization: o\nbucket: b",
            "influx2-readonly" to "type: influxdb2\nurl: u\ntoken: t\norganization: o\nbucket: b\nreadOnly: true",
            "influx2-rp" to "type: influxdb2\nurl: u\ntoken: t\norganization: o\nbucket: b\nretentionPolicyConfiguration: {everySeconds: 10, shardGroupDurationSeconds: 20}",
            "influx2-rp-null" to "type: influxdb2\nurl: u\ntoken: t\norganization: o\nbucket: b\nretentionPolicyConfiguration: ~",
            "influx2-null-url" to "type: influxdb2\nurl: ~\ntoken: t\norganization: o\nbucket: b",
            "influx2-defaults-null" to "type: influxdb2\nurl: u\ntoken: t\norganization: o\nbucket: b\ndefaults: ~",
            "graphite-min" to graphite,
            "graphite-full" to "$graphite\nport: 2003\nprefix: pre",
            "graphite-port-str" to "$graphite\nport: \"2003\"",
            "graphite-port-float" to "$graphite\nport: 2003.0",
            "graphite-port-plus" to "$graphite\nport: \"+2003\"",
            "graphite-port-space" to "$graphite\nport: \" 2003\"",
            "graphite-port-neg" to "$graphite\nport: \"-1\"",
            "graphite-port-null" to "$graphite\nport: ~",
            "graphite-port-bool" to "$graphite\nport: true",
            "graphite-port-arabic" to "$graphite\nport: \"٢٠٠٣\"",
            "graphite-prefix-null" to "$graphite\nprefix: ~",
            "graphite-prefix-empty" to "$graphite\nprefix: \"\"",
            "graphite-prefix-obj" to "$graphite\nprefix: {a: 1}",
            "graphite-missing-host" to "{type: graphite}",
            "graphite-host-null" to "type: graphite\nhost: ~",
            "graphite-defaults-null" to "$graphite\ndefaults: ~",
            "graphite-defaults-unknown" to "$graphite\ndefaults: {zzz: 1}",
            "influx-rp-scalar" to "$influx\nretentionPolicyConfiguration: x",
            "influx-bool-url" to "type: influxdb\nurl: true\nuser: u\npassword: p\ndbName: d",
            "graphite-defaults-scalar" to "$graphite\ndefaults: x",
            "graphite-port-obj" to "$graphite\nport: {a: 1}",
            "graphite-port-decstr" to "$graphite\nport: \"2003.5\"",
            "unknown-type" to "{type: bogus}",
            "missing-type" to "{url: u}",
        ),
        "RetentionPolicy" to listOf(
            "full" to rp,
            "missing-name" to "duration: 1d\nshardDuration: 1h\nreplicationFactor: 1\nisDefault: true",
            "null-name" to "name: ~\nduration: 1d\nshardDuration: 1h\nreplicationFactor: 1\nisDefault: true",
            "null-factor" to "name: n\nduration: 1d\nshardDuration: 1h\nreplicationFactor: ~\nisDefault: true",
            "float-factor" to "name: n\nduration: 1d\nshardDuration: 1h\nreplicationFactor: 1.9\nisDefault: true",
            "bool-factor" to "name: n\nduration: 1d\nshardDuration: 1h\nreplicationFactor: true\nisDefault: true",
            "int-default" to "name: n\nduration: 1d\nshardDuration: 1h\nreplicationFactor: 1\nisDefault: 1",
            "str-default" to "name: n\nduration: 1d\nshardDuration: 1h\nreplicationFactor: 1\nisDefault: \"true\"",
            "unknown" to "$rp\nzzz: 1",
            "scalar" to "x",
            "null-doc" to "~",
            "empty" to "{}",
        ),
        "RetentionPolicy2" to listOf(
            "full" to "everySeconds: 10\nshardGroupDurationSeconds: 20",
            "missing" to "everySeconds: 10",
            "null-every" to "everySeconds: ~\nshardGroupDurationSeconds: 20",
            "unknown" to "everySeconds: 10\nshardGroupDurationSeconds: 20\nzzz: 1",
        ),
        "Defaults" to listOf(
            "empty" to "{}",
            "full" to "successRate: 0.25\nduration: PT40S\nobservedDurationMinTests: 3",
            "duration-number" to "duration: 40",
            "duration-float" to "duration: 40.5",
            "null-rate" to "successRate: ~",
            "null-duration" to "duration: ~",
            "null-observed" to "observedDurationMinTests: ~",
            "observed-empty" to "observedDurationMinTests: \"\"",
            "observed-blank" to "observedDurationMinTests: \" \"",
            "observed-nullstr" to "observedDurationMinTests: \"null\"",
            "observed-zero" to "observedDurationMinTests: 0",
            "unknown" to "zzz: 1",
            "scalar" to "x",
        ),
        "OutputConfiguration" to listOf(
            "empty" to "{}",
            "full" to "maxPath: 10\nmaxFilename: 20",
            "null-path" to "maxPath: ~",
        ),
        "ExecutionStrategy" to listOf(
            "empty" to "{}",
            "all" to "mode: ALL_SUCCESS\nfast: false",
            "null-mode" to "mode: ~",
            "bad-mode" to "mode: bogus",
            "null-fast" to "fast: ~",
        ),
        "CIConfiguration" to listOf(
            "auto" to "{type: auto}",
            "none" to "{type: none}",
            "teamcity" to "{type: teamcity}",
            "auto-extra" to "{type: auto, zzz: 1}",
            "scalar" to "auto",
            "null-doc" to "~",
        ),
        "FilteringConfiguration" to listOf(
            "empty" to "{}",
            "allow-null" to "allowlist: ~",
            "allow-null-elem" to "allowlist: [~]",
            "allow-one" to "allowlist: [{type: method, regex: \".*\"}]",
            "allow-scalar-elem" to "allowlist: [x]",
            "block-only" to "blocklist: [{type: allure}]",
            "unknown" to "zzz: 1",
        ),
        "TestFilterConfiguration" to listOf(
            "method-regex" to "{type: method, regex: \".*\"}",
            "method-empty" to "{type: method}",
            "method-values-null-elem" to "{type: method, values: [~, a]}",
            "package-sub" to "{type: package, regex: \".*\", subpackages: true}",
            "annotation-data" to "{type: annotationData, nameRegex: a, valueRegex: b}",
            "annotation-data-missing" to "{type: annotationData, nameRegex: a}",
            "annotation-data-null" to "{type: annotationData, nameRegex: ~, valueRegex: b}",
            "fragmentation" to "{type: fragmentation, index: 0, count: 2}",
            "fragmentation-missing" to "{type: fragmentation, index: 0}",
            "composition" to "{type: composition, filters: [{type: allure}], op: UNION}",
            "composition-null-elem" to "{type: composition, filters: [~], op: UNION}",
            "composition-bad-op" to "{type: composition, filters: [], op: bogus}",
            "allure" to "{type: allure}",
            "allure-disabled" to "{type: allure, enabled: false}",
            "allure-extra" to "{type: allure, zzz: 1}",
            "fqcn-file" to "{type: fully-qualified-class-name, file: filterfile}",
            "unknown-prop" to "{type: method, regex: x, zzz: 1}",
        ),
        "Pooling" to listOf(
            "omni" to "{type: omni}",
            "omni-extra" to "{type: omni, zzz: 1}",
            "omni-scalar" to "omni",
            "combo" to "type: combo\nlist: [{type: omni}, {type: abi}]",
            "combo-null" to "type: combo\nlist: ~",
            "combo-null-elem" to "type: combo\nlist: [~]",
            "combo-missing" to "{type: combo}",
            "combo-nested" to "type: combo\nlist: [{type: combo, list: [{type: omni}]}]",
        ),
        "Sharding" to listOf(
            "parallel" to "{type: parallel}",
            "count" to "{type: count, count: 3}",
            "count-missing" to "{type: count}",
            "count-null" to "{type: count, count: ~}",
        ),
        "Sorting" to listOf(
            "no-sorting" to "{type: no-sorting}",
            "no-sorting-extra" to "{type: no-sorting, zzz: 1}",
            "random" to "{type: random-order}",
            "exec-missing-pct" to "{type: execution-time, timeLimit: PT1H}",
            "exec-null-pct" to "{type: execution-time, percentile: ~, timeLimit: PT1H}",
            "exec-bool-pct" to "{type: execution-time, percentile: true, timeLimit: PT1H}",
            "exec-overflow" to "{type: execution-time, percentile: 80, timeLimit: P106751991167300D}",
            "exec-findvalue-array" to "type: execution-time\nitems: [{percentile: 33}]\ntimeLimit: PT1H",
            "exec-str-pct" to "{type: execution-time, percentile: \"80.5\", timeLimit: PT1H}",
            "exec-obj-pct" to "type: execution-time\npercentile: {a: 1}\ntimeLimit: PT1H",
            "rate-full" to "type: success-rate\ntimeLimit: \"2026-01-01T00:00:00Z\"\nascending: true",
            "rate-null-asc" to "type: success-rate\ntimeLimit: \"2026-01-01T00:00:00Z\"\nascending: ~",
            "scalar-doc" to "x",
            "array-doc" to "[1]",
        ),
        "Batching" to listOf(
            "isolate" to "{type: isolate}",
            "class-name" to "{type: class-name}",
            "fixed-full" to "type: fixed-size\nsize: 5\ndurationMillis: 100\npercentile: 90\nlastMileLength: 2",
            "fixed-missing-size" to "{type: fixed-size}",
            "fixed-null-size" to "{type: fixed-size, size: ~}",
            "fixed-bool-size" to "{type: fixed-size, size: true}",
            "fixed-str-size" to "{type: fixed-size, size: \"7\"}",
            "fixed-decimal-str" to "{type: fixed-size, size: \"7.9\"}",
            "fixed-overflow" to "{type: fixed-size, size: 5, timeLimit: P106751991167300D}",
            "fixed-findvalue-array" to "type: fixed-size\nitems: [{size: 9}]",
            "fixed-null-optionals" to "{type: fixed-size, size: 5, durationMillis: ~, percentile: ~, lastMileLength: ~}",
            "fixed-str-duration" to "{type: fixed-size, size: 5, durationMillis: \"12.9\"}",
        ),
        "Flakiness" to listOf(
            "ignore" to "{type: ignore}",
            "prob-missing-rate" to "{type: probability, maxCount: 3, timeLimit: PT1H}",
            "prob-missing-count" to "{type: probability, minSuccessRate: 0.5, timeLimit: PT1H}",
            "prob-null-rate" to "{type: probability, minSuccessRate: ~, maxCount: 3, timeLimit: PT1H}",
            "prob-num-timelimit-bogus" to "type: probability\nminSuccessRate: 0.5\nmaxCount: 3\ntimeLimit: \"bogus\"",
            "prob-overflow" to "{type: probability, minSuccessRate: 0.5, maxCount: 3, timeLimit: P106751991167300D}",
        ),
        "Retry" to listOf(
            "no-retry" to "{type: no-retry}",
            "fixed-empty" to "{type: fixed-quota}",
            "fixed-partial" to "{type: fixed-quota, retryPerTestQuota: 9}",
            "fixed-null" to "{type: fixed-quota, totalAllowedRetryQuota: ~}",
        ),
    )
}
