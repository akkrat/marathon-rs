package oracle

import com.malinskiy.marathon.config.vendor.apple.AppleTestBundleConfiguration
import io.ktor.util.decodeString
import kotlinx.coroutines.runBlocking
import java.io.File
import java.nio.ByteBuffer
import java.nio.file.Files
import java.util.zip.CRC32
import java.util.zip.ZipEntry
import java.util.zip.ZipOutputStream

/**
 * WP 2.3b bundle probes: the real [AppleTestBundleConfiguration.validate]
 * over temp-dir layouts and synthetic zips, plus the two ktor facts the
 * dynamic frame decoder needs (`readInt` is `ByteBuffer` order as used by
 * ktor's `ByteReadChannel.readInt`, probed via `ByteBuffer.getInt` on a
 * default-order buffer, and `ByteBuffer.decodeString` on malformed UTF-8).
 * Paths record as `$DIR`-relative; bracketed path lists in messages are
 * sorted before recording because Kotlin's walk order is the filesystem's
 * (WP 2.3b S4 sorts in Rust).
 */
object BundleProbe {
    private val dirToken = "\$DIR"

    fun generate(outDir: File) {
        val root = Files.createTempDirectory("bundle-probe").toFile().canonicalFile
        try {
            val table = JsonlTable(
                "bundle-probe",
                "WP 2.3b: AppleTestBundleConfiguration.validate() probe cases over temp layouts; paths are \$DIR-relative, message path lists sorted (S4)",
                linkedMapOf(
                    "id" to "case name",
                    "result" to "{ok: {app, testApp, xctest, appExists, tree}} or {error: {class, message, causes}, tree}",
                ),
            )
            for (case in cases) {
                val dir = File(root, case.first).apply { mkdirs() }
                val result = try {
                    val config = case.second(dir)
                    config.validate()
                    mapOf(
                        "ok" to mapOf(
                            "app" to config.app?.let { rel(it, dir) },
                            "testApp" to config.testApp?.let { rel(it, dir) },
                            "xctest" to rel(config.xctest, dir),
                            "appExists" to config.app?.exists(),
                            "tree" to tree(dir),
                        )
                    )
                } catch (t: Throwable) {
                    mapOf("error" to sortedLists(substitute(errorJson(t), dir)), "tree" to tree(dir))
                }
                table.add(linkedMapOf("id" to case.first, "result" to result))
            }
            table.add(linkedMapOf("id" to "ktor-facts", "result" to mapOf("ok" to ktorFacts())))
            table.writeTo(outDir)
        } finally {
            // Undo the cases' chmods first, so nothing stays behind.
            root.walkTopDown().onEnter { it.setReadable(true); it.setWritable(true); it.setExecutable(true) }.forEach { }
            root.deleteRecursively()
        }
    }

    private fun ktorFacts(): Map<String, Any?> {
        // readInt(): ktor reads a big-endian int (ByteOrder.BIG_ENDIAN packet).
        val channel = io.ktor.utils.io.ByteReadChannel(byteArrayOf(0x01, 0x02, 0x03, 0x04))
        val readInt = runBlocking { channel.readInt() }
        // decodeString() over malformed UTF-8 (a lone continuation byte and a
        // truncated sequence).
        val malformed = try {
            ByteBuffer.wrap(byteArrayOf(0x61, 0x80.toByte(), 0xC3.toByte())).decodeString()
        } catch (t: Throwable) {
            "THROWS ${t.javaClass.name}: ${t.message}"
        }
        val eof = try {
            val empty = io.ktor.utils.io.ByteReadChannel(byteArrayOf())
            runBlocking { empty.readInt() }
            "NO THROW"
        } catch (t: Throwable) {
            t.javaClass.name
        }
        return mapOf("readInt01020304" to readInt, "decodeStringMalformed" to malformed, "readIntEofClass" to eof)
    }

    private fun rel(file: File, dir: File): String =
        file.absolutePath.removePrefix(dir.absolutePath).removePrefix("/")

