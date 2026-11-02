package probe;

import org.junit.Test;

public class ProbeTest {
    // Values are set explicitly: DEX stores only set elements, not defaults.
    @Test
    @Negatives(shortValue = -1, intValue = -1, longValue = -1L,
            intMin = Integer.MIN_VALUE, floatValue = -1.5f, doubleValue = -2.5,
            supplementary = "𝄞", byteValue = -1, charValue = '﷐')
    @Holder(nested = @Nested("explicit"), ints = {-1, 2}, nesteds = {@Nested("a")})
    public void probeValues() {
    }
}
