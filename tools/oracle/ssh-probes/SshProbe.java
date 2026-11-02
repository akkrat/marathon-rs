// WP 4.2 oracle: the actual Kotlin shellCommand and sshj 0.39.0's OpenSSHKnownHosts/connect failures.
// Run through run.sh; the classpath is the Kotlin checkout's installed CLI distribution (read-only).
import com.malinskiy.marathon.apple.cmd.remote.ssh.sshj.SshjCommandExecutor;
import com.malinskiy.marathon.apple.cmd.remote.ssh.sshj.SshjCommandExecutorFactory;
import com.malinskiy.marathon.apple.cmd.remote.ssh.sshj.SshjCommandExecutorKt;
import com.malinskiy.marathon.apple.cmd.remote.ssh.sshj.auth.SshAuthentication;
import net.schmizz.sshj.SSHClient;
import net.schmizz.sshj.common.Buffer;
import net.schmizz.sshj.common.KeyType;
import net.schmizz.sshj.userauth.keyprovider.KeyProvider;
import net.schmizz.sshj.common.SSHException;
import net.schmizz.sshj.common.SecurityUtils;
import net.schmizz.sshj.connection.channel.direct.Session;
import net.schmizz.sshj.transport.verification.PromiscuousVerifier;
import net.schmizz.sshj.transport.verification.OpenSSHKnownHosts;

import java.nio.file.Files;
import java.nio.file.Path;
import java.security.PublicKey;
import java.util.ArrayList;
import java.util.Base64;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

public class SshProbe {
    public static void main(String[] args) throws Exception {
        switch (args[0]) {
            case "shell" -> shell();
            case "known-hosts" -> knownHosts(Path.of(args[1]));
            case "connect" -> connect(Integer.parseInt(args[1]), Path.of(args[2]), args[3]);
            case "keys" -> keys(java.util.Arrays.copyOfRange(args, 1, args.length));
            case "flood" -> flood(Integer.parseInt(args[1]));
            default -> throw new IllegalArgumentException(args[0]);
        }
    }

    static String json(Object value) {
        if (value == null) return "null";
        if (value instanceof Boolean || value instanceof Number) return value.toString();
        if (value instanceof List<?> list) {
            List<String> items = new ArrayList<>();
            for (Object item : list) items.add(json(item));
            return "[" + String.join(",", items) + "]";
        }
        if (value instanceof Map<?, ?> map) {
            List<String> items = new ArrayList<>();
            for (Map.Entry<?, ?> e : map.entrySet()) items.add(json(e.getKey().toString()) + ":" + json(e.getValue()));
            return "{" + String.join(",", items) + "}";
        }
        StringBuilder sb = new StringBuilder("\"");
        for (char c : value.toString().toCharArray()) {
            switch (c) {
                case '"' -> sb.append("\\\"");
                case '\\' -> sb.append("\\\\");
                case '\n' -> sb.append("\\n");
                case '\r' -> sb.append("\\r");
                case '\t' -> sb.append("\\t");
                default -> { if (c < 0x20) sb.append(String.format("\\u%04x", (int) c)); else sb.append(c); }
            }
        }
        return sb.append('"').toString();
    }

    static Map<String, Object> row(Object... pairs) {
        Map<String, Object> map = new LinkedHashMap<>();
        for (int i = 0; i < pairs.length; i += 2) map.put((String) pairs[i], pairs[i + 1]);
        return map;
    }

    static void shell() {
        String[] t03 = {"plain", "a b", "$HOME", "it's", "'already quoted'", "\"double quoted\"", "FOO=bar",
            "/p-a.t,h:+@x", "'half", "half'", "'mixed\"", "a\"b", "~", "semi;colon", "", "tab\tx", "back\\slash",
            "star*", "$(echo injected)"};
        for (String arg : t03) {
            List<String> command = List.of("printf", "<%s>\\n", arg);
            System.out.println(json(row("case", "T03", "command", command, "wire", SshjCommandExecutorKt.shellCommand(command))));
        }
        String[][] extra = {{"'"}, {"\""}, {"''"}, {"\"'"}, {"'\""}, {"x'y"}, {"&"}, {"sleep", "100000", "&"},
            {"sh", "-c", "echo start; seq 1 100000 1>&2; echo end"}, {"echo", "Hello world"}, {"a", "", "b"},
            {"dollar$", "#hash", "[x]", "!bang", "a>b", "a<b", "a|b", "{x}", "(y)", "?q", "~/t"}, {"ñ ü"}};
        for (String[] command : extra) {
            System.out.println(json(row("case", "extra", "command", List.of(command), "wire", SshjCommandExecutorKt.shellCommand(List.of(command)))));
        }
    }

