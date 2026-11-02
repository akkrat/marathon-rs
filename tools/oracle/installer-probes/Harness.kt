import com.google.gson.Gson
import com.malinskiy.marathon.apple.ios.SkipCopyingDerivedDataTest
import java.io.File
import java.lang.reflect.InvocationTargetException

// Execute the unchanged source tests, including their real simulator installer and cleanup.
fun main(args:Array<String>) {
    val results=mutableListOf<Any>()
    val methods=SkipCopyingDerivedDataTest::class.java.declaredMethods.filter {
        it.isAnnotationPresent(org.junit.jupiter.api.Test::class.java) && !it.name.startsWith("parsing")
    }.sortedBy { it.name }
    for((index,method) in methods.withIndex()) {
        val test=SkipCopyingDerivedDataTest()
        test.root=File(args[0],index.toString()).apply { mkdirs() }
        try {
            method.invoke(test)
            results.add(linkedMapOf("test" to method.name,"passed" to true))
        } catch(e:InvocationTargetException) { throw e.cause!! }
        finally { test.dispose() }
    }
    check(results.size==13)
    println("HARNESS="+Gson().toJson(results))
}
