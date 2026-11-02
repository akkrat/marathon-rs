// WP 4.5a JVM probes against dd-plist 1.28 and the Kotlin plist models of marathon e47352f2.
// Run via run.sh (needs the Kotlin distribution jars and a JDK; see README.md). Output is compared
// with expected.txt; the Rust writer/models must reproduce every PROBE section that reaches an
// output file or a production path.
import com.dd.plist.*;
import com.malinskiy.marathon.apple.plist.bundle.BundleInfo;
import java.io.*;
import java.nio.charset.StandardCharsets;
import java.nio.file.*;
import java.util.*;

public class PlistProbes {
    static String writeXml(NSObject o) throws Exception {
        ByteArrayOutputStream out = new ByteArrayOutputStream();
        XMLPropertyListWriter.write(o, out);
        return out.toString(StandardCharsets.UTF_8);
    }

    static void section(String name) {
        System.out.println("PROBE " + name);
    }

    static void show(String label, String xml) {
        System.out.println(label + " bytes=" + xml.getBytes(StandardCharsets.UTF_8).length);
        System.out.println(xml.replace("\t", "\\t"));
        System.out.println("[end]");
    }

    interface Throwing { Object run() throws Exception; }

    static void attempt(String label, Throwing t) {
        try {
            Object r = t.run();
            System.out.println(label + " => OK " + describe(r));
        } catch (Throwable e) {
            Throwable c = e;
            System.out.println(label + " => THROW " + c.getClass().getName() + ": " + c.getMessage());
        }
    }

    static String describe(Object r) {
        if (r == null) return "null";
        if (r.getClass().isArray()) {
            StringBuilder sb = new StringBuilder(r.getClass().getComponentType().getName() + "[");
            int n = java.lang.reflect.Array.getLength(r);
            for (int i = 0; i < n; i++) {
                if (i > 0) sb.append(", ");
                sb.append(describe(java.lang.reflect.Array.get(r, i)));
            }
            return sb.append("]").toString();
        }
        if (r instanceof java.util.Date) return "java.util.Date(" + ((java.util.Date) r).toInstant() + ")";
        return r.getClass().getName() + "(" + String.valueOf(r).replaceAll("@[0-9a-f]+", "@HASH") + ")";
    }

    public static void main(String[] args) throws Exception {
        writerScalars();
        nullWrites();
        duplicateKeys();
        equalityAndOrder(args);
        nestedModelReads(args);
        missingKeys();
        conversions(args);
        asciiInput();
        escapeRules();
        putNullDirect();
        intConversions();
        controlCharsAndArrays();
        cdataAndEncodings();
        writerKeyControlAndDates();
        reviewRound2();
    }

    static void writerScalars() throws Exception {
        section("writer-scalars");
        NSDictionary d = new NSDictionary();
        d.put("EmptyString", new NSString(""));
        d.put("EmptyDict", new NSDictionary());
        d.put("EmptyArray", new NSArray());
        NSDictionary nested = new NSDictionary();
        nested.put("InnerEmptyArray", new NSArray());
        nested.put("InnerEmptyDict", new NSDictionary());
        d.put("Nested", nested);
        d.put("Escapes", new NSString("a&b<c>d\"e'f\ng\th\ri"));
        d.put("NonAscii", new NSString("café — üñïçødé 😀"));
        d.put("Key<&", new NSString("special key"));
        d.put("NegInt", new NSNumber(-42));
        d.put("BigInt", new NSNumber(3000000000L));
        d.put("LongMax", new NSNumber(Long.MAX_VALUE));
        d.put("LongMin", new NSNumber(Long.MIN_VALUE));
        d.put("Real", new NSNumber(1.5));
        d.put("RealWhole", new NSNumber(2.0));
        d.put("RealNegZero", new NSNumber(-0.0));
        d.put("RealSmall", new NSNumber(0.000001));
        d.put("RealBig", new NSNumber(1.0e20));
        d.put("True", new NSNumber(true));
        d.put("False", new NSNumber(false));
        d.put("Date", new NSDate("2024-01-02T03:04:05Z"));
        d.put("Data", new NSData(new byte[]{0x00, 0x01, 0x02, (byte) 0xfd, (byte) 0xfe, (byte) 0xff}));
        byte[] longData = new byte[80];
        for (int i = 0; i < longData.length; i++) longData[i] = (byte) i;
        d.put("LongData", new NSData(longData));
        d.put("Uid", new UID("CF$UID", new byte[]{0x01, 0x02}));
        show("scalars", writeXml(d));

        section("writer-real-special");
        for (double v : new double[]{Double.NaN, Double.POSITIVE_INFINITY, Double.NEGATIVE_INFINITY, 3.141592653589793, 1.0e-7, 123456789.123456789}) {
            NSDictionary one = new NSDictionary();
            one.put("R", new NSNumber(v));
            attempt("real " + v, () -> writeXml(one).split("\n")[5].trim());
        }
        section("writer-root-string");
        show("root-string", writeXml(new NSString("hello")));
        section("writer-set");
        NSSet set = new NSSet(new NSString("b"), new NSString("a"));
        NSDictionary sd = new NSDictionary();
        sd.put("Set", set);
        attempt("nsset-write", () -> writeXml(sd));
    }

