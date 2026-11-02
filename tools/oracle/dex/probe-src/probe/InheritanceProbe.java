package probe;

import java.lang.annotation.Retention;
import java.lang.annotation.RetentionPolicy;
import org.junit.Test;

// Array identity through createAllTestMethods (WP 6.4 R1). Upstream's
// getOrPut cache is never filled, so ChainChild's inherited test carries a
// fresh read of ChainMiddle's class-level Shared array, not the one
// ChainMiddle's own test carries; a memoised port would share it. All classes
// are package-private: javac gives a public subclass of a package-private class
// a visibility bridge for inherited(), with the annotations copied onto it.
@Retention(RetentionPolicy.RUNTIME)
@interface Shared {
    int[] value();
}

@Retention(RetentionPolicy.RUNTIME)
@interface Own {
    int[] value();
}

class ChainBase {
    @Test
    @Own({1})
    public void inherited() {
    }
}

@Shared({2, 3})
class ChainMiddle extends ChainBase {
}

@Own({4})
class InheritanceProbe extends ChainMiddle {
}
