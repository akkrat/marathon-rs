package probe;

import java.lang.annotation.ElementType;
import java.lang.annotation.Retention;
import java.lang.annotation.RetentionPolicy;
import java.lang.annotation.Target;

// Witness values for the P1/P2 oracle probes: negative short/int/long values
// (dex-test-parser zero-pads them, see docs/known-kotlin-bugs.md), high-bit
// float/double, a supplementary-plane string (P1 MUTF-8) and a nested
// annotation value (DecodedValue maps it to DecodedNull).
@Retention(RetentionPolicy.RUNTIME)
@Target({ElementType.METHOD, ElementType.TYPE})
@interface Negatives {
    short shortValue() default -1;
    int intValue() default -1;
    long longValue() default -1L;
    int intMin() default Integer.MIN_VALUE;
    float floatValue() default -1.5f;
    double doubleValue() default -2.5;
    String supplementary() default "𝄞";
    byte byteValue() default -1;
    char charValue() default '﷐';
}

@Retention(RetentionPolicy.RUNTIME)
@Target({ElementType.METHOD, ElementType.TYPE})
@interface Nested {
    String value() default "nested";
}

@Retention(RetentionPolicy.RUNTIME)
@Target({ElementType.METHOD, ElementType.TYPE})
@interface Holder {
    Nested nested() default @Nested;
    int[] ints() default {-1, 2};
    Nested[] nesteds() default {@Nested("a")};
}
