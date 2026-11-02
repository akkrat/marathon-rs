import com.google.gson.Gson
import com.malinskiy.marathon.apple.*
import java.io.File
import java.lang.reflect.InvocationTargetException
val gson = Gson()
fun row(value:Any) = println("ROW="+gson.toJson(value))
fun main(args:Array<String>) {
    for (type in listOf(AppleDeviceTestRunnerTest::class.java,XCTestParserTest::class.java,
        com.malinskiy.marathon.apple.ios.SkipCopyingDerivedDataTest::class.java,
        com.malinskiy.marathon.apple.ios.StartupPhasesTest::class.java)) {
        for ((index,method) in type.declaredMethods.filter { it.isAnnotationPresent(org.junit.jupiter.api.Test::class.java) &&
            (type.simpleName != "SkipCopyingDerivedDataTest" || it.name.startsWith("parsing ")) &&
            (type.simpleName != "StartupPhasesTest" || it.name.contains("symbol scan")) }.sortedBy { it.name }.withIndex()) {
            val test=type.getDeclaredConstructor().newInstance()
            if(test is XCTestParserTest) test.dir=File(args[0],index.toString()).apply { mkdirs() }
            if(test is com.malinskiy.marathon.apple.ios.SkipCopyingDerivedDataTest) test.root=File(args[0],"skip$index").apply {mkdirs()}
            if(test is com.malinskiy.marathon.apple.ios.StartupPhasesTest) test.root=File(args[0],"startup$index").apply {mkdirs()}
            try { method.invoke(test);row(linkedMapOf("test" to method.name,"passed" to true)) }
            catch(e:InvocationTargetException) { throw e.cause!! }
            finally { when(test) {
                is XCTestParserTest -> test.releaseSessions()
                is AppleDeviceTestRunnerTest -> test.releaseSessions()
                is com.malinskiy.marathon.apple.ios.SkipCopyingDerivedDataTest -> test.dispose()
                is com.malinskiy.marathon.apple.ios.StartupPhasesTest -> test.release()
            } }
        }
    }
    detailedCases(File(args[0]))

}
