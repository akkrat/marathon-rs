//! Port of dex-test-parser 2.3.4 parser/src/test/kotlin/com/linkedin/dex/AbstractClassInSecondDex.kt
//! (BSD-2-Clause).
//!
//! This apk contains two dex files: classes.dex holds ConcreteTest (here
//! under its BasicJUnit4 name), classes2.dex the abstract class it
//! extends. The archive was created in a way that "classes.dex" goes
//! before "classes2.dex".

use std::path::PathBuf;

fn apk_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/dex/abstract-class-in-second-dex.apk")
}

fn count(test_name: &str) -> usize {
    marathon_dex::find_test_methods(&apk_path(), &["".into()])
        .unwrap()
        .iter()
        .filter(|m| m.test_name == test_name)
        .count()
}

#[test]
fn parse_method_from_base_abstract_class_when_abstract_class_in_the_second_dex() {
    assert_eq!(
        count("com.linkedin.parser.test.junit4.java.BasicJUnit4#abstractTest"),
        1
    );
}

#[test]
fn parse_method_from_concrete_class_that_extends_from_abstract_when_abstract_class_in_the_second_dex() {
    assert_eq!(
        count("com.linkedin.parser.test.junit4.java.BasicJUnit4#concreteTest"),
        1
    );
}