    static void nullWrites() throws Exception {
        section("null-writes");
        NSObject fromNull = NSObject.fromJavaObject(null);
        System.out.println("fromJavaObject(null) = " + (fromNull == null ? "java-null" : fromNull.getClass().getName() + "(" + fromNull + ")"));
        NSDictionary d = new NSDictionary();
        attempt("put-fromNull", () -> { d.put("K", fromNull); return "stored class=" + (d.get("K") == null ? "java-null" : d.get("K").getClass().getName()); });
        attempt("write-fromNull", () -> writeXml(d));

        // The v0 constructor assigns testBundlePath/testHostPath unconditionally (null on the artifact-reuse path).
        Map<String, String> env = new LinkedHashMap<>();
        env.put("A", "1");
        com.malinskiy.marathon.apple.xctestrun.v0.TestTarget t0 =
            new com.malinskiy.marathon.apple.xctestrun.v0.TestTarget(null, null, env, null, null, null, null, null, null, null, null, null, null, null, null);
        System.out.println("v0-reuse dict keys = " + t0.getDelegate().keySet());
        for (Map.Entry<String, NSObject> e : t0.getDelegate().entrySet()) {
            System.out.println("  " + e.getKey() + " -> " + (e.getValue() == null ? "java-null" : e.getValue().getClass().getSimpleName() + "(" + e.getValue() + ")"));
        }
        ByteArrayOutputStream out = new ByteArrayOutputStream();
        attempt("v0-reuse-saveAsXML", () -> { t0.saveAsXML(out); return out.toString(StandardCharsets.UTF_8); });
        attempt("v0-reuse-read-testBundlePath", () -> t0.getTestBundlePath());
        // optionalDelegateFor set to null after a real value
        t0.setBaselinePath("somewhere");
        t0.setBaselinePath(null);
        System.out.println("after baseline=null keys = " + t0.getDelegate().keySet());
        NSObject bp = t0.getDelegate().get("BaselinePath");
        System.out.println("BaselinePath stored = " + (bp == null ? "java-null-or-absent" : bp.getClass().getSimpleName() + "(" + bp + ")"));
        attempt("optional-null-then-read", () -> t0.getBaselinePath());
        out.reset();
        attempt("v0-after-baseline-null-saveAsXML", () -> { t0.saveAsXML(out); return out.toString(StandardCharsets.UTF_8); });

        // optionalPlistDelegateFor set to null: v2 Xctestrun testPlan
        com.malinskiy.marathon.apple.xctestrun.v2.Xctestrun x2 =
            new com.malinskiy.marathon.apple.xctestrun.v2.Xctestrun(new NSDictionary());
        com.malinskiy.marathon.apple.xctestrun.v2.TestPlan plan =
            new com.malinskiy.marathon.apple.xctestrun.v2.TestPlan("P", true);
        x2.setTestPlan(plan);
        System.out.println("v2 after setTestPlan keys = " + x2.getDelegate().keySet());
        x2.setTestPlan(null);
        System.out.println("v2 after setTestPlan(null) keys = " + x2.getDelegate().keySet());
    }

    static void duplicateKeys() throws Exception {
        section("duplicate-keys");
        String xml = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n"
            + "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n"
            + "<plist version=\"1.0\"><dict>"
            + "<key>A</key><string>first</string><key>B</key><string>bee</string><key>A</key><string>second</string>"
            + "</dict></plist>";
        NSObject parsed = PropertyListParser.parse(xml.getBytes(StandardCharsets.UTF_8));
        show("dup-parsed-rewritten", writeXml(parsed));
    }

