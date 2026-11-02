// The JDK side of examples/limits.rs: Kotlin's Regex.matches is Matcher.matches.
// Run: "$JAVA_HOME/bin/java" examples/LimitsProbe.java
import java.util.regex.Pattern;

public class LimitsProbe {
    public static void main(String[] args) {
        Object[][] cases = {
            {"(?:ab)*", "ab".repeat(100)},
            {"(?:ab)*", "ab".repeat(300)},
            {"(a|b)*", "ab".repeat(300)},
            {".*x", "a".repeat(10_000) + "x"},
            {"[a-z]+", "a".repeat(10_000)},
            {"(?:a+b)+", "aab".repeat(400)},
            {"^((?!Abstract).)*Test[s]*$", "A".repeat(600) + "Test"},
            {"com\\.example\\..*", "com.example." + "x".repeat(2_000)},
        };
        for (Object[] c : cases) {
            String p = (String) c[0], s = (String) c[1];
            System.out.printf("%-32s len %6d  Java %s%n", p, s.length(), Pattern.compile(p).matcher(s).matches());
        }
    }
}
