package com.malinskiy.marathon.apple.xctestrun

import com.google.gson.GsonBuilder
import org.junit.jupiter.api.Test
import java.io.File
import java.nio.file.Files
import java.util.Base64

object CoverageRecorder {
    val calls = mutableListOf<Any>()
    fun remote(file: File, roots: List<Pair<File, String>>, directory: String, body: () -> CodeCoverageTargets.Remote): CodeCoverageTargets.Remote {
        val input = linkedMapOf<String, Any?>("xctestrun" to file.path, "roots" to roots.map { listOf(it.first.path, it.second) }, "directory" to directory)
        try {
            val result = body()
            input["infos"] = result.infos.map { it.delegate.toJavaObject() }
            input["staticLibraries"] = result.staticLibraries.map { listOf(it.first.path, it.second) }
            calls.add(input)
            return result
        } catch (error: Exception) {
            input["error"] = linkedMapOf("class" to error.javaClass.name, "message" to error.message)
            calls.add(input)
            throw error
        }
    }
}
fun main(args: Array<String>) {
    val gson = GsonBuilder().serializeNulls().disableHtmlEscaping().create()
    val output = File(args[0])
    output.writeText("")
    val methods = CodeCoverageTargetsTest::class.java.declaredMethods.filter { it.isAnnotationPresent(Test::class.java) }.sortedBy { it.name }
    for (method in methods) {
        val root = Files.createTempDirectory("mrs-coverage-").toFile()
        try {
            CoverageRecorder.calls.clear()
            val test = CodeCoverageTargetsTest().apply { this.root = root }
            method.invoke(test)
            val paths = Files.walk(root.toPath()).use { stream -> stream.map { it.toFile() }.toList() }
            val files = paths.filter { it != root }.map { file ->
                val path = file.relativeTo(root).invariantSeparatorsPath
                when {
                    Files.isSymbolicLink(file.toPath()) -> linkedMapOf("path" to path, "symlink" to Files.readSymbolicLink(file.toPath()).toString())
                    file.isDirectory -> linkedMapOf("path" to path, "directory" to true)
                    else -> linkedMapOf("path" to path, "bytes" to Base64.getEncoder().encodeToString(if (file.extension == "xctestrun") file.readText().replace(root.path, "<root>").toByteArray() else file.readBytes()))
                }
            }.sortedBy { it["path"].toString() }.toList()
            val mach = paths.filter { it.isFile && !Files.isSymbolicLink(it.toPath()) }.sortedBy { it.path }.map { file ->
                linkedMapOf("path" to file.relativeTo(root).invariantSeparatorsPath, "static" to MachO.isStatic(file), "slices" to MachO.slices(file).map { linkedMapOf("architecture" to it.architecture, "uuid" to it.uuid) })
            }.toList()
            output.appendText(gson.toJson(linkedMapOf("test" to method.name, "files" to files, "calls" to CoverageRecorder.calls, "mach" to mach)).replace(root.path, "<root>") + "\n")
        } finally { root.deleteRecursively() }
    }
    val companion = CodeCoverageTargets::class.java.getDeclaredField("Companion").apply { isAccessible = true }.get(null)
    val isBundle = companion.javaClass.getDeclaredMethod("isBundle", File::class.java).apply { isAccessible = true }
    val extensions = listOf(".app", ".framework", "Kit.framework", "App.APP", "App.", "App", ".xctest", "a/.bundle/", "/", ".")
    File(output.parentFile, "extensions.jsonl").writeText(extensions.joinToString("") { path -> gson.toJson(linkedMapOf("path" to path, "bundle" to isBundle.invoke(companion, File(path)))) + "\n" })
    println("${methods.size} original Kotlin tests passed")
}