    static PublicKey readKey(Path pub) throws Exception {
        String[] parts = Files.readString(pub).trim().split(" ");
        return new Buffer.PlainBuffer(Base64.getDecoder().decode(parts[1])).readPublicKey();
    }

    static String line(Path pub) throws Exception {
        String[] parts = Files.readString(pub).trim().split(" ");
        return parts[0] + " " + parts[1];
    }

    static String hashed(String host) throws Exception {
        byte[] salt = new byte[20];
        for (int i = 0; i < salt.length; i++) salt[i] = (byte) (i * 7 + 3);
        javax.crypto.Mac mac = javax.crypto.Mac.getInstance("HmacSHA1");
        mac.init(new javax.crypto.spec.SecretKeySpec(salt, "HmacSHA1"));
        return "|1|" + Base64.getEncoder().encodeToString(salt) + "|" + Base64.getEncoder().encodeToString(mac.doFinal(host.getBytes()));
    }

    static void knownHosts(Path keys) throws Exception {
        String ed = line(keys.resolve("ed25519.pub")), other = line(keys.resolve("other.pub"));
        String rsa = line(keys.resolve("rsa.pub")), ecdsa = line(keys.resolve("ecdsa.pub"));
        String edType = ed.split(" ")[0], edKey = ed.split(" ")[1];
        String[][] cases = {
            {"plain", "localhost " + ed},
            {"plain_port", "[localhost]:2222 " + ed},
            {"hashed", hashed("[localhost]:2222") + " " + ed},
            {"hashed_bad_format", "|1|abc " + ed},
            {"hashed_bad_salt", "|1|***|xyz " + ed},
            {"wrong_key", "[localhost]:2222 " + other},
            {"unknown_host", "[otherhost]:2222 " + ed},
            {"other_algorithm_only", "[localhost]:2222 " + rsa},
            {"empty", ""},
            {"malformed_first", "[localhost]:2222 ssh-ed25519 !!!!\n[localhost]:2222 " + ed},
            {"differing_before", "[localhost]:2222 " + other + "\n[localhost]:2222 " + ed},
            {"differing_after", "[localhost]:2222 " + ed + "\n[localhost]:2222 " + other},
            {"tabs_and_spaces", "[localhost]:2222\t \t" + edType + "   " + edKey + "  comment here"},
            {"comma_list", "foo,[localhost]:2222,bar " + ed},
            {"wildcard_question", "[localhos?]:2222 " + ed},
            {"wildcard_star", "* " + ed},
            {"wildcard_star_port", "[local*]:2222 " + ed},
            {"negation_list", "![127.0.0.1]:2222,* " + ed},
            {"negation_alone", "![otherhost]:2222 " + ed},
            {"negation_self", "![localhost]:2222 " + ed},
            {"uppercase_entry", "[LOCALHOST]:2222 " + ed},
            {"uppercase_host", "[localhost]:2222 " + ed},
            {"comment_and_blank", "# comment\n\n   \n[localhost]:2222 " + ed},
            {"too_few_fields", "[localhost]:2222 " + edType + "\n[localhost]:2222 " + ed},
            {"unknown_type", "[localhost]:2222 ssh-foo AAAA\n"},
            {"bits_rsa1", "[localhost]:2222 2048 65537 12345"},
            {"revoked", "@revoked [localhost]:2222 " + ed},
            {"revoked_then_plain", "@revoked [localhost]:2222 " + ed + "\n[localhost]:2222 " + ed},
            {"cert_authority", "@cert-authority [localhost]:2222 " + ed},
            {"unknown_marker", "@foo [localhost]:2222 " + ed},
            {"crlf", "[localhost]:2222 " + other + "\r\n[localhost]:2222 " + ed + "\r\n"},
            {"cr_only", "[localhost]:2222 " + ed + "\r"},
            {"leading_space", "   [localhost]:2222 " + ed},
            {"port_22", "localhost " + ed},
            {"trailing_comma", "[localhost]:2222, " + ed},
            {"ecdsa_and_ed", "[localhost]:2222 " + ecdsa + "\n[localhost]:2222 " + ed},
            {"padding_stripped", "[localhost]:2222 " + edType + " " + edKey.replace("=", "")},
            // WildcardHostMatcher escapes only [ ] . before Pattern.compile: other regex syntax stays live.
            {"wildcard_regex_group", "local(host|x)* " + ed},
            {"wildcard_regex_group_port", "[loc(a|x)l*]:2222 " + ed},
            {"wildcard_regex_plus", "l+ocalhos? " + ed},
            {"wildcard_regex_invalid", "localhost " + ed + "\nlocal(host* " + ed},
        };
        PublicKey key = readKey(keys.resolve("ed25519.pub"));
        Path file = keys.resolve("known_hosts");
        for (String[] c : cases) {
            Files.writeString(file, c[1]);
            OpenSSHKnownHosts known;
            try {
                known = new OpenSSHKnownHosts(file.toFile());
            } catch (RuntimeException e) {
                System.out.println(json(row("case", c[0], "known_hosts", c[1], "key", ed, "error", e.getClass().getName())));
                continue;
            }
            for (Object[] target : new Object[][]{{"localhost", 2222}, {"LocalHost", 2222}, {"localhost", 22}, {"127.0.0.1", 2222}}) {
                String host = (String) target[0];
                int port = (Integer) target[1];
                System.out.println(json(row("case", c[0], "known_hosts", c[1], "host", host, "port", port, "key", ed,
                    "verify", known.verify(host, port, key), "algorithms", known.findExistingAlgorithms(host, port))));
            }
        }
        System.out.println(json(row("case", "missing_file", "known_hosts", null, "host", "localhost", "port", 2222, "key", ed,
            "verify", new OpenSSHKnownHosts(keys.resolve("absent").toFile()).verify("localhost", 2222, key), "algorithms", List.of())));
        System.out.println(json(row("case", "fingerprint", "key", ed, "fingerprint", SecurityUtils.getFingerprint(key))));
    }

