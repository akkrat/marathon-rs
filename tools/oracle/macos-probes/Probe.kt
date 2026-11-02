import com.google.gson.Gson
import com.malinskiy.marathon.apple.macos.*
import java.io.File
import org.junit.jupiter.api.Test

fun main(args: Array<String>) {
    val root = File(args[0]).apply { mkdirs() }
    val gson = Gson()
    for (type in listOf(AppleMacosProviderTest::class.java, MacosDeviceBatchTest::class.java, MacosTestRootTest::class.java)) {
        for (method in type.declaredMethods.filter { it.isAnnotationPresent(Test::class.java) }.sortedBy { it.name }) {
            val test = type.getDeclaredConstructor().newInstance()
            for (field in type.declaredFields.filter { it.isAnnotationPresent(org.junit.jupiter.api.io.TempDir::class.java) }) {
                field.isAccessible = true
                field.set(test, File(root, type.simpleName + "/" + method.name.hashCode()).apply { mkdirs() })
            }
            try { method.invoke(test) } catch (e: java.lang.reflect.InvocationTargetException) { throw e.targetException }
            println("ROW=" + gson.toJson(linkedMapOf("class" to type.simpleName, "test" to method.name, "passed" to true)))
        }
    }
    supplemental(root)
}
