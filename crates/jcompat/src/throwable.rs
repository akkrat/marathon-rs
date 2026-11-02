//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/AppleDeviceTestRunner.kt
//! and NmTestParser.kt / XCTestParser.kt (JVM Error versus Exception catches).

/// Known JVM Error classes represented at the Rust throwable boundary.
/// A class name alone cannot establish inheritance; suffixes also match Exception subclasses.
pub fn is_error_class(class: &str) -> bool {
    matches!(
        class,
        "kotlin.NotImplementedError"
            | "kotlin.jvm.KotlinReflectionNotSupportedError"
            | "java.lang.Error"
            | "java.lang.AssertionError"
            | "java.lang.BootstrapMethodError"
            | "java.lang.ClassCircularityError"
            | "java.lang.ClassFormatError"
            | "java.lang.ExceptionInInitializerError"
            | "java.lang.IllegalAccessError"
            | "java.lang.IncompatibleClassChangeError"
            | "java.lang.InstantiationError"
            | "java.lang.InternalError"
            | "java.lang.LinkageError"
            | "java.lang.NoClassDefFoundError"
            | "java.lang.NoSuchFieldError"
            | "java.lang.NoSuchMethodError"
            | "java.lang.OutOfMemoryError"
            | "java.lang.StackOverflowError"
            | "java.lang.ThreadDeath"
            | "java.lang.UnknownError"
            | "java.lang.UnsatisfiedLinkError"
            | "java.lang.UnsupportedClassVersionError"
            | "java.lang.VerifyError"
            | "java.lang.VirtualMachineError"
            | "java.lang.annotation.AnnotationFormatError"
            | "java.awt.AWTError"
            | "java.io.IOError"
            | "java.nio.charset.CoderMalfunctionError"
            | "java.util.ServiceConfigurationError"
            | "java.util.zip.ZipError"
            | "javax.xml.parsers.FactoryConfigurationError"
            | "javax.xml.stream.FactoryConfigurationError"
            | "javax.xml.transform.TransformerFactoryConfigurationError"
    )
}

#[cfg(test)]
mod tests {
    use super::is_error_class;

    #[test]
    fn error_catches_follow_known_classes_including_thread_death() {
        for class in ["java.lang.AssertionError", "java.lang.ThreadDeath", "java.io.IOError"] {
            assert!(is_error_class(class));
        }
        for class in [
            "java.io.IOException",
            "probe.ExceptionNamedError",
            "probe.AssertionError",
        ] {
            assert!(!is_error_class(class));
        }
    }
}
