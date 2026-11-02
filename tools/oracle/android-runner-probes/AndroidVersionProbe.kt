// AndroidVersion from the unchanged marathon e47352f2 distribution; no adb.
import com.android.sdklib.AndroidVersion
import com.google.gson.Gson

fun main() {
    val version = AndroidVersion(29)
    println(Gson().toJson(linkedMapOf(
        "case" to "android-version:29-equals-int",
        "value" to version.equals(29)
    )))
    println(Gson().toJson(linkedMapOf(
        "case" to "android-version:28-equals-int-29",
        "value" to AndroidVersion(28).equals(29)
    )))
}
