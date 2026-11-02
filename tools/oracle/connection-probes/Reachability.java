import java.net.InetAddress;
public class Reachability {
    public static void main(String[] args) throws Exception {
        int timeout = args.length == 0 ? 150 : Integer.parseInt(args[0]);
        for (String host : new String[]{"127.0.0.1", "::1", "192.0.2.1", "2001:db8::1", "255.255.255.255", "224.0.0.1"}) {
            String value;
            try { value = Boolean.toString(InetAddress.getByName(host).isReachable(timeout)); }
            catch (Exception error) { value = "\"" + error.getClass().getName() + ": " + error.getMessage() + "\""; }
            System.out.println("{\"case\":\""+host+(timeout==0?"/0":"")+"\",\"value\":"+value+"}");
        }
    }
}