    static void equalityAndOrder(String[] args) throws Exception {
        section("equality-order");
        NSDictionary a = new NSDictionary();
        a.put("X", new NSString("1")); a.put("Y", new NSNumber(2)); a.put("Z", new NSArray(new NSString("s")));
        NSDictionary b = new NSDictionary();
        b.put("Z", new NSArray(new NSString("s"))); b.put("Y", new NSNumber(2)); b.put("X", new NSString("1"));
        System.out.println("dict-eq-reordered = " + a.equals(b) + " hash-eq = " + (a.hashCode() == b.hashCode()));
        NSArray arr1 = new NSArray(new NSString("a"), new NSString("b"));
        NSArray arr2 = new NSArray(new NSString("b"), new NSString("a"));
        System.out.println("array-eq-reordered = " + arr1.equals(arr2));
        System.out.println("num-int-vs-long = " + new NSNumber(2).equals(new NSNumber(2L)));
        System.out.println("num-int-vs-real = " + new NSNumber(2).equals(new NSNumber(2.0)));
        System.out.println("num-bool-vs-int = " + new NSNumber(true).equals(new NSNumber(1)));
        a.put("X", new NSString("replaced"));
        a.put("W", new NSString("appended"));
        show("replace-keeps-position", writeXml(a));
        Path fx = Path.of(args.length > 0 ? args[0] : "fixtures");
        NSObject f1 = PropertyListParser.parse(fx.resolve("UITesting_iphonesimulator11.2-x86_64.xctestrun").toFile());
        NSObject f2 = PropertyListParser.parse(fx.resolve("UITesting_iphonesimulator11.2-x86_64-reordered.xctestrun").toFile());
        System.out.println("reordered-fixture-eq = " + f1.equals(f2) + " hash-eq = " + (f1.hashCode() == f2.hashCode()));
    }

    static void nestedModelReads(String[] args) throws Exception {
        section("nested-model-reads");
        Path fx = Path.of(args.length > 0 ? args[0] : "fixtures");
        NSDictionary man2 = (NSDictionary) PropertyListParser.parse(fx.resolve("man_v2.xctestrun").toFile());
        com.malinskiy.marathon.apple.xctestrun.v2.Xctestrun x2 =
            new com.malinskiy.marathon.apple.xctestrun.v2.Xctestrun(man2);
        attempt("v2.metadata", () -> x2.getMetadata());
        attempt("v2.metadata.formatVersion", () -> x2.getMetadata().getFormatVersion());
        attempt("v2.testPlan", () -> x2.getTestPlan());
        attempt("v2.testPlan.name", () -> x2.getTestPlan() == null ? null : x2.getTestPlan().getName());
        attempt("v2.testConfigurations", () -> x2.getTestConfigurations());
        attempt("v2.codeCoverageBuildableInfos", () -> x2.getCodeCoverageBuildableInfos());
        NSDictionary man0 = (NSDictionary) PropertyListParser.parse(fx.resolve("man_v0.xctestrun").toFile());
        com.malinskiy.marathon.apple.xctestrun.v0.Xctestrun x0 =
            new com.malinskiy.marathon.apple.xctestrun.v0.Xctestrun(man0);
        attempt("v0.targets", () -> x0.getTargets().keySet().toString());
        NSDictionary withScalar = new NSDictionary();
        withScalar.put("version", new NSNumber(1));
        attempt("v0.targets-nondict-value", () -> new com.malinskiy.marathon.apple.xctestrun.v0.Xctestrun(withScalar).getTargets());
        attempt("v1.targets-nondict-value", () -> new com.malinskiy.marathon.apple.xctestrun.v1.Xctestrun(withScalar).getTargets().keySet().toString());
        attempt("v2.testConfigurations-missing", () -> new com.malinskiy.marathon.apple.xctestrun.v2.Xctestrun(new NSDictionary()).getTestConfigurations());
        attempt("v2.codeCoverageBuildableInfos-missing", () -> new com.malinskiy.marathon.apple.xctestrun.v2.Xctestrun(new NSDictionary()).getCodeCoverageBuildableInfos());
    }

    static void missingKeys() {
        section("missing-keys");
        BundleInfo empty = new BundleInfo(new NSDictionary());
        System.out.println("BundleInfo over empty dict constructed OK");
        attempt("identification.bundleIdentifier", () -> empty.getIdentification().getBundleIdentifier());
        attempt("naming.bundleName", () -> empty.getNaming().getBundleName());
        attempt("osVersion.lsMinimumSystemVersion", () -> empty.getOperatingSystemVersion().getLsMinimumSystemVersion());
        attempt("undocumented.bundleSupportedPlatforms", () -> empty.getUndocumented().getBundleSupportedPlatforms());
        attempt("undocumented.uiDeviceFamily", () -> empty.getUndocumented().getUiDeviceFamily());
        attempt("categorization.packageType", () -> empty.getCategorization().getPackageType());
        NSDictionary wrong = new NSDictionary();
        wrong.put("CFBundleIdentifier", new NSNumber(7));
        BundleInfo wrongInfo = new BundleInfo(wrong);
        attempt("identification.bundleIdentifier-wrongtype", () -> wrongInfo.getIdentification().getBundleIdentifier());
        NSDictionary fam = new NSDictionary();
        fam.put("UIDeviceFamily", new NSArray(new NSNumber(1), new NSNumber(2)));
        fam.put("CFBundleSupportedPlatforms", new NSArray(new NSString("iPhoneSimulator")));
        BundleInfo famInfo = new BundleInfo(fam);
        attempt("undocumented.uiDeviceFamily-values", () -> famInfo.getUndocumented().getUiDeviceFamily());
        attempt("undocumented.bundleSupportedPlatforms-values", () -> famInfo.getUndocumented().getBundleSupportedPlatforms());
        NSDictionary famWrong = new NSDictionary();
        famWrong.put("UIDeviceFamily", new NSArray(new NSString("iphone")));
        attempt("undocumented.uiDeviceFamily-wrongtype", () -> new BundleInfo(famWrong).getUndocumented().getUiDeviceFamily());
        // non-string env value through the unchecked map cast (P9 witness)
        NSDictionary envDict = new NSDictionary();
        NSDictionary env = new NSDictionary();
        env.put("S", new NSString("ok"));
        env.put("N", new NSNumber(3));
        envDict.put("TestingEnvironmentVariables", env);
        com.malinskiy.marathon.apple.xctestrun.v0.TestTarget tt =
            new com.malinskiy.marathon.apple.xctestrun.v0.TestTarget(envDict);
        attempt("v0.testingEnvironmentVariables-nonstring", () -> {
            Map<String, String> m = tt.getTestingEnvironmentVariables();
            return m + " N-class=" + ((Object) m.get("N")).getClass().getName();
        });
    }

