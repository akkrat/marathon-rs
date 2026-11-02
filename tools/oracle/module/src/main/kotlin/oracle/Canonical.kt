package oracle

import com.fasterxml.jackson.annotation.JsonSubTypes
import java.io.File
import java.lang.reflect.Modifier
import java.math.BigDecimal
import java.math.BigInteger
import java.nio.file.Path
import java.time.Duration
import java.time.Instant
import java.util.IdentityHashMap
import java.util.TreeMap
import java.util.regex.Pattern

/**
 * The canonical JSON form of a parsed configuration (docs/design/config-dump-format.md).
 *
 * Objects are rendered from their JVM fields, not from Jackson's view of them, so that values hidden by `@JsonIgnore` (the
 * resolved Apple bundle paths) and derived defaults appear too. Keys are sorted; polymorphic objects get a `type` key with the
 * name Jackson binds them by.
 */
object Canonical {
    fun render(value: Any?): Any? = Renderer().render(value)

    private class Renderer {
        private val path = IdentityHashMap<Any, Unit>()

        fun render(v: Any?): Any? = when (v) {
            null -> null
            is String -> v
            is Boolean -> v
            is Int, is Long, is Short, is Byte, is BigInteger -> v
            is Double -> jsonDouble(v)
            is Float -> if (v.isFinite()) JRaw(v.toString()) else v.toString()
            is BigDecimal -> JRaw(v.toString())
            is Char -> v.toString()
            is Enum<*> -> v.name
            is Duration -> v.toString()
            is Instant -> v.toString()
            is File -> v.absolutePath
            is Path -> v.toAbsolutePath().toString()
            is Regex -> v.pattern
            is Pattern -> v.pattern()
            is Map<*, *> -> nested(v) { TreeMap<String, Any?>().apply { v.forEach { (k, x) -> put(k.toString(), render(x)) } } }
            is Collection<*> -> nested(v) { v.map { render(it) } }
            is Array<*> -> nested(v) { v.map { render(it) } }
            is ByteArray -> java.util.Base64.getEncoder().encodeToString(v)
            is Lazy<*> -> if (v.isInitialized()) render(v.value) else mapOf("lazy" to "uninitialized")
            else -> nested(v) { obj(v) }
        }

        private fun <T> nested(v: Any, block: () -> T): T {
            require(path.put(v, Unit) == null) { "cycle through ${v.javaClass.name}" }
            try {
                return block()
            } finally {
                path.remove(v)
            }
        }

        private fun obj(v: Any): Map<String, Any?> {
            val out = TreeMap<String, Any?>()
            var c: Class<*>? = v.javaClass
            while (c != null && c != Any::class.java) {
                for (f in c.declaredFields) {
                    if (Modifier.isStatic(f.modifiers) || f.isSynthetic) continue
                    f.isAccessible = true
                    val x = f.get(v)
                    // Lambdas such as AppleTestBundleConfiguration.tempDirFor are behaviour, not configuration.
                    if (x is Function<*> || f.type.name.startsWith("kotlin.jvm.functions.")) continue
                    val name = f.name.removeSuffix("\$delegate")
                    // A subclass field that overrides a superclass property shadows the superclass's backing field.
                    if (out.containsKey(name)) continue
                    out[name] = render(x)
                }
                c = c.superclass
            }
            typeName(v.javaClass)?.let {
                require(!out.containsKey("type")) { "${v.javaClass.name} has both a type discriminator and a type field" }
                out["type"] = it
            }
            return out
        }
    }

    /** The first name under which a supertype's `@JsonSubTypes` lists [cls], or null when it is not polymorphic. */
    fun typeName(cls: Class<*>): String? {
        val supers = generateSequence(cls.superclass) { it.superclass } + cls.interfaces.asSequence()
        for (s in supers) {
            val subTypes = s.getAnnotation(JsonSubTypes::class.java) ?: continue
            for (t in subTypes.value) {
                if (t.value.java == cls) {
                    return t.name.ifEmpty { t.names.firstOrNull() ?: cls.simpleName }
                }
            }
        }
        return null
    }
}
