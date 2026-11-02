package oracle

import ch.qos.logback.classic.Logger
import ch.qos.logback.core.read.ListAppender
import ch.qos.logback.classic.spi.ILoggingEvent
import com.malinskiy.marathon.device.*
import com.malinskiy.marathon.extension.escape
import com.malinskiy.marathon.extension.md5
import com.malinskiy.marathon.io.*
import com.malinskiy.marathon.test.Test
import com.malinskiy.marathon.test.toTestName
import org.slf4j.LoggerFactory
import java.io.File

/** WP 1.7 edge cases; all expected values come from the unmodified Kotlin/JDK implementation. */
object FileManagerEdges {
    fun generate(dir: File) {
        val table = JsonlTable("file_manager_edges", "WP 1.7 UTF-16 paths, diagnostics and original Kotlin test witnesses",
            linkedMapOf("id" to "case name", "op" to "file/folder/result (optional _utf16) | escape_utf16 | md5_utf16 | absolute | fragment | filesystem | native_file",
                "args" to "operation inputs", "result" to "UTF-16 units, or error class; messages are UTF-16 arrays"))
        val root = File("/tmp/oracle-wp17")
        root.deleteRecursively()
        val logger = LoggerFactory.getLogger("FileManager") as Logger
        val appender = ListAppender<ILoggingEvent>().apply { start() }
        logger.addAppender(appender)
        fun units(s: String) = s.map { it.code }
        fun device(serial: String) = DeviceInfo(OperatingSystem("23"), serial, "m", "x", NetworkState.CONNECTED, emptyList(), true)
        fun row(id: String, op: String = "file", maxPath: Int = 0, maxFilename: Int = 255,
                output: String = "${root.path}/out", pool: String = "testPoolId", serial: String = "xxyyzz",
                test: Test? = Test("com.example", "Clazz", "method", emptyList()),
                batch: String? = "batchId", type: FileType = FileType.LOG,
                extraId: String? = null, chunk: String? = null, filename: String = "raw.json") {
            appender.list.clear()
            val args = linkedMapOf<String, Any?>("maxPath" to maxPath, "maxFilename" to maxFilename, "output" to output,
                "pool" to pool, "serial" to serial, "testName" to test?.toTestName(), "batch" to batch,
                "type" to type.name, "id" to extraId, "chunk" to chunk, "filename" to filename)
            if (op.endsWith("_utf16")) {
                for ((key, value) in args.toMap()) { if (value is String) args[key] = units(value) }
                args["type"] = type.name
            }
            val fm = FileManager(maxPath, maxFilename, File(output))
            val result = try {
                val file = when (op.removeSuffix("_utf16")) {
                    "folder" -> fm.createFolder(FolderType.DEVICE_FILES, DevicePoolId(pool), device(serial))
                    "result" -> fm.createTestResultFile(filename)
                    else -> fm.createFile(type, DevicePoolId(pool), device(serial), test, batch, extraId, chunk)
                }
                mapOf("units" to units(file.absolutePath))
            } catch (e: Throwable) { mapOf("error" to e.javaClass.name) }
            table.add(linkedMapOf("id" to id, "op" to op, "args" to args, "result" to result,
                "messages" to appender.list.map { units(it.formattedMessage) }))
        }
        try {
            val escapeWitness = "com.example.MyTest#method[1 - some parameters]"
            table.add(linkedMapOf("id" to "test_string_escaping", "op" to "escape_utf16", "args" to units(escapeWitness), "result" to units(escapeWitness.escape())))
            row("create_filename_normal_length_test")
            // Original test input as code units: kept as numbers so the source text stays free of random words.
            row("create_filename_long_length_method_limited_path_test", maxPath = 255, maxFilename = 0,
                test = Test("com.example", "Clazz", String(intArrayOf(112, 101, 102, 97, 108, 103, 107, 120, 98, 110, 102, 114, 118, 120, 115, 100, 112, 114, 116, 111, 112, 114, 103, 103, 115, 105, 98, 113, 97, 101, 111, 98, 103, 105, 121, 118, 97, 105, 97, 116, 121, 115, 97, 106, 101, 109, 99, 111, 97, 109, 117, 98, 112, 112, 118, 112, 112, 105, 98, 121, 117, 107, 110, 121, 101, 113, 104, 105, 112, 107, 118, 122, 119, 121, 101, 115, 110, 103, 97, 122, 103, 121, 99, 112, 107, 119, 122, 97, 104, 121, 111, 99, 115, 102, 115, 120, 99, 108, 102, 103, 97, 120, 118, 112, 119, 114, 115, 114, 106, 117, 107, 119, 114, 116, 108, 119, 112, 114, 101, 101, 119, 108, 118, 101, 107, 113, 118, 107, 105, 98, 118, 116, 119, 108, 105, 122, 97, 120, 119, 119, 118, 122, 98, 103, 102, 101, 112, 119, 121, 112, 105, 119, 112, 98, 102, 103, 122, 99, 104, 106, 114, 121, 116, 118, 122, 97, 119, 111, 117, 108, 117, 112, 117, 105, 116, 98, 120, 107, 103, 99, 104, 106, 105, 104, 98, 98, 106, 100, 97, 99, 111, 104, 120, 108, 101, 111, 106, 116, 99, 77, 121, 82, 97, 110, 100, 111, 109, 84, 101, 120, 116, 69, 110, 100, 105, 110, 103, 87, 105, 116, 104, 77, 111, 114, 101, 84, 104, 97, 110, 50, 53, 54, 67, 104, 97, 114, 97, 99, 116, 101, 114, 115, 76, 101, 110, 103, 116, 104, 84, 111, 86, 101, 114, 105, 102, 121, 67, 117, 116, 80, 111, 105, 110, 116).map { it.toChar() }.toCharArray()), emptyList()))
            row("test_create_filename_named_parameterized_long", maxPath = 255, maxFilename = 0,
                test = Test("com.example", "Clazz", "someRidiculousLongLongLongLongLongLongLongLongLongLongLongLongLongLongLongNamepefalgkxbnfrvxsdprtoprggsibqaeobgiyvaiatysajemcoamubppvppibyuknyeqhipkvzwyesngazgycpkwzah[Some parameter with spaces: /rest/api/2/endpoint, Other parameter: 1, And another one: {3}]", emptyList()))
            row("test_device_serial_escaping", serial = "127.0.0.1:5037:emulator-5554", test = null, batch = null)
            row("test_screenshotfile", serial = "127.0.0.1:5037:emulator-5554", batch = null,
                type = FileType.SCREENSHOT_PNG, extraId = "on-device-test")
            val proposed = File(root, "LOG/testPoolId/xxyyzz").absolutePath
            row("test_too_long_output_path_unlimited_filename", maxPath = 255, maxFilename = 0,
                output = File(root, "x".repeat(255 - proposed.length)).path,
                test = Test("com.xxyyzzxxyy.android.abcdefgh.abcdefghi", "PackageNameTest", "useAppContext", emptyList()))
            for (type in FileType.values()) row("type-${type.name}", type = type, test = null, batch = null)
            for ((i, pool) in listOf("", "/absolute", "a//b/", "../pool", "é😀", ".", "..").withIndex()) {
                row("pool-file-$i", pool = pool)
                row("pool-folder-$i", op = "folder", pool = if (pool.startsWith("/")) "${root.path}/absolute" else pool)
            }
            for ((i, name) in listOf("a/b\\c:<*>?\"|", "😀é中", "x".repeat(5000), "", "a//b/../c").withIndex()) {
                row("id-$i", extraId = name, chunk = "😀/é", maxFilename = 30)
                row("batch-$i", test = null, batch = name, maxFilename = 30)
                row("result-$i", op = "result", filename = name, maxFilename = 30)
            }
            for (limit in listOf(-1, 0, 1, 2, 3, 10, 16, 17, 18, 24, 25, 26, 27, 28, 29, 30, 60, 255)) {
                row("path-$limit", maxPath = limit, maxFilename = 0, output = "${root.path}/😀/out")
                if (limit <= 0 || limit == 1 || limit == 4 || limit > root.path.length)
                    row("folder-$limit", op = "folder", maxPath = limit, output = "${root.path}/😀/out")
                row("filename-$limit", maxFilename = limit, test = null, batch = "😀é中", extraId = "😀")
            }
            for ((i, value) in listOf("\uD800", "\uDC00", "a\u0000b", "😀").withIndex()) {
                row("raw-id-$i", op = "file_utf16", extraId = value)
                row("raw-test-$i", op = "file_utf16", test = Test("", "C", value, emptyList()))
                row("raw-pool-$i", op = "file_utf16", pool = value, maxPath = 1)
                row("raw-output-$i", op = "folder_utf16", output = "${root.path}/$value", pool = "${root.path}/override", maxPath = 1)
                row("raw-result-$i", op = "result_utf16", filename = value)
                row("raw-result-output-$i", op = "result_utf16", output = "${root.path}/$value")
            }
            val nativeRoot = File(root, "native").apply { mkdirs() }
            val malformed = "name\uD800"
            val rejected = try { File(nativeRoot, malformed).createNewFile(); false } catch (e: java.io.IOException) { true }
            table.add(linkedMapOf("id" to "native-file", "op" to "native_file", "args" to units(malformed), "result" to rejected))
            val fsRoot = File(root, "filesystem")
            val fs = FileManager(0, 255, fsRoot)
            val planned = fs.createFile(FileType.LOG, DevicePoolId("p"), device("s"), Test("", "C", "m", emptyList()))
            val folder = fs.createFolder(FolderType.HTML)
            val blocked = File(root, "blocked").apply { writeText("x") }
            val bad = FileManager(0, 255, blocked)
            val ignored = bad.createTestResultFile("raw.json")
            val nioFailed = try { bad.createFile(FileType.LOG, DevicePoolId("p"), device("s")); false } catch (e: java.io.IOException) { true }
            table.add(linkedMapOf("id" to "filesystem", "op" to "filesystem", "result" to linkedMapOf(
                "fileExists" to planned.exists(), "directoryExists" to planned.parentFile.isDirectory,
                "folderExists" to folder.isDirectory, "resultExists" to ignored.exists(), "nioFailed" to nioFailed)))
            for (hash in listOf("-170141183460469231731687303715884105728", "-123456789", "0", "123456789")) {
                for (count in listOf(0, -1, -2, Int.MIN_VALUE, 1, 2, Int.MAX_VALUE)) {
                    val result = try {
                        val power = java.math.BigInteger.valueOf(count.toLong())
                        val rem = java.math.BigInteger(hash).remainder(power)
                        (if (rem.signum() < 0) rem + power else rem).toString()
                    } catch (e: ArithmeticException) { "ArithmeticException" }
                    table.add(linkedMapOf("id" to "remainder-$hash-$count", "op" to "fragment", "hash" to hash, "count" to count, "result" to result))
                }
            }
            row("both-limits", maxFilename = 15, maxPath = 35)
            row("dot-output", output = "${root.path}//out/../out///")
            row("null-filename", op = "result", filename = "a\u0000b")
            for ((i, value) in listOf("😀", "\uD800", "\uDC00", "\uD800x\uDC00", "\uD800\uD800\uDC00", "a\u0000b").withIndex()) {
                table.add(linkedMapOf("id" to "escape-$i", "op" to "escape_utf16", "args" to units(value), "result" to units(value.escape())))
                table.add(linkedMapOf("id" to "hash-$i", "op" to "md5_utf16", "args" to units(value), "result" to value.md5().toString()))
            }
            for ((i, value) in listOf("", ".", "../a", "a//b/", "/a//../b/").withIndex()) {
                table.add(linkedMapOf("id" to "absolute-$i", "op" to "absolute", "args" to value,
                    "cwd" to "/oracle-cwd", "result" to units(File(value).absolutePath.replace(System.getProperty("user.dir"), "/oracle-cwd"))))
            }
        } finally {
            logger.detachAppender(appender)
            appender.stop()
            root.deleteRecursively()
        }
        table.writeTo(dir)
    }
}