    static void conversions(String[] args) throws Exception {
        section("conversions");
        System.out.println("NSNumber(2).toJavaObject = " + describe(new NSNumber(2).toJavaObject()));
        System.out.println("NSNumber(3000000000L).toJavaObject = " + describe(new NSNumber(3000000000L).toJavaObject()));
        System.out.println("NSNumber(1.5).toJavaObject = " + describe(new NSNumber(1.5).toJavaObject()));
        System.out.println("NSArray.toJavaObject = " + describe(new NSArray(new NSString("x")).toJavaObject()));
        System.out.println("NSDate.toJavaObject = " + describe(new NSDate("2024-01-02T03:04:05Z").toJavaObject()));
        System.out.println("NSData.toJavaObject = " + describe(new NSData(new byte[]{1}).toJavaObject()));
        Path fx = Path.of(args.length > 0 ? args[0] : "fixtures");
        NSDictionary man2 = (NSDictionary) PropertyListParser.parse(fx.resolve("man_v2.xctestrun").toFile());
        NSObject fv = ((NSDictionary) man2.get("__xctestrun_metadata__")).get("FormatVersion");
        System.out.println("parsed FormatVersion object = " + fv.getClass().getSimpleName() + " toJavaObject = " + describe(fv.toJavaObject()));
        // FormatVersion wider than Int read through the Kotlin Int delegate
        NSDictionary wide = new NSDictionary();
        wide.put("FormatVersion", new NSNumber(3000000000L));
        attempt("metadata.formatVersion-wide", () -> new com.malinskiy.marathon.apple.xctestrun.v2.Metadata(wide).getFormatVersion());
        NSDictionary realFv = new NSDictionary();
        realFv.put("FormatVersion", new NSNumber(2.0));
        attempt("metadata.formatVersion-real", () -> new com.malinskiy.marathon.apple.xctestrun.v2.Metadata(realFv).getFormatVersion());
        attempt("fromJavaObject-int-type", () -> NSObject.fromJavaObject(2).getClass().getSimpleName() + " type=" + ((NSNumber) NSObject.fromJavaObject(2)).type());
        // root not a dictionary, and not a plist at all
        attempt("parse-string-root", () -> PropertyListParser.parse("<?xml version=\"1.0\"?><plist version=\"1.0\"><string>x</string></plist>".getBytes(StandardCharsets.UTF_8)).getClass().getSimpleName());
        attempt("parse-garbage", () -> PropertyListParser.parse("not a plist".getBytes(StandardCharsets.UTF_8)));
        attempt("parse-empty", () -> PropertyListParser.parse(new byte[0]));
    }

