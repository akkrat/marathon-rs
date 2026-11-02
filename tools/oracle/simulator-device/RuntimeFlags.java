/** Compare production JVM defaults with Gradle's assertion-enabled test runtime. */
public class RuntimeFlags {
    public static void main(String[] args) throws Exception {
        Class<?> flags = Class.forName("kotlinx.coroutines.DebugKt");
        System.out.printf("{\"assertions\":%b,\"debug\":%s,\"recoverStackTraces\":%s}%n",
                RuntimeFlags.class.desiredAssertionStatus(),
                flags.getMethod("getDEBUG").invoke(null),
                flags.getMethod("getRECOVER_STACK_TRACES").invoke(null));
    }
}
