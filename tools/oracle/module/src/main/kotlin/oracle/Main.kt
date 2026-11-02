package oracle

import java.io.File
import kotlin.system.exitProcess

/** Entry point of the oracle tools; tools/oracle/README.md documents each command. */
fun main(args: Array<String>) {
    val rest = args.drop(1)
    when (args.firstOrNull()) {
        "binding-review-corpus" -> BindingReviewCorpus.generate(File(rest.singleOrNull() ?: usage()))
        "binding-corpus" -> BindingCorpus.generate(File(rest.singleOrNull() ?: usage()))
        "schema-corpus" -> SchemaCorpus.generate(File(rest.singleOrNull() ?: usage()))
        "vendor-corpus" -> VendorCorpus.generate(File(rest.singleOrNull() ?: usage()))
        "devices-corpus" -> DevicesCorpus.generate(File(rest.singleOrNull() ?: usage()))
        "validation-corpus" -> {
            if (rest.size != 2) usage()
            ValidationCorpus.generate(File(rest[0]), File(rest[1]))
        }
        "bundle-probe" -> BundleProbe.generate(File(rest.singleOrNull() ?: usage()))
        "frame-eof-probe" -> FrameEofProbe.generate(File(rest.singleOrNull() ?: usage()))
        "device-actor-probe" -> DeviceActorProbe.generate(File(rest.singleOrNull() ?: usage()))
        "logging-probe" -> loggingProbe(rest)
        "scenario" -> scenario(rest)
        "config-differential-seeds" -> configDifferentialSeeds(rest)
        "config-dump" -> configDump(rest)
        "marathonfile-writes" -> marathonfileWrites(rest)
        "string-substitutor-edges" -> StringSubstitutorEdges.generate(File(rest.singleOrNull() ?: usage()))
        "formatting-round1" -> FormattingRound1.generate(File(rest.singleOrNull() ?: usage()))
        "jcompat-tables" -> JcompatTables.generate(File(rest.singleOrNull() ?: usage()))
        "yaml-round3-corpus" -> YamlReviewCorpus.generate(File(rest.singleOrNull() ?: usage()), round3 = true)
        "yaml-review-corpus" -> YamlReviewCorpus.generate(File(rest.singleOrNull() ?: usage()))
        "yaml-corpus" -> YamlCorpus.generate(File(rest.singleOrNull() ?: usage()))
        "regex-corpus" -> RegexCorpus.generate(File(rest.singleOrNull() ?: usage()))
        else -> usage()
    }
}

private fun usage(): Nothing {
    System.err.println(
        """
        usage: oracle config-dump <Marathonfile> [--env-file F] [--now <ISO instant>]
               oracle marathonfile-writes <Marathonfile> [--env-file F]
               oracle formatting-round1 <output dir>
               oracle jcompat-tables <output dir>
               oracle string-substitutor-edges <output dir>
               oracle logging-probe <startup|android|apple|compact|details|fallback>
               oracle logging-probe state <output file>
               oracle yaml-corpus <output file>
               oracle yaml-review-corpus <output file>
               oracle yaml-round3-corpus <output file>
               oracle regex-corpus <output file>
               oracle vendor-corpus <output dir>
               oracle devices-corpus <output dir>
               oracle validation-corpus <output dir> <fixtures/configuration/fixture/config>
               oracle bundle-probe <output dir>
               oracle frame-eof-probe <output dir>
               oracle device-actor-probe <output file>
               oracle scenario <scenario.json> <empty work dir>
        """.trimIndent()
    )
    exitProcess(2)
}