    static void escapeRules() throws Exception {
        section("escape-rules");
        String[] cases = {"amp&only", "lt<only", "gt>only", "quot\"only", "apos'only", "new\nline", "carriage\rreturn", "tab\there", "cdata]]>inside", "ctrlchar", "delchar", "", " plain ", "]]>leading", "trailing]]"};
        for (int i = 0; i < cases.length; i++) {
            NSDictionary d = new NSDictionary();
            d.put("K" + i, new NSString(cases[i]));
            String xml = writeXml(d);
            System.out.println("case" + i + " input=" + cases[i].replace("\n", "\\n").replace("\r", "\\r").replace("\t", "\\t"));
            System.out.println(xml.replace("\t", "\\t").replace("\r", "\\r"));
            // round trip the full document in case the value spans lines
            attempt("case" + i + "-roundtrip", () -> describe(PropertyListParser.parse(xml.getBytes(StandardCharsets.UTF_8)).toJavaObject()));
        }
        NSDictionary kd = new NSDictionary();
        kd.put("", new NSString("empty key"));
        kd.put("key]]>bad", new NSString("v"));
        attempt("key-escapes", () -> writeXml(kd));
        byte[] data200 = new byte[200];
        for (int i = 0; i < data200.length; i++) data200[i] = (byte) i;
        NSDictionary dd = new NSDictionary();
        dd.put("D", new NSData(data200));
        String[] lines = writeXml(dd).split("\n");
        for (int i = 0; i < lines.length; i++) System.out.println("data200 line" + i + " len=" + lines[i].length() + " : " + lines[i].trim());
        System.out.println("real formats:");
        for (double v : new double[]{0.0, -1.5, 100.0, 0.1, 2.5e-5, 9999999.0, 10000000.0, 0.001, 0.0001, 4.9e-324, 1.7976931348623157e308, 1234567.891}) {
            NSDictionary one = new NSDictionary();
            one.put("R", new NSNumber(v));
            System.out.println("  " + v + " => " + writeXml(one).split("\n")[5].trim() + " java=" + Double.toString(v));
        }
    }

    static void putNullDirect() throws Exception {
        section("put-null-direct");
        NSDictionary d = new NSDictionary();
        d.put("A", new NSString("one"));
        d.put("A", (NSObject) null);
        System.out.println("after put(A,null) keys=" + d.keySet() + " value=" + d.get("A"));
        d.put("B", (NSObject) null);
        System.out.println("after put(B,null) keys=" + d.keySet());
        show("put-null-written", writeXml(d));
        // through a required delegate: testBundlePath set then set to null
        com.malinskiy.marathon.apple.xctestrun.v0.TestTarget t =
            new com.malinskiy.marathon.apple.xctestrun.v0.TestTarget(new NSDictionary());
        t.setTestBundlePath("x");
        t.setTestBundlePath(null);
        System.out.println("required-delegate set-null keys=" + t.getDelegate().keySet());
    }

    static void intConversions() {
        section("int-conversions");
        NSDictionary famWide = new NSDictionary();
        famWide.put("UIDeviceFamily", new NSArray(new NSNumber(3000000000L), new NSNumber(2.9), new NSNumber(-2.9)));
        attempt("uiDeviceFamily-wide-real", () -> new BundleInfo(famWide).getUndocumented().getUiDeviceFamily());
        NSDictionary fvReal = new NSDictionary();
        fvReal.put("FormatVersion", new NSNumber(2.9));
        attempt("formatVersion-2.9", () -> new com.malinskiy.marathon.apple.xctestrun.v2.Metadata(fvReal).getFormatVersion());
        NSDictionary fvBool = new NSDictionary();
        fvBool.put("FormatVersion", new NSNumber(true));
        attempt("formatVersion-bool", () -> new com.malinskiy.marathon.apple.xctestrun.v2.Metadata(fvBool).getFormatVersion());
        NSDictionary fvStr = new NSDictionary();
        fvStr.put("FormatVersion", new NSString("2"));
        attempt("formatVersion-string", () -> new com.malinskiy.marathon.apple.xctestrun.v2.Metadata(fvStr).getFormatVersion());
        // bool delegate from integer 1
        NSDictionary boolDict = new NSDictionary();
        boolDict.put("WKWatchKitApp", new NSNumber(1));
        attempt("bool-from-int1", () -> new BundleInfo(boolDict).getOperatingSystemVersion().getWatchKitApp());
        NSDictionary boolReal = new NSDictionary();
        boolReal.put("WKWatchKitApp", new NSNumber(1.0));
        attempt("bool-from-real1", () -> new BundleInfo(boolReal).getOperatingSystemVersion().getWatchKitApp());
        // string delegate from real/int (erasure: ClassCast at use site, see missing-keys)
        NSDictionary sDict = new NSDictionary();
        sDict.put("CFBundleName", new NSNumber(1.5));
        attempt("string-from-real", () -> new BundleInfo(sDict).getNaming().getBundleName());
    }