    static List<Object> chain(Throwable t) {
        List<Object> out = new ArrayList<>();
        for (Throwable c = t; c != null; c = c.getCause()) {
            Map<String, Object> m = row("class", c.getClass().getName(), "message", c.getMessage(), "toString", c.toString());
            if (c instanceof SSHException s) m.put("reason", s.getDisconnectReason().toString());
            out.add(m);
        }
        return out;
    }

    static void connect(int port, Path knownHosts, String hostKey) throws Exception {
        try {
            new SshjCommandExecutorFactory().connect("127.0.0.1", port,
                new SshAuthentication.PasswordAuthentication("fixture", "fake-password"),
                new OpenSSHKnownHosts(knownHosts.toFile()), false);
            System.out.println(json(row("case", "connect", "result", "connected")));
        } catch (Throwable t) {
            PublicKey key = readKey(Path.of(hostKey));
            System.out.println(json(row("case", "connect", "known_hosts", Files.exists(knownHosts) ? Files.readString(knownHosts) : null,
                "host_key", line(Path.of(hostKey)), "fingerprint", SecurityUtils.getFingerprint(key), "error", chain(t))));
        }
    }

    // SshAuthentication.PublicKeyAuthentication: loadKeys(absolutePath) without a password finder, then
    // authPublickey asks the provider for the private key (the public half comes from the same provider).
    static void keys(String[] files) throws Exception {
        for (String name : files) {
            Path file = Path.of(name);
            Map<String, Object> r = row("case", name);
            try {
                KeyProvider provider = new SSHClient().loadKeys(file.toAbsolutePath().toString());
                try {
                    PublicKey key = provider.getPublic();
                    r.put("type", KeyType.fromKey(key).toString());
                    r.put("fingerprint", SecurityUtils.getFingerprint(key));
                } catch (Throwable t) { r.put("public_error", chain(t)); }
                try { provider.getPrivate(); r.put("private", "loaded"); } catch (Throwable t) { r.put("private_error", chain(t)); }
            } catch (Throwable t) { r.put("load_error", chain(t)); }
            System.out.println(json(r));
        }
    }

    // R2-sol-1: the fake's flood (32 x 1 MiB stdout + one stderr byte) read through stderr only, one byte per
    // 100 ms, on the Kotlin factory's client (its private `client`, reached by reflection).
    static void flood(int port) throws Exception {
        SshjCommandExecutor executor = new SshjCommandExecutorFactory().connect("127.0.0.1", port,
            new SshAuthentication.PasswordAuthentication("fixture", "fake-password"), new PromiscuousVerifier(), false);
        java.lang.reflect.Field field = SshjCommandExecutor.class.getDeclaredField("client");
        field.setAccessible(true);
        SSHClient client = (SSHClient) field.get(executor);
        Session.Command command = client.startSession().exec("flood");
        Map<String, Object> r = row();
        int consumed = 0;
        try {
            // Sleep first, as in Sol's witness: a read that returns while the window is still above sshj's
            // adjust threshold grants no credit, and the next read then waits forever.
            for (int i = 0; i < 32; i++) {
                Thread.sleep(100);
                if (command.getErrorStream().read() < 0) break;
                consumed++;
            }
        } catch (Throwable t) { r.put("read_error", chain(t)); }
        Thread.sleep(200);
        r.put("stderr_consumed", consumed);
        r.put("stdout_buffered", command.getInputStream().available());
        r.put("connected", client.isConnected());
        try { command.join(1, java.util.concurrent.TimeUnit.SECONDS); r.put("join", "closed"); }
        catch (Throwable t) { r.put("join_error", chain(t)); }
        System.out.println(json(r));
        client.close();
    }
}
