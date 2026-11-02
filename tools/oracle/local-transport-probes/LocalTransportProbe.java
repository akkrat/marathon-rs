// Reference JDK/Kotlin stdlib edge probes for WP 4.1; run with the stdlib on -cp.
import java.nio.file.*;
import java.nio.charset.StandardCharsets;
import java.time.Duration;
import java.io.File;
import kotlin.io.FilesKt;
import kotlin.text.StringsKt;

class LocalTransportProbe {
    static void copy(String label, Path src, Path dst, boolean overwrite) {
        try {
            var method = FilesKt.class.getMethod("copyRecursively$default", File.class,
                File.class, boolean.class, kotlin.jvm.functions.Function2.class, int.class, Object.class);
            method.setAccessible(true);
            System.out.println(label + "=" + method.invoke(null, src.toFile(), dst.toFile(), overwrite, null, 4, null));
        } catch (Exception e) {
            var cause = e.getCause() == null ? e : e.getCause();
            System.out.println(label + "=" + cause.getClass().getSimpleName());
        }
    }
    public static void main(String[] args) throws Exception {
        Path root = Path.of(args[0]); Files.createDirectories(root);
        for (String s : new String[]{"", "'", "\"", "''", "\"'x'\"", "x'"}) {
            String r = StringsKt.removeSurrounding(StringsKt.removeSurrounding(s, "\""), "'");
            System.out.println("strip[" + s + "]=[" + r + "]");
        }
        for (boolean force : new boolean[]{false, true}) {
            var p = new ProcessBuilder("sleep", "100").start();
            if (force) p.destroyForcibly(); else p.destroy();
            System.out.println("signal[" + force + "]=" + p.waitFor());
        }
        System.out.println("split=" + new String(new byte[]{(byte)0xc3}, StandardCharsets.UTF_8)
            + new String(new byte[]{(byte)0xa9}, StandardCharsets.UTF_8));
        for (Duration d : new Duration[]{Duration.ofMillis(Long.MAX_VALUE), Duration.ofNanos(-1), Duration.ofSeconds(Long.MAX_VALUE)}) {
            try { System.out.println("millis[" + d + "]=" + d.toMillis()); }
            catch (ArithmeticException e) { System.out.println("millis[" + d + "]=ArithmeticException"); }
        }
        Path binary = root.resolve("path-probe");
        Files.writeString(binary, "#!/bin/sh\necho found\n"); binary.toFile().setExecutable(true);
        var builder = new ProcessBuilder("path-probe");
        builder.environment().put("PATH", root.toAbsolutePath().toString());
        try { System.out.println("child-env-path-lookup=" + builder.start().waitFor()); }
        catch (java.io.IOException e) { System.out.println("child-env-path-lookup=IOException"); }
        var inherited = new ProcessBuilder("echo", "parent");
        inherited.environment().put("PATH", root.toAbsolutePath().toString());
        System.out.println("parent-path-lookup=" + inherited.start().waitFor());
        Path src = root.resolve("src"), dst = root.resolve("dst"), dir = root.resolve("dir");
        Files.writeString(src, "new"); Files.writeString(dst, "old"); Files.createDirectory(dir);
        Files.writeString(dir.resolve("child"), "child");
        copy("missing", root.resolve("missing"), dst, true);
        copy("no-overwrite", src, dst, false);
        src.toFile().setExecutable(true);
        copy("overwrite", src, dst, true);
        System.out.println("copied-executable=" + dst.toFile().canExecute());
        String diagnostic = "command false failed with exit code 1:\n                   stdout: out\n                   stderr: err\n                ";
        System.out.println("critical=" + StringsKt.trimMargin(diagnostic, "|").replace(" ", "·").replace("\n", "\\n"));
        copy("directory-onto-file", dir, dst, true);
        copy("file-onto-directory", src, dst, true);
        copy("parent-is-file", src, src.resolve("child"), true);
        System.out.println("empty-child=" + new File(dir.toFile(), "").getPath());
        FilesKt.deleteRecursively(root.toFile());
    }
}