    static void controlCharsAndArrays() throws Exception {
        section("control-chars-arrays");
        StringBuilder kept = new StringBuilder();
        StringBuilder dropped = new StringBuilder();
        for (int c = 0; c < 0x20; c++) {
            NSDictionary d = new NSDictionary();
            d.put("K", new NSString("x" + (char) c + "y"));
            String xml = writeXml(d);
            (xml.contains("x" + (char) c + "y") ? kept : dropped).append(String.format("%02x ", c));
        }
        for (int c : new int[]{0x7f, 0xfffe, 0xffff, 0x2028, 0x85}) {
            NSDictionary d = new NSDictionary();
            d.put("K", new NSString("x" + (char) c + "y"));
            String xml = writeXml(d);
            (xml.contains("x" + (char) c + "y") ? kept : dropped).append(String.format("%04x ", c));
        }
        System.out.println("writer keeps chars: " + kept);
        System.out.println("writer drops chars: " + dropped);
        NSDictionary amp = new NSDictionary();
        amp.put("K", new NSString("a&bc"));
        show("cdata-with-control", writeXml(amp));

        Object[][] elems = {
            {"int", new NSNumber(5)}, {"long-small", new NSNumber(5L)}, {"long-wide", new NSNumber(3000000000L)},
            {"real-whole", new NSNumber(2.0)}, {"real-frac", new NSNumber(2.5)}, {"bool", new NSNumber(true)},
            {"string", new NSString("5")},
        };
        for (Object[] e : elems) {
            NSDictionary fam = new NSDictionary();
            fam.put("UIDeviceFamily", new NSArray((NSObject) e[1]));
            BundleInfo info = new BundleInfo(fam);
            attempt("intArray-elem-" + e[0], () -> info.getUndocumented().getUiDeviceFamily());
            NSDictionary plat = new NSDictionary();
            plat.put("CFBundleSupportedPlatforms", new NSArray((NSObject) e[1]));
            BundleInfo pinfo = new BundleInfo(plat);
            attempt("stringArray-elem-" + e[0], () -> pinfo.getUndocumented().getBundleSupportedPlatforms());
        }
    }

    static void asciiInput() {
        section("ascii-input");
        String ascii = "{ CFBundleName = TestApp; CFBundleIdentifier = \"com.example.test\"; }";
        attempt("openstep-parse", () -> describe(PropertyListParser.parse(ascii.getBytes(StandardCharsets.UTF_8)).toJavaObject()));
    }

    // Review round 1: CDATA on the read path, CR normalisation, XML encodings, forbidden
    // control characters on parse, shared-subtree equality.
    static String visible(Object o) {
        return String.valueOf(o).replace("\r", "\\r").replace("\n", "\\n").replace("\t", "\\t")
            .replace("\u0001", "<01>").replace("\ufffe", "<FFFE>");
    }

    static String bundleNameOf(byte[] bytes) throws Exception {
        NSDictionary d = (NSDictionary) PropertyListParser.parse(bytes);
        return visible(d.get("CFBundleName"));
    }

