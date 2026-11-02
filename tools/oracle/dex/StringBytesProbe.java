// WP 6.4 oracle: what dex-test-parser's ParseUtils.parseStringBytes returns
// for raw DEX string bytes. It decodes with `String(bytes)`, the default
// charset, which is UTF-8 on the reference JDK 21 (JEP 400). The probe refuses
// to run on any other default.
//
// Usage: java StringBytesProbe <out.txt>
// One line per byte sequence: "<hex bytes> <hex UTF-16 units, comma separated>".
// The corpus is every sequence of length 1-3 over an alphabet of byte classes,
// then 3000 seeded random sequences of length 4-6 over the same alphabet.
// It never contains 0x00, which terminates a DEX string.
import java.io.PrintWriter;
import java.nio.charset.Charset;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Random;

public class StringBytesProbe {
    static final int[] ALPHABET = {
        0x41, 0x80, 0x8f, 0x90, 0x9f, 0xa0, 0xbf, 0xc0, 0xc1, 0xc2,
        0xdf, 0xe0, 0xe1, 0xed, 0xef, 0xf0, 0xf1, 0xf4, 0xf5, 0xff,
    };

    public static void main(String[] args) throws Exception {
        if (!Charset.defaultCharset().equals(StandardCharsets.UTF_8)) {
            throw new IllegalStateException("default charset is " + Charset.defaultCharset() + ", expected UTF-8");
        }
        try (PrintWriter out = new PrintWriter(Files.newBufferedWriter(Path.of(args[0]), StandardCharsets.UTF_8))) {
            for (int length = 1; length <= 3; length++) {
                int[] index = new int[length];
                while (true) {
                    byte[] bytes = new byte[length];
                    for (int i = 0; i < length; i++) bytes[i] = (byte) ALPHABET[index[i]];
                    line(out, bytes);
                    int k = length - 1;
                    while (k >= 0 && ++index[k] == ALPHABET.length) index[k--] = 0;
                    if (k < 0) break;
                }
            }
            Random random = new Random(64);
            for (int n = 0; n < 3000; n++) {
                byte[] bytes = new byte[4 + random.nextInt(3)];
                for (int i = 0; i < bytes.length; i++) bytes[i] = (byte) ALPHABET[random.nextInt(ALPHABET.length)];
                line(out, bytes);
            }
        }
    }

    static void line(PrintWriter out, byte[] bytes) {
        StringBuilder sb = new StringBuilder();
        for (byte b : bytes) sb.append(String.format("%02x", b & 0xff));
        sb.append(' ');
        String decoded = new String(bytes);
        for (int i = 0; i < decoded.length(); i++) {
            if (i > 0) sb.append(',');
            sb.append(String.format("%04x", (int) decoded.charAt(i)));
        }
        out.print(sb);
        out.print('\n');
    }
}
