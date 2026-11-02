// WP 6.4 oracle: dumps dex-test-parser 2.3.4 DexParser.findTestMethods and the
// reference ApkParser (compiled unchanged from the Kotlin repo) over APK files
// as JSONL, for the Rust differential tests. Run via dex-oracle.sh.
//
// Usage: java DexOracle <out.jsonl> <apk[:customAnnotations=comma,list]>...
// One JSON object per APK; failures are recorded as {"error": {...}} with the
// JVM exception class chain instead of aborting.
import com.google.gson.Gson;
import com.google.gson.GsonBuilder;
import com.google.gson.JsonArray;
import com.google.gson.JsonObject;
import com.linkedin.dex.parser.DecodedValue;
import com.linkedin.dex.parser.DexParser;
import com.linkedin.dex.parser.TestAnnotation;
import com.linkedin.dex.parser.TestMethod;
import com.malinskiy.marathon.android.ApkParser;
import com.malinskiy.marathon.android.InstrumentationInfo;

import java.io.File;
import java.io.PrintWriter;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.IdentityHashMap;
import java.util.List;
import java.util.Map;

public class DexOracle {
    public static void main(String[] args) throws Exception {
        // ParseUtils decodes DEX strings with String(bytes): the recorded
        // values assume the reference JDK's UTF-8 default (JEP 400).
        if (!java.nio.charset.Charset.defaultCharset().equals(StandardCharsets.UTF_8)) {
            throw new IllegalStateException("default charset is " + java.nio.charset.Charset.defaultCharset());
        }
        Gson gson = new GsonBuilder().disableHtmlEscaping().create();
        boolean append = new File(args[0]).exists();
        try (PrintWriter out = new PrintWriter(java.nio.file.Files.newBufferedWriter(
                new File(args[0]).toPath(), StandardCharsets.UTF_8,
                java.nio.file.StandardOpenOption.CREATE,
                append ? java.nio.file.StandardOpenOption.APPEND : java.nio.file.StandardOpenOption.TRUNCATE_EXISTING))) {
            for (int i = 1; i < args.length; i++) {
                String spec = args[i];
                String path = spec;
                List<String> custom = List.of();
                int marker = spec.indexOf(":customAnnotations=");
                if (marker >= 0) {
                    path = spec.substring(0, marker);
                    String list = spec.substring(marker + ":customAnnotations=".length());
                    custom = List.of(list.split(",", -1)); // "" -> [""], as the upstream tests pass
                }
                out.println(gson.toJson(dump(path, custom)));
            }
        }
    }

    static JsonObject dump(String path, List<String> custom) {
        JsonObject root = new JsonObject();
        root.addProperty("apk", new File(path).getName());
        JsonArray customs = new JsonArray();
        custom.forEach(customs::add);
        root.add("customAnnotations", customs);
        try {
            List<TestMethod> methods = DexParser.findTestMethods(path, new ArrayList<>(custom));
            JsonArray tests = new JsonArray();
            IdentityHashMap<Object, Integer> arrays = new IdentityHashMap<>();
            for (TestMethod m : methods) tests.add(method(m, arrays));
            root.add("tests", tests);
        } catch (Throwable t) {
            root.add("testsError", error(t));
        }
        ApkParser apkParser = new ApkParser();
        try {
            InstrumentationInfo info = apkParser.parseInstrumentationInfo(new File(path));
            JsonObject o = new JsonObject();
            o.addProperty("applicationPackage", info.getApplicationPackage());
            o.addProperty("instrumentationPackage", info.getInstrumentationPackage());
            o.addProperty("testRunnerClass", info.getTestRunnerClass());
            root.add("instrumentationInfo", o);
        } catch (Throwable t) {
            root.add("instrumentationInfoError", error(t));
        }
        try {
            root.addProperty("appPackageName", apkParser.parseAppPackageName(new File(path)));
        } catch (Throwable t) {
            root.add("appPackageNameError", error(t));
        }
        return root;
    }

    static JsonObject method(TestMethod m, IdentityHashMap<Object, Integer> arrays) {
        JsonObject o = new JsonObject();
        o.addProperty("testName", m.getTestName());
        JsonArray anns = new JsonArray();
        for (TestAnnotation a : m.getAnnotations()) {
            JsonObject ao = new JsonObject();
            ao.addProperty("name", a.getName());
            ao.addProperty("inherited", a.getInherited());
            JsonArray values = new JsonArray();
            for (Map.Entry<String, DecodedValue> e : a.getValues().entrySet()) {
                JsonArray pair = new JsonArray();
                pair.add(e.getKey());
                pair.add(value(e.getValue(), arrays));
                values.add(pair);
            }
            ao.add("values", values);
            anns.add(ao);
        }
        o.add("annotations", anns);
        return o;
    }

    // [kind, rendered] where rendered keeps Java scalar text; arrays recurse as
    // ["array", [element, ...], id]. Char renders as its UTF-16 code unit number
    // so unpaired units survive JSON. The id numbers the backing DecodedValue[]
    // instances of one APK in order of first visit (outer before nested): DecodedArrayValue
    // equality is array identity, so sharing is part of the result.
    static JsonArray value(DecodedValue v, IdentityHashMap<Object, Integer> arrays) {
        JsonArray r = new JsonArray();
        if (v instanceof DecodedValue.DecodedByte b) { r.add("byte"); r.add(Byte.toString(b.getValue())); }
        else if (v instanceof DecodedValue.DecodedShort s) { r.add("short"); r.add(Short.toString(s.getValue())); }
        else if (v instanceof DecodedValue.DecodedChar c) { r.add("char"); r.add(Integer.toString((int) c.getValue())); }
        else if (v instanceof DecodedValue.DecodedInt i) { r.add("int"); r.add(Integer.toString(i.getValue())); }
        else if (v instanceof DecodedValue.DecodedLong l) { r.add("long"); r.add(Long.toString(l.getValue())); }
        else if (v instanceof DecodedValue.DecodedFloat f) { r.add("float"); r.add(Float.toString(f.getValue())); }
        else if (v instanceof DecodedValue.DecodedDouble d) { r.add("double"); r.add(Double.toString(d.getValue())); }
        else if (v instanceof DecodedValue.DecodedString s) { r.add("string"); r.add(s.getValue()); }
        else if (v instanceof DecodedValue.DecodedType t) { r.add("type"); r.add(t.getValue()); }
        else if (v instanceof DecodedValue.DecodedEnum e) { r.add("enum"); r.add(e.getValue()); }
        else if (v instanceof DecodedValue.DecodedBoolean b) { r.add("boolean"); r.add(Boolean.toString(b.getValue())); }
        else if (v instanceof DecodedValue.DecodedNull) { r.add("null"); r.add("null"); }
        else if (v instanceof DecodedValue.DecodedArrayValue a) {
            r.add("array");
            Integer id = arrays.get(a.getValues());
            if (id == null) {
                id = arrays.size();
                arrays.put(a.getValues(), id);
            }
            JsonArray elements = new JsonArray();
            for (DecodedValue e : a.getValues()) elements.add(value(e, arrays));
            r.add(elements);
            r.add(id);
        } else { r.add("unknown"); r.add(String.valueOf(v)); }
        return r;
    }

    static JsonObject error(Throwable t) {
        JsonObject o = new JsonObject();
        o.addProperty("class", t.getClass().getName());
        if (t.getMessage() != null) o.addProperty("message", t.getMessage());
        if (t.getCause() != null) o.add("cause", error(t.getCause()));
        return o;
    }
}