    static void cdataAndEncodings() throws Exception {
        section("cdata-read");
        // Writer -> reader round trip through dd-plist itself: &, <, ]]> in values and keys.
        NSDictionary d = new NSDictionary();
        d.put("URL", new NSString("https://h/?a=1&b=2"));
        d.put("K<&]]>", new NSString("x]]>y & <z>"));
        d.put("Lines", new NSString("a\rb\r\nc\nd"));
        String xml = writeXml(d);
        show("cdata-written", xml.replace("\r", "\\r"));
        NSDictionary back = (NSDictionary) PropertyListParser.parse(xml.getBytes(StandardCharsets.UTF_8));
        System.out.println("roundtrip keys = " + visible(back.keySet()));
        System.out.println("roundtrip URL = " + visible(back.get("URL")));
        System.out.println("roundtrip special = " + visible(back.get("K<&]]>")));
        System.out.println("roundtrip Lines = " + visible(back.get("Lines")));
        System.out.println("roundtrip eq = " + back.equals(d));
        // Hand-written CDATA (incl. a split ]]>) and mixed text+CDATA.
        String hand = "<?xml version=\"1.0\" encoding=\"UTF-8\"?><plist version=\"1.0\"><dict>"
            + "<key><![CDATA[A&B]]></key><string><![CDATA[cdata]]]]><![CDATA[>inside]]></string>"
            + "<key>Mixed</key><string>pre<![CDATA[<&]]>post</string></dict></plist>";
        NSDictionary hp = (NSDictionary) PropertyListParser.parse(hand.getBytes(StandardCharsets.UTF_8));
        System.out.println("hand keys = " + visible(hp.keySet()));
        System.out.println("hand A&B = " + visible(hp.get("A&B")));
        System.out.println("hand Mixed = " + visible(hp.get("Mixed")));

        section("encodings");
        String cafe = "caf\u00e9";
        String doc8 = "<?xml version=\"1.0\" encoding=\"UTF-8\"?><plist version=\"1.0\"><dict><key>CFBundleName</key><string>" + cafe + "</string></dict></plist>";
        attempt("enc utf-8", () -> bundleNameOf(doc8.getBytes(StandardCharsets.UTF_8)));
        byte[] bom8 = new byte[]{(byte) 0xef, (byte) 0xbb, (byte) 0xbf};
        byte[] body8 = doc8.getBytes(StandardCharsets.UTF_8);
        byte[] withBom = new byte[bom8.length + body8.length];
        System.arraycopy(bom8, 0, withBom, 0, bom8.length);
        System.arraycopy(body8, 0, withBom, bom8.length, body8.length);
        attempt("enc utf-8-bom", () -> bundleNameOf(withBom));
        String doc16 = doc8.replace("UTF-8", "UTF-16");
        byte[] le = doc16.getBytes(java.nio.charset.Charset.forName("UTF-16LE"));
        byte[] bom16 = new byte[]{(byte) 0xff, (byte) 0xfe};
        byte[] withBom16 = new byte[bom16.length + le.length];
        System.arraycopy(bom16, 0, withBom16, 0, bom16.length);
        System.arraycopy(le, 0, withBom16, bom16.length, le.length);
        attempt("enc utf-16-bom-le", () -> bundleNameOf(withBom16));
        attempt("enc utf-16be-nobom", () -> bundleNameOf(doc8.replace("UTF-8", "UTF-16BE").getBytes(java.nio.charset.Charset.forName("UTF-16BE"))));
        attempt("enc utf-16le-nobom", () -> bundleNameOf(doc8.replace("UTF-8", "UTF-16LE").getBytes(java.nio.charset.Charset.forName("UTF-16LE"))));
        attempt("enc iso-8859-1", () -> bundleNameOf(doc8.replace("UTF-8", "ISO-8859-1").getBytes(java.nio.charset.Charset.forName("ISO-8859-1"))));

        section("control-on-parse");
        String ctrl = "<?xml version=\"1.0\" encoding=\"UTF-8\"?><plist version=\"1.0\"><dict><key>x</key><string>a\u0001b</string></dict></plist>";
        attempt("parse literal-u0001", () -> bundleNameOf(ctrl.getBytes(StandardCharsets.UTF_8)));
        String ref = "<?xml version=\"1.0\" encoding=\"UTF-8\"?><plist version=\"1.0\"><dict><key>CFBundleName</key><string>a&#x1;b</string></dict></plist>";
        attempt("parse charref-u0001", () -> bundleNameOf(ref.getBytes(StandardCharsets.UTF_8)));

        section("equality-shared");
        NSDictionary a = new NSDictionary(), b = new NSDictionary(), c = new NSDictionary();
        c.put("k", new NSString("x"));
        b.put("k", c);
        a.put("k", b);
        System.out.println("shared a-eq-b = " + a.equals(b));
        System.out.println("shared b-eq-c = " + b.equals(c));
        NSDictionary a2 = new NSDictionary(), b2 = new NSDictionary(), c2 = new NSDictionary();
        c2.put("k", new NSString("x"));
        b2.put("k", c2);
        a2.put("k", b2);
        System.out.println("shared a-eq-a2 = " + a.equals(a2));
    }

    // Review round 1: NSDictionary.toXML does not filter control characters in keys (it only
    // CDATA-wraps), while NSString values are filtered; NSDate writes whole seconds only.
    static void writerKeyControlAndDates() throws Exception {
        section("writer-key-control");
        NSDictionary d = new NSDictionary();
        d.put("k\u0001ey", new NSString("plain control key"));
        d.put("k\u0001&<ey", new NSString("cdata control key"));
        d.put("bad\ufffekey", new NSString("fffe key"));
        d.put("V", new NSString("v\u0001\ufffew"));
        show("key-control-written", visible(writeXml(d)));
        section("writer-date-truncate");
        for (long ms : new long[]{1700000000123L, 1704164645123L, 1704164645000L}) {
            NSDictionary one = new NSDictionary();
            one.put("D", new NSDate(new java.util.Date(ms)));
            System.out.println("date-ms " + ms + " => " + writeXml(one).split("\n")[5].trim());
        }
    }

    // Review round 2: number text with whitespace, boolean content validation, BOM versus
    // XML declaration, fractional-second dates.
    static String valueOf(String body) throws Exception {
        String doc = "<?xml version=\"1.0\" encoding=\"UTF-8\"?><plist version=\"1.0\"><dict><key>V</key>"
            + body + "</dict></plist>";
        NSObject v = ((NSDictionary) PropertyListParser.parse(doc.getBytes(StandardCharsets.UTF_8))).get("V");
        if (v instanceof NSNumber) {
            NSNumber n = (NSNumber) v;
            String type = n.type() == NSNumber.INTEGER ? "integer" : n.type() == NSNumber.REAL ? "real" : "boolean";
            return type + "(" + n + ")";
        }
        if (v instanceof NSDate) return "date(" + ((NSDate) v).getDate().toInstant() + ")";
        return v.getClass().getSimpleName() + "(" + visible(v) + ")";
    }

