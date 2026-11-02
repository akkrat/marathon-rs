// Actual frozen Kotlin command layer, using its built CLI distribution read-only.
import com.malinskiy.marathon.apple.cmd.CommandSession;
import com.malinskiy.marathon.apple.cmd.local.KotlinProcessCommandExecutor;
import kotlinx.coroutines.BuildersKt;
import kotlin.coroutines.EmptyCoroutineContext;
import java.time.Duration;
import java.nio.charset.StandardCharsets;
import java.util.List;
import java.util.Map;

class KotlinCommandProbe {
    public static void main(String[] args) throws Exception {
        var executor = new KotlinProcessCommandExecutor(false, 64);
        CommandSession timeout = BuildersKt.runBlocking(EmptyCoroutineContext.INSTANCE,
            (scope, continuation) -> executor.execute(List.of("sleep", "100"), Duration.ofMillis(10),
                Duration.ofSeconds(1), Map.of(), null, StandardCharsets.UTF_8, continuation));
        try {
            com.malinskiy.marathon.apple.cmd.CommandResult result = BuildersKt.runBlocking(EmptyCoroutineContext.INSTANCE, (scope, continuation) -> timeout.await(continuation));
            System.out.println("total-timeout-exit=" + result.getExitCode() + ", alive=" + timeout.isAlive());
        } finally { timeout.close(); }
        CommandSession idle = BuildersKt.runBlocking(EmptyCoroutineContext.INSTANCE,
            (scope, continuation) -> executor.execute(List.of("sleep", "100"), Duration.ofSeconds(5),
                Duration.ofMillis(20), Map.of(), null, StandardCharsets.UTF_8, continuation));
        try {
            BuildersKt.runBlocking(EmptyCoroutineContext.INSTANCE, (scope, continuation) -> idle.await(continuation));
        } catch (Exception e) {
            System.out.println("idle-await=" + e.getClass().getSimpleName() + ":" + e.getMessage());
            System.out.println("idle-cause=" + e.getCause().getClass().getSimpleName() + ":" + e.getCause().getMessage());
        } finally { idle.close(); executor.close(); }
    }
}
