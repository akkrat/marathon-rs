import java.io.File;
import java.nio.file.*;
public class CanonProbe {
    public static void main(String[] a) throws Exception {
        Path d = Files.createTempDirectory("canon");
        Files.createSymbolicLink(d.resolve("loop"), Paths.get("loop"));
        Files.createSymbolicLink(d.resolve("a"), Paths.get("b"));
        Files.createSymbolicLink(d.resolve("b"), Paths.get("a"));
        System.out.println("java " + System.getProperty("java.vm.name") + " " + System.getProperty("java.runtime.version") + " " + System.getProperty("os.name"));
        for (String rel : new String[]{"loop", "loop/Marathondevices", "a/x/y", "a", "loop/.."}) {
            File f = new File(d.toFile(), rel);
            try {
                System.out.println(rel + " -> " + d.relativize(f.getCanonicalFile().toPath()));
            } catch (Exception e) {
                System.out.println(rel + " -> " + e.getClass().getName() + ": " + e.getMessage());
            }
        }
    }
}