    private fun substitute(value: Any?, dir: File): Any? = when (value) {
        is String -> value.replace(dir.absolutePath, dirToken)
        is Map<*, *> -> value.mapValues { (_, v) -> substitute(v, dir) }
        is Collection<*> -> value.map { substitute(it, dir) }
        else -> value
    }

    private val bracketList = Regex("""\[([^\]]*)]""")

    private fun sortedLists(value: Any?): Any? = when (value) {
        is String -> bracketList.replace(value) { m ->
            val items = m.groupValues[1].split(", ").filter { it.isNotEmpty() }.sorted()
            items.joinToString(", ", "[", "]")
        }
        is Map<*, *> -> value.mapValues { (_, v) -> sortedLists(v) }
        is Collection<*> -> value.map { sortedLists(it) }
        else -> value
    }

    private fun tree(dir: File): List<String> =
        dir.walkTopDown().filter { it != dir }.map { file ->
            val relative = rel(file, dir)
            when {
                file.isDirectory -> "$relative/"
                // Archive bytes embed zip timestamps; their contents are the
                // extracted tree entries, recorded separately.
                file.extension.lowercase() in setOf("zip", "ipa") -> "$relative=<archive>"
                else -> "$relative=${file.readText()}"
            }
        }.sorted().toList()

    private fun mkdirs(parent: File, path: String): File = File(parent, path).apply { mkdirs() }

    private fun bundle(parent: File, path: String): File =
        mkdirs(parent, path).also { File(it, "empty").writeText("") }

    private fun zip(parent: File, name: String, vararg entries: Pair<String, String?>): File {
        val file = File(parent, name)
        ZipOutputStream(file.outputStream()).use { out ->
            for ((entryName, content) in entries) {
                out.putNextEntry(ZipEntry(entryName))
                if (content != null) out.write(content.toByteArray())
                out.closeEntry()
            }
        }
        return file
    }

    /** Entries whose names are written as raw bytes, with no language-encoding flag (bit 11). */
    private fun zipRawNames(parent: File, name: String, vararg entries: Pair<ByteArray, String>): File {
        val file = File(parent, name)
        ZipOutputStream(file.outputStream(), Charsets.ISO_8859_1).use { out ->
            for ((entryName, content) in entries) {
                out.putNextEntry(ZipEntry(String(entryName, Charsets.ISO_8859_1)))
                out.write(content.toByteArray())
                out.closeEntry()
            }
        }
        return file
    }

    /** The Info-ZIP Unicode Path extra field (0x7075) naming [unicodeName] for an entry stored as [rawName]. */
    private fun unicodePathExtra(rawName: String, unicodeName: String): ByteArray =
        unicodePathExtra(rawName, unicodeName.toByteArray())

    private fun unicodePathExtra(rawName: String, unicodeName: ByteArray): ByteArray {
        val crc = CRC32().apply { update(rawName.toByteArray()) }.value
        val data = byteArrayOf(1) + ByteBuffer.allocate(4).order(java.nio.ByteOrder.LITTLE_ENDIAN).putInt(crc.toInt()).array() +
            unicodeName
        return byteArrayOf(0x75, 0x70, data.size.toByte(), 0) + data
    }

    private fun replaceBytes(file: File, from: String, to: String) {
        val bytes = String(file.readBytes(), Charsets.ISO_8859_1).replace(from, to)
        file.writeBytes(bytes.toByteArray(Charsets.ISO_8859_1))
    }

