import com.malinskiy.marathon.apple.extensions.ReaderKt;
import kotlin.coroutines.Continuation;
import kotlin.coroutines.EmptyCoroutineContext;
import kotlin.jvm.functions.Function2;
import kotlinx.coroutines.BuildersKt;
import kotlinx.coroutines.CoroutineScope;
import kotlinx.coroutines.Job;
import kotlinx.coroutines.SupervisorKt;
import kotlinx.coroutines.channels.ChannelsKt;
import kotlinx.coroutines.channels.ReceiveChannel;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import java.time.Duration;
import java.util.Arrays;
import java.util.List;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.concurrent.atomic.AtomicInteger;
import java.util.concurrent.atomic.AtomicLong;

/**
 * U145: runs Kotlin's own produceLinesManually (Reader.kt) on a stream that receives the command's whole stdout,
 * followed by EOF, between the poll that found available() == 0 and the canRead() call that reads isEOF.
 */
public class ReaderRaceProbe {
    /** Stands in for sshj's ChannelInputStream: available() is the buffered byte count. */
    static class Stream extends InputStream {
        private byte[] buffer = new byte[0];
        private int position = 0;

        synchronized void deliver(String text) {
            byte[] bytes = text.getBytes(StandardCharsets.UTF_8);
            byte[] rest = Arrays.copyOfRange(buffer, position, buffer.length);
            buffer = Arrays.copyOf(rest, rest.length + bytes.length);
            System.arraycopy(bytes, 0, buffer, rest.length, bytes.length);
            position = 0;
        }

        @Override public synchronized int available() { return buffer.length - position; }

        @Override public synchronized int read() { return position < buffer.length ? buffer[position++] & 0xff : -1; }

        @Override public synchronized int read(byte[] target, int offset, int length) {
            int count = Math.min(length, buffer.length - position);
            if (count <= 0) return -1;
            System.arraycopy(buffer, position, target, offset, count);
            position += count;
            return count;
        }
    }

    @SuppressWarnings({"unchecked", "rawtypes"})
    static List<String> run(boolean beforeFirstPoll) throws Exception {
        String output = "HOME=/Users/worker\nUSER=worker\n";
        Stream input = new Stream();
        AtomicBoolean eof = new AtomicBoolean(false);
        AtomicInteger canReadCalls = new AtomicInteger();
        if (beforeFirstPoll) {
            input.deliver(output);
            eof.set(true);
        }
        Job job = SupervisorKt.SupervisorJob(null);
        Function2 body = (Object scope, Object continuation) -> {
            ReceiveChannel<String> lines = ReaderKt.produceLinesManually((CoroutineScope) scope, job, input,
                new AtomicLong(System.currentTimeMillis()), Duration.ofSeconds(60), StandardCharsets.UTF_8, 16,
                System::currentTimeMillis, () -> {
                    // sshj's reader thread delivers the stdout and then EOF here.
                    if (canReadCalls.getAndIncrement() == 0 && !beforeFirstPoll) {
                        input.deliver(output);
                        eof.set(true);
                    }
                    return !eof.get();
                });
            return ChannelsKt.toList((ReceiveChannel) lines, (Continuation) continuation);
        };
        return (List<String>) BuildersKt.runBlocking(EmptyCoroutineContext.INSTANCE, body);
    }

    public static void main(String[] args) throws Exception {
        System.out.println("stdout and EOF before the first poll: " + run(true));
        System.out.println("stdout and EOF between available() and canRead(): " + run(false));
    }
}