    static byte[] concat(byte[] a, byte[] b) {
        byte[] out = new byte[a.length + b.length];
        System.arraycopy(a, 0, out, 0, a.length);
        System.arraycopy(b, 0, out, a.length, b.length);
        return out;
    }

    static void reviewRound2() throws Exception {
        section("number-text");
        String[] numbers = {
            "<integer> 42 </integer>", "<integer>\n42\n</integer>", "<integer>\t-7\t</integer>",
            "<real> 2.5 </real>", "<real>\n2.5\n</real>", "<integer>42</integer>", "<real>2.5</real>",
            "<real>1</real>", "<integer>2.0</integer>", "<integer>18446744073709551615</integer>",
            "<integer>0x10</integer>", "<integer> 0x10 </integer>", "<integer> </integer>",
        };
        for (String n : numbers) attempt("number " + visible(n), () -> valueOf(n));

        section("boolean-content");
        String[] bools = {
            "<true/>", "<false/>", "<true>text</true>", "<true>&amp;</true>", "<true>&#x41;</true>",
            "<true>&#x1;</true>", "<true>&bogus;</true>", "<false><string>ignored</string></false>",
            "<true><a><b/></a>x</true>", "<true><![CDATA[x]]></true>",
        };
        for (String b : bools) attempt("bool " + b, () -> valueOf(b));

        section("encoding-matrix");
        String cafe = "café";
        String[] decls = {
            null, "UTF-8", "utf-8", "ISO-8859-1", "iso-8859-1", "ISO8859-1", "US-ASCII", "us-ascii", "ASCII",
            "UTF-16", "utf-16", "UTF-16LE", "UTF-16BE", "utf-16le", "utf-16be",
        };
        String[][] prefixes = {
            {"none", "UTF-8", ""}, {"bom-utf8", "UTF-8", "efbbbf"}, {"bom-utf16le", "UTF-16LE", "fffe"},
            {"bom-utf16be", "UTF-16BE", "feff"}, {"pattern-utf16le", "UTF-16LE", ""}, {"pattern-utf16be", "UTF-16BE", ""},
        };
        for (String[] p : prefixes) {
            byte[] bom = new byte[p[2].length() / 2];
            for (int i = 0; i < bom.length; i++) bom[i] = (byte) Integer.parseInt(p[2].substring(2 * i, 2 * i + 2), 16);
            for (String d : decls) {
                String decl = d == null ? "" : "<?xml version=\"1.0\" encoding=\"" + d + "\"?>";
                String doc = decl + "<plist version=\"1.0\"><dict><key>CFBundleName</key><string>" + cafe
                    + "</string></dict></plist>";
                // The UTF-16 patterns need a declaration to be recognised as such; without one
                // the document starts with "<p", so those rows probe the BOM-less fallback.
                byte[] bytes = concat(bom, doc.getBytes(java.nio.charset.Charset.forName(p[1])));
                attempt("enc " + p[0] + " decl=" + d, () -> bundleNameOf(bytes));
            }
        }
        String ascii = "<?xml version=\"1.0\" encoding=\"US-ASCII\"?><plist version=\"1.0\"><dict><key>CFBundleName</key><string>cafe</string></dict></plist>";
        attempt("enc none decl=US-ASCII ascii-only", () -> bundleNameOf(ascii.getBytes(StandardCharsets.US_ASCII)));
        String latin = "<?xml version=\"1.0\" encoding=\"US-ASCII\"?><plist version=\"1.0\"><dict><key>CFBundleName</key><string>" + cafe + "</string></dict></plist>";
        attempt("enc none decl=US-ASCII latin1-byte", () -> bundleNameOf(latin.getBytes(StandardCharsets.ISO_8859_1)));

        section("date-text");
        String[] dates = {
            "2024-01-02T03:04:05Z", "2024-01-02T03:04:05.123Z", "2024-01-02T03:04:05.5Z",
            "2024-01-02T03:04:05+01:00", "2024-01-02T03:04:05",
        };
        for (String d : dates) attempt("date " + d, () -> valueOf("<date>" + d + "</date>"));

        // Java-side leniency the Rust reader does not copy (approved simplifications).
        section("lenient-java");
        String[] lenient = {"2024-1-2T3:4:5Z", "2024-13-40T25:61:61Z", "2024-01-02T03:04:05Zjunk"};
        for (String d : lenient) attempt("date " + d, () -> valueOf("<date>" + d + "</date>"));
        String latin1 = "<?xml version=\"1.0\" encoding=\"LATIN1\"?><plist version=\"1.0\"><dict><key>CFBundleName</key><string>" + cafe + "</string></dict></plist>";
        attempt("enc decl=LATIN1", () -> bundleNameOf(latin1.getBytes(StandardCharsets.ISO_8859_1)));
    }
}
