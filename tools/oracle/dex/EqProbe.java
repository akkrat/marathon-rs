// WP 6.4 probe (run by hand against the pinned parser-2.3.4.jar):
// DecodedArrayValue equality is backing-array identity while its
// hashCode is content-based, so upstream HashSets keep fresh
// equal-content arrays as duplicates. Output (reference JBR 21.0.10):
//   freshArraysEqual=false, sameArrayEqual=true, hashFresh=true,
//   setSize=2, floatNaN=true, floatNegZero=false
// This is why WP 6.4 could not take proposal P4 (JUnit3 fixpoint).
import com.linkedin.dex.parser.DecodedValue;
import java.util.*;

public class EqProbe {
    public static void main(String[] a) {
        DecodedValue.DecodedInt i1 = new DecodedValue.DecodedInt(1);
        DecodedValue[] arr1 = {i1};
        DecodedValue[] arr2 = {new DecodedValue.DecodedInt(1)};
        DecodedValue.DecodedArrayValue x = new DecodedValue.DecodedArrayValue(arr1);
        DecodedValue.DecodedArrayValue y = new DecodedValue.DecodedArrayValue(arr2);
        DecodedValue.DecodedArrayValue z = new DecodedValue.DecodedArrayValue(arr1);
        System.out.println("freshArraysEqual=" + x.equals(y));
        System.out.println("sameArrayEqual=" + x.equals(z));
        System.out.println("hashFresh=" + (x.hashCode() == y.hashCode()));
        Set<DecodedValue> s = new HashSet<>(Arrays.asList(x, y, z));
        System.out.println("setSize=" + s.size());
        System.out.println("floatNaN=" + new DecodedValue.DecodedFloat(Float.NaN).equals(new DecodedValue.DecodedFloat(Float.NaN)));
        System.out.println("floatNegZero=" + new DecodedValue.DecodedFloat(0f).equals(new DecodedValue.DecodedFloat(-0f)));
    }
}
