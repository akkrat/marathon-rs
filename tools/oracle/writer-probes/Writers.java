// Oracle-only probes for WP 1.5. Run with the reference JDK and pinned jars.
import java.util.*;
import com.google.gson.*;
import com.fasterxml.jackson.databind.*;
import com.fasterxml.jackson.annotation.JsonInclude;
import com.fasterxml.jackson.dataformat.yaml.*;
public class Writers {
  static ObjectMapper yaml = new ObjectMapper(new YAMLFactory().disable(YAMLGenerator.Feature.USE_NATIVE_TYPE_ID)).setSerializationInclusion(JsonInclude.Include.NON_NULL);
  static Gson transport = new GsonBuilder().serializeNulls().disableHtmlEscaping().create();
  static Map<String,Object> obj(Object... pairs) {
    Map<String,Object> m = new LinkedHashMap<>();
    for (int i=0;i<pairs.length;i+=2) m.put((String)pairs[i],pairs[i+1]);
    return m;
  }
  static List<Integer> units(String s) { List<Integer> r = new ArrayList<>(); for(char c:s.toCharArray())r.add((int)c); return r; }
  static Object input(Object v) {
    if(v==null)return obj("kind","null");
    if(v instanceof String)return obj("kind","string","units",units((String)v));
    if(v instanceof Boolean)return obj("kind","bool","value",v);
    if(v instanceof Number)return obj("kind","number","value",v.toString());
    if(v instanceof Map) { List<Object> e=new ArrayList<>(); for(Object item:((Map<?,?>)v).entrySet()) {Map.Entry<?,?> a=(Map.Entry<?,?>)item;e.add(Arrays.asList(units(a.getKey().toString()),input(a.getValue())));}return obj("kind","object","entries",e); }
    List<Object> a=new ArrayList<>(); for(Object x:(List<?>)v)a.add(input(x));return obj("kind","array","items",a);
  }
  static Map<String,Object> success(String s) {
    List<Integer> bytes=new ArrayList<>();for(byte b:s.getBytes(java.nio.charset.StandardCharsets.UTF_8))bytes.add(b&255);
    return obj("ok",units(s),"bytes",bytes);
  }
  static boolean nativeStrings;
  static String nativeText(String s) {
    StringBuilder out = new StringBuilder();
    for (int i = 0; i < s.length(); i++) {
      char c = s.charAt(i);
      if (Character.isHighSurrogate(c) && i + 1 < s.length() && Character.isLowSurrogate(s.charAt(i + 1))) {
        out.append(c).append(s.charAt(++i));
      } else out.append(Character.isSurrogate(c) ? '\ufffd' : c);
    }
    return out.toString();
  }
  static Object nativeValue(Object v) {
    if (v instanceof String) return nativeText((String)v);
    if (v instanceof Map) {
      Map<String,Object> out = new LinkedHashMap<>();
      for (Map.Entry<?,?> e : ((Map<?,?>)v).entrySet()) out.put(nativeText(e.getKey().toString()), nativeValue(e.getValue()));
      return out;
    }
    if (v instanceof List) {
      List<Object> out = new ArrayList<>();
      for (Object x : (List<?>)v) out.add(nativeValue(x));
      return out;
    }
    return v;
  }
  static void row(String id,Object v) throws Exception {
    Map<String,Object> r=obj("id",id,"input",input(v));
    if (nativeStrings) v = nativeValue(v);
    for(boolean pretty:new boolean[]{false,true}) {
      GsonBuilder b=new GsonBuilder();if(pretty)b.setPrettyPrinting();
      try {r.put(pretty?"pretty":"gson",success(b.create().toJson(v)));}
      catch(Exception e) {r.put(pretty?"pretty":"gson",obj("error",e.getMessage()));}
    }
    try {r.put("yaml",success(yaml.writeValueAsString(v)));}
    catch(Exception e){r.put("yaml",obj("error",e.getClass().getName()));}
    System.out.println(transport.toJson(r));
  }
  public static void main(String[] args) throws Exception {
    nativeStrings = args.length == 1 && args[0].equals("--native-strings");
    if (args.length == 1 && args[0].equals("--round1")) {
      System.out.println(transport.toJson(obj("table","writer_round1","rows",18,"jdk",System.getProperty("java.runtime.version"),"generator","tools/oracle/writer-probes/run --round1")));
      String[] names = {"cr", "lf", "crlf", "nel", "ls", "ps"};
      String[] breaks = {"\r", "\n", "\r\n", "\u0085", "\u2028", "\u2029"};
      for (int i = 0; i < names.length; i++) {
        String key = "a" + breaks[i] + "b";
        row(names[i] + "-scalar", obj(key, 1));
        row(names[i] + "-list", obj(key, List.of(1, 2)));
        row(names[i] + "-mapping", obj(key, obj("child", 1)));
      }
      return;
    }
    System.out.println(transport.toJson(obj("table","writer_edges","jdk",System.getProperty("java.runtime.version"),"generator","tools/oracle/writer-probes/run")));
    row("null",null);row("empty-map",obj());row("empty-list",List.of());row("null-map",obj("a",null));
    row("nested",obj("first",Arrays.asList(obj("a",1,"b",List.of(1,2)),List.of(List.of(3,4),obj("c",5)),null),"last",true));
    StringBuilder controls=new StringBuilder();for(char c=0;c<160;c++)controls.append(c);
    row("controls",obj("value",controls.toString()));
    for(String s:new String[]{"\ud800","\udc00","\ud800x","\ud800\ud800","\udfff\ud800","😀","\ufffe\uffff\ufeff","\u0085\u00a0\u2028\u2029"})row("unicode-"+units(s),obj("value",s));
    for(int n:new int[]{78,79,80,81,120,128,250})row("wrap-"+n,obj("value","word ".repeat(n/5),"unicode","é".repeat(n),"escaped","\t".repeat(n)));
    for(String s:new String[]{"", "n", "~abc", "1word", "a:b", "a: b", "#hash", "a #b", "a,b", "[x]", " lead", "trail ", "?foo", "? foo", "é", "\u00a0", "line\nbreak", "k".repeat(128)})row("key-"+units(s),obj(s,"value"));
    for(String s:new String[]{"a\nb\n"+"long ".repeat(40), "\ud800", "\ud800x", "\udc00", "é".repeat(100), "a: b".repeat(40), "\u0085", "\u2028", "a\t", "\ufeff"})row("edge-key-"+units(s),obj(s,"value"));
    row("explicit-in-array", List.of(obj("",List.of(1,2)), obj("line\nbreak",obj("a",true))));
    row("nested-null-map", List.of(obj("a",null), List.of(obj(), List.of())));
    row("numbers",Arrays.asList(1L,1.0,0.5f,-0.0,1e7,1e-4,Float.MIN_VALUE,Double.MIN_VALUE));
    row("nan",Double.NaN);row("infinity",Double.POSITIVE_INFINITY);row("negative-infinity",Double.NEGATIVE_INFINITY);
    Random rng=new Random(15);
    String alphabet=" ab:#?,-[]{}&*!.~012true\u0085\u2028\u00a0";
    for(int i=0;i<200;i++){StringBuilder s=new StringBuilder();for(int j=0;j<rng.nextInt(160);j++)s.append(alphabet.charAt(rng.nextInt(alphabet.length())));row("seeded-key-"+i,obj(s.toString(),"v"));}
    for(int i=0;i<100;i++) {StringBuilder s=new StringBuilder();for(int j=0;j<rng.nextInt(180);j++)s.append((char)rng.nextInt(65536));row("seeded-"+i,obj("value",s.toString()));}
  }
}