    private val cases: List<Pair<String, (File) -> AppleTestBundleConfiguration>> = listOf(
        "empty-config" to { AppleTestBundleConfiguration() },
        "root-entry" to { dir ->
            val app = zip(dir, "app.ipa", "Info.plist" to "ROOT", "Payload/Sample.app/" to null, "Payload/Sample.app/empty" to "")
            AppleTestBundleConfiguration(application = app, testApplication = bundle(dir, "ui.xctest"))
        },
        "macosx-entries" to { dir ->
            val app = zip(
                dir, "app.zip",
                "__MACOSX" to "EXACT", ".DS_Store" to "DSROOT", "sub/.DS_Store" to "DSNESTED",
                "__MACOSX/Payload/._Sample.app" to "FORK", "Payload/Sample.app/empty" to "",
            )
            AppleTestBundleConfiguration(application = app, testApplication = bundle(dir, "ui.xctest"))
        },
        "ambiguous-2-apps" to { dir ->
            bundle(dir, "derived/ui.xctest"); bundle(dir, "derived/a.app"); bundle(dir, "derived/b.app")
            AppleTestBundleConfiguration(derivedDataDir = File(dir, "derived"))
        },
        "ambiguous-3-xctest" to { dir ->
            bundle(dir, "derived/a.xctest"); bundle(dir, "derived/b.xctest"); bundle(dir, "derived/c.xctest"); bundle(dir, "derived/a.app")
            AppleTestBundleConfiguration(derivedDataDir = File(dir, "derived"))
        },
        "watch-pair" to { dir ->
            bundle(dir, "derived/ui.xctest"); bundle(dir, "derived/X.app/Watch/Y.app"); File(dir, "derived/X.app/empty").writeText("")
            AppleTestBundleConfiguration(derivedDataDir = File(dir, "derived"))
        },
        "derived-no-app" to { dir ->
            bundle(dir, "derived/ui.xctest")
            AppleTestBundleConfiguration(derivedDataDir = File(dir, "derived"))
        },
        "derived-no-xctest" to { dir ->
            bundle(dir, "derived/a.app")
            AppleTestBundleConfiguration(derivedDataDir = File(dir, "derived"))
        },
        "corrupt-archive" to { dir ->
            val app = File(dir, "app.ipa").apply { writeText("not a zip") }
            AppleTestBundleConfiguration(application = app, testApplication = bundle(dir, "ui.xctest"))
        },
        "missing-archive" to { dir ->
            AppleTestBundleConfiguration(application = File(dir, "nope.ipa"), testApplication = bundle(dir, "ui.xctest"))
        },
        "same-stem-archives" to { dir ->
            val app = zip(dir, "bundle.zip", "A.app/empty" to "")
            val test = zip(dir, "bundle.ipa", "T.app/PlugIns/ui.xctest/empty" to "", "T.app/empty" to "")
            AppleTestBundleConfiguration(application = app, testApplication = test)
        },
        "zip-slip" to { dir ->
            val app = zip(dir, "app.zip", "../escape.txt" to "ESCAPED", "A.app/empty" to "")
            AppleTestBundleConfiguration(application = app, testApplication = bundle(dir, "ui.xctest"))
        },
        "app-regular-file" to { dir ->
            val app = File(dir, "X.app").apply { writeText("") }
            AppleTestBundleConfiguration(application = app, testApplication = bundle(dir, "ui.xctest"))
        },
        "ipa-directory" to { dir ->
            AppleTestBundleConfiguration(application = mkdirs(dir, "X.ipa"), testApplication = bundle(dir, "ui.xctest"))
        },
        "uppercase-extension" to { dir ->
            val app = zip(dir, "APP.IPA", "A.app/empty" to "")
            AppleTestBundleConfiguration(application = app, testApplication = bundle(dir, "ui.xctest"))
        },
        "test-archive-xctest-only" to { dir ->
            val test = zip(dir, "test.zip", "ui.xctest/empty" to "")
            AppleTestBundleConfiguration(application = bundle(dir, "a.app"), testApplication = test)
        },
        "test-archive-app-scopes-xctest" to { dir ->
            // The .app inside the test archive has no xctest; one exists at the
            // top of the extraction but must NOT be found (scoping).
            val test = zip(dir, "test.zip", "T.app/empty" to "", "ui.xctest/empty" to "")
            AppleTestBundleConfiguration(application = bundle(dir, "a.app"), testApplication = test)
        },
        "lone-application-with-derived" to { dir ->
            bundle(dir, "derived/ui.xctest"); bundle(dir, "derived/a.app")
            AppleTestBundleConfiguration(application = File(dir, "missing.ipa"), derivedDataDir = File(dir, "derived"))
        },
        "hidden-archive-stem" to { dir ->
            // nameWithoutExtension of ".zip" is empty, so the default
            // extraction dir is the parent itself (WP 2.3b S6 rejects this
            // instead). Run it in its own subdir so the wipe is contained.
            val sub = mkdirs(dir, "sub")
            val app = zip(sub, ".zip", "A.app/empty" to "")
            AppleTestBundleConfiguration(application = app, testApplication = bundle(dir, "ui.xctest"))
        },
        // Round 1 review cases (docs/wp/2.3b-findings.md "Review round 1").
        "stem-dot" to { dir ->
            // "..zip": the stem "." makes the extraction dir the parent itself (S6).
            val sub = mkdirs(dir, "a").also { File(it, "KEEP").writeText("KEEP") }
            val app = zip(sub, "..zip", "A.app/empty" to "")
            AppleTestBundleConfiguration(application = app, testApplication = bundle(dir, "ui.xctest"))
        },
        "stem-dotdot" to { dir ->
            // "...zip": the stem ".." makes the extraction dir the grandparent (S6).
            mkdirs(dir, "a").also { File(it, "KEEP").writeText("KEEP") }
            val sub = mkdirs(dir, "a/b").also { File(it, "KEEP").writeText("KEEP") }
            val app = zip(sub, "...zip", "A.app/empty" to "")
            AppleTestBundleConfiguration(application = app, testApplication = bundle(dir, "ui.xctest"))
        },
        "utf8-unflagged-name" to { dir ->
            val app = zipRawNames(dir, "app.zip", "Café.app/empty".toByteArray() to "")
            AppleTestBundleConfiguration(application = app, testApplication = bundle(dir, "ui.xctest"))
        },
        "unicode-path-extra" to { dir ->
            // ZipFile ignores the 0x7075 field: the entry stays A.app.
            val file = File(dir, "app.zip")
            ZipOutputStream(file.outputStream()).use { out ->
                out.putNextEntry(ZipEntry("A.app/empty").apply { extra = unicodePathExtra("A.app/empty", "B.app/empty") })
                out.closeEntry()
            }
            AppleTestBundleConfiguration(application = file, testApplication = bundle(dir, "ui.xctest"))
        },
        "symlink-escape" to { dir ->
            // A stale extraction dir whose cleanup fails (locked is r-x) and
            // which holds a link to a directory outside it.
            val outside = mkdirs(dir, "outside").also { File(it, "KEEP").writeText("KEEP") }
            val locked = mkdirs(dir, "input/locked")
            Files.createSymbolicLink(File(locked, "link").toPath(), outside.toPath())
            locked.setWritable(false, false)
            val app = zip(dir, "input.zip", "locked/link/owned" to "ESCAPE", "B.app/empty" to "")
            AppleTestBundleConfiguration(application = app, testApplication = bundle(dir, "ui.xctest"))
        },
        "symlinked-destination" to { dir ->
            val target = mkdirs(dir, "target").also { File(it, "KEEP").writeText("KEEP") }
            Files.createSymbolicLink(File(dir, "app").toPath(), target.toPath())
            val app = zip(dir, "app.zip", "A.app/empty" to "")
            AppleTestBundleConfiguration(application = app, testApplication = bundle(dir, "ui.xctest"))
        },
        "unreadable-dir-in-derived" to { dir ->
            bundle(dir, "derived/ui.xctest"); bundle(dir, "derived/A.app")
            mkdirs(dir, "derived/locked").apply { setReadable(false, false); setWritable(false, false); setExecutable(false, false) }
            AppleTestBundleConfiguration(derivedDataDir = File(dir, "derived"))
        },
        "bad-crc-stored" to { dir ->
            val file = File(dir, "app.zip")
            ZipOutputStream(file.outputStream()).use { out ->
                val content = "HELLO".toByteArray()
                out.putNextEntry(ZipEntry("B.app/data").apply {
                    method = ZipEntry.STORED; size = 5; compressedSize = 5
                    crc = CRC32().apply { update(content) }.value
                })
                out.write(content)
                out.closeEntry()
            }
            replaceBytes(file, "HELLO", "WORLD")
            AppleTestBundleConfiguration(application = file, testApplication = bundle(dir, "ui.xctest"))
        },
        "mkdirs-ignored" to { dir ->
            val app = zip(dir, "app.zip", "junk" to "FILE", "junk/junk/child/" to null, "B.app/empty" to "")
            AppleTestBundleConfiguration(application = app, testApplication = bundle(dir, "ui.xctest"))
        },
        "duplicate-entries" to { dir ->
            // ZipOutputStream refuses duplicate names, so the first entry is
            // renamed in place afterwards (same length, names carry no CRC).
            val app = zip(
                dir, "app.zip",
                "B.app/dat1" to "FIRST", "parent/file" to "FILE", "parent/file/child" to "next", "B.app/data" to "LAST",
            )
            replaceBytes(app, "B.app/dat1", "B.app/data")
            AppleTestBundleConfiguration(application = app, testApplication = bundle(dir, "ui.xctest"))
        },
        // Round 2 review cases (docs/wp/2.3b-findings.md "Review round 2").
        "destination-link-locked" to { dir ->
            // The extraction dir itself is a stale link out of the tree, and
            // its r-x parent keeps deleteRecursively from unlinking it.
            val outside = mkdirs(dir, "outside")
            val locked = mkdirs(dir, "locked")
            Files.createSymbolicLink(File(locked, "input").toPath(), outside.toPath())
            val app = zip(locked, "input.zip", "B.app/owned" to "ESCAPE")
            locked.setWritable(false, false)
            AppleTestBundleConfiguration(application = app, testApplication = bundle(dir, "ui.xctest"))
        },
        "stale-hard-link" to { dir ->
            // A stale extraction tree holding a hard link to a file outside
            // it; the r-x parent keeps the cleanup from removing it.
            val outside = mkdirs(dir, "outside").also { File(it, "owned").writeText("KEEP") }
            val locked = mkdirs(dir, "input/locked")
            Files.createLink(File(locked, "link").toPath(), File(outside, "owned").toPath())
            locked.setWritable(false, false)
            val app = zip(dir, "input.zip", "locked/link" to "ESCAPE", "B.app/empty" to "")
            AppleTestBundleConfiguration(application = app, testApplication = bundle(dir, "ui.xctest"))
        },
        "unicode-path-collision" to { dir ->
            // Two stored names, one 0x7075 name: ZipFile sees two apps.
            val file = File(dir, "app.zip")
            ZipOutputStream(file.outputStream()).use { out ->
                for ((name, content) in listOf("A.app/empty" to "FIRST", "B.app/empty" to "LAST")) {
                    out.putNextEntry(ZipEntry(name).apply { extra = unicodePathExtra(name, "Z.app/empty") })
                    out.write(content.toByteArray())
                    out.closeEntry()
                }
            }
            AppleTestBundleConfiguration(application = file, testApplication = bundle(dir, "ui.xctest"))
        },
        "unicode-path-invalid" to { dir ->
            // A 0x7075 payload that is not UTF-8; ZipFile never reads it.
            val file = File(dir, "app.zip")
            ZipOutputStream(file.outputStream()).use { out ->
                out.putNextEntry(ZipEntry("A.app/empty").apply { extra = unicodePathExtra("A.app/empty", byteArrayOf(0xff.toByte())) })
                out.closeEntry()
            }
            AppleTestBundleConfiguration(application = file, testApplication = bundle(dir, "ui.xctest"))
        },
    )
}
