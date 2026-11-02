import java.io.*;
import java.lang.reflect.*;
import java.nio.charset.StandardCharsets;
import java.util.*;
import java.util.function.IntPredicate;
import java.util.zip.GZIPOutputStream;

/** JDK 21 / Unicode 15.0 regex tables, generated from the reference implementation. */
public class RegexUnicode {
    static PrintWriter out;
    static Method test;
    static void ranges(String name, IntPredicate predicate) {
        out.print(name); out.print('\t');
        int start = -1;
        for (int c = 0; c <= 0x110000; c++) {
            boolean yes = c < 0x110000 && predicate.test(c);
            if (yes && start < 0) start = c;
            if (!yes && start >= 0) {
                out.print(Integer.toHexString(start)); out.print('-');
                out.print(Integer.toHexString(c - 1)); out.print(','); start = -1;
            }
        }
        out.println();
    }
    static Map<?,?> map(Class<?> cls, String name) throws Exception {
        Field f = cls.getDeclaredField(name); f.setAccessible(true); return (Map<?,?>)f.get(null);
    }
    public static void main(String[] args) throws Exception {
        if (Runtime.version().feature() != 21) throw new IllegalStateException("Requires JDK 21");
        out = new PrintWriter(new OutputStreamWriter(new GZIPOutputStream(new FileOutputStream(args[0])), StandardCharsets.UTF_8));
        out.println("JDK21-Unicode15.0");
        Class<?> predicates = Class.forName("java.util.regex.CharPredicates");
        test = Class.forName("java.util.regex.Pattern$CharPredicate").getDeclaredMethod("is", int.class); test.setAccessible(true);
        String general = "Cn Lu Ll Lt Lm Lo Mn Me Mc Nd Nl No Zs Zl Zp Cc Cf Co Cs Pd Ps Pe Pc Po Sm Sc Sk So Pi Pf L M N Z C P S LC LD L1 all ASCII Alnum Alpha Blank Cntrl Digit Graph Lower Print Punct Space Upper XDigit javaLowerCase javaUpperCase javaAlphabetic javaIdeographic javaTitleCase javaDigit javaDefined javaLetter javaLetterOrDigit javaSpaceChar javaWhitespace javaISOControl javaMirrored javaIdentifierIgnorable javaJavaIdentifierStart javaJavaIdentifierPart javaUnicodeIdentifierStart javaUnicodeIdentifierPart";
        String binary = "ALPHABETIC ASSIGNED CONTROL EMOJI EMOJI_PRESENTATION EMOJI_MODIFIER EMOJI_MODIFIER_BASE EMOJI_COMPONENT EXTENDED_PICTOGRAPHIC HEXDIGIT HEX_DIGIT IDEOGRAPHIC JOINCONTROL JOIN_CONTROL LETTER LOWERCASE NONCHARACTERCODEPOINT NONCHARACTER_CODE_POINT TITLECASE PUNCTUATION UPPERCASE WHITESPACE WHITE_SPACE WORD ALNUM BLANK GRAPH PRINT DIGIT";
        String posix = "ALPHA LOWER UPPER SPACE PUNCT XDIGIT ALNUM CNTRL DIGIT BLANK GRAPH PRINT";
        String[] names = {general, binary, posix};
        String[] methods = {"forProperty", "forUnicodeProperty", "forPOSIXName"};
        String[] prefixes = {"P", "U", "X"};
        for (int kind = 0; kind < names.length; kind++) {
            Method method = predicates.getDeclaredMethod(methods[kind], String.class, boolean.class); method.setAccessible(true);
            for (String name : names[kind].split(" ")) for (boolean ci : new boolean[]{false, true}) {
                Object predicate = method.invoke(null, name, ci);
                if (predicate != null) ranges(prefixes[kind] + (ci ? "1:" : "0:") + name, c -> {
                    try { return (boolean)test.invoke(predicate, c); } catch (Exception e) { throw new RuntimeException(e); }
                });
            }
        }
        for (Character.UnicodeScript script : Character.UnicodeScript.values())
            ranges("S:" + script, c -> Character.UnicodeScript.of(c) == script);
        TreeMap<String,String> scripts = new TreeMap<>();
        for (var entry : map(Character.UnicodeScript.class, "aliases").entrySet()) scripts.put(entry.getKey().toString(), entry.getValue().toString());
        for (var entry : scripts.entrySet()) out.println("s:" + entry.getKey() + "\t" + entry.getValue());
        TreeMap<String,Character.UnicodeBlock> blocks = new TreeMap<>();
        for (var entry : map(Character.UnicodeBlock.class, "map").entrySet()) blocks.put(entry.getKey().toString(), (Character.UnicodeBlock)entry.getValue());
        Set<Character.UnicodeBlock> seen = new HashSet<>();
        for (var entry : blocks.entrySet()) {
            out.println("b:" + entry.getKey() + "\t" + entry.getValue());
            if (seen.add(entry.getValue())) ranges("B:" + entry.getValue(), c -> Character.UnicodeBlock.of(c) == entry.getValue());
        }
        for (int c = 0; c <= 0x10ffff; c++) {
            int upper = Character.toUpperCase(c), lower = Character.toLowerCase(c);
            if (upper != c || lower != c) out.println("C:" + c + "\t" + upper + "," + lower);
            String name = Character.getName(c);
            if (name != null) out.println("N:" + name + "\t" + c);
        }
        out.close();
    }
}
