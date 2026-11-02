import java.net.InetAddress;
public class ZeroReachability {
 public static void main(String[] args) throws Exception {
  for(String host : new String[]{"127.0.0.1","::1"}) {
   int yes=0,no=0;
   for(int i=0;i<1000;i++) {if(InetAddress.getByName(host).isReachable(0))yes++;else no++;}
   System.out.println(host+" true="+yes+" false="+no);
  }
 }
}
