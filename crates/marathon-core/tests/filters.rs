//! Port of core/src/test/kotlin/com/malinskiy/marathon/execution/filter/*Test.kt
mod filter_support;
use filter_support::*;

use marathon_config::schema::{CompositionOperation as Op, TestFilterConfiguration as C, TestPackageFilter};
use marathon_core::{
    error::OperationResult,
    filter::{
        TestFilter,
        allure::{PlanTest, TestPlan},
    },
    model::{MetaProperty, MetaValue, Test},
};
use std::sync::Arc;

mod fully_qualified_classname_filter_test {
    use super::*;
    fn inputs() -> Vec<Test> {
        ["com.example", "com.example.subpackage", "com.sample"]
            .into_iter()
            .map(|p| test(p, "ClassTest", "fakeMethod"))
            .collect()
    }
    fn cnf(enabled: bool) -> C {
        C::FullyQualifiedClassname(config(Some(r"com\.example\.ClassTest"), None, enabled))
    }
    #[test]
    fn should_filter() -> OperationResult<()> {
        let t = inputs();
        check(cnf(true), &t, false, &t[..1])?;
        Ok(())
    }
    #[test]
    fn should_not_filter() -> OperationResult<()> {
        let t = inputs();
        check(cnf(true), &t, true, &t[1..])?;
        Ok(())
    }
    #[test]
    fn disabled() -> OperationResult<()> {
        let t = inputs();
        for not in [false, true] {
            check(cnf(false), &t, not, &t)?;
        }
        Ok(())
    }
}

mod fully_qualified_testname_filter_test {
    use super::*;
    fn inputs() -> Vec<Test> {
        vec![
            test("com.example", "Test1", "test1"),
            test("com.example.subpackage", "Test2", "test2"),
            test("com.sample", "Test3", "test3"),
            test("", "Test4", "test1"),
        ]
    }
    fn cnf(enabled: bool) -> C {
        C::FullyQualifiedTestname(config(Some(r"com\.example\..*#test.*"), None, enabled))
    }
    #[test]
    fn should_filter() -> OperationResult<()> {
        let t = inputs();
        check(cnf(true), &t, false, &t[..2])?;
        Ok(())
    }
    #[test]
    fn should_not_filter() -> OperationResult<()> {
        let t = inputs();
        check(cnf(true), &t, true, &t[2..])?;
        Ok(())
    }
    #[test]
    fn disabled() -> OperationResult<()> {
        let t = inputs();
        for not in [false, true] {
            check(cnf(false), &t, not, &t)?;
        }
        Ok(())
    }
    #[test]
    fn should_filter_with_values() -> OperationResult<()> {
        let t = inputs();
        check(
            C::FullyQualifiedTestname(config(
                None,
                Some(&["com.example.subpackage.Test2#test2", "Test4#test1"]),
                true,
            )),
            &t,
            false,
            &[t[1].clone(), t[3].clone()],
        )?;
        Ok(())
    }
}

mod simple_classname_filter_test {
    use super::*;
    fn inputs() -> Vec<Test> {
        ["SimpleTest", "ComplexTest", "SomeClass"]
            .into_iter()
            .map(|c| test("com.example", c, "fakeMethod"))
            .collect()
    }
    #[test]
    fn should_filter_if_values_are_specified() -> OperationResult<()> {
        let t = inputs();
        check(
            C::SimpleClassname(config(None, Some(&["SimpleTest"]), true)),
            &t,
            false,
            &t[..1],
        )?;
        Ok(())
    }
    #[test]
    fn should_filter_if_values_file_is_specified() -> OperationResult<()> {
        let temp = Temp::new()?;
        temp.write("values", "SimpleTest")?;
        let t = inputs();
        let mut c = config(None, None, true);
        c.file = Some(temp.path("values"));
        check(C::SimpleClassname(c), &t, false, &t[..1])?;
        Ok(())
    }
    #[test]
    fn supported_pattern_selection_and_disabled() -> OperationResult<()> {
        let t = inputs();
        for enabled in [false, true] {
            for not in [false, true] {
                let expected = if !enabled {
                    t.clone()
                } else if not {
                    t[2..].to_vec()
                } else {
                    t[..2].to_vec()
                };
                check(
                    C::SimpleClassname(config(Some(".*Test"), None, enabled)),
                    &t,
                    not,
                    &expected,
                )?;
            }
        }
        Ok(())
    }
    #[test]
    fn multi_parameter_validation_with_supported_pattern() -> OperationResult<()> {
        let c = C::SimpleClassname(config(Some(".*Test"), Some(&["SimpleTest"]), true));
        assert!(c.validate().is_err());
        Ok(())
    }
}

mod simple_testname_filter_test {
    use super::*;
    fn inputs() -> Vec<Test> {
        vec![
            test("com.example", "Test", "test1"),
            test("com.example.subpackage", "Test2", "test2"),
            test("com.sample", "Test3", "test3"),
            test("", "Test4", "test1"),
        ]
    }
    fn cnf(enabled: bool) -> C {
        C::SimpleTestname(config(None, Some(&["Test2#test2"]), enabled))
    }
    #[test]
    fn should_filter() -> OperationResult<()> {
        let t = inputs();
        check(cnf(true), &t, false, &t[1..2])?;
        Ok(())
    }
    #[test]
    fn should_not_filter() -> OperationResult<()> {
        let t = inputs();
        check(cnf(true), &t, true, &[t[0].clone(), t[2].clone(), t[3].clone()])?;
        Ok(())
    }
    #[test]
    fn disabled() -> OperationResult<()> {
        let t = inputs();
        for not in [false, true] {
            check(cnf(false), &t, not, &t)?;
        }
        Ok(())
    }
    #[test]
    fn should_filter_with_regex() -> OperationResult<()> {
        let t = inputs();
        check(
            C::SimpleTestname(config(Some(".+test3"), None, true)),
            &t,
            false,
            &t[2..3],
        )?;
        Ok(())
    }
}

mod annotation_filter_test {
    use super::*;
    fn inputs() -> Vec<Test> {
        let mut a = annotated("com.example.AnnotationOne", MetaValue::String(".*".into()));
        a.meta_properties
            .push(property("com.sample.AnnotationTwo", MetaValue::String(".*".into())));
        vec![
            a,
            annotated("com.example.AnnotationOne", MetaValue::String(".*".into())),
            test("com.sample", "SimpleTest", "fakeMethod"),
        ]
    }
    fn cnf(enabled: bool) -> C {
        C::Annotation(config(Some(r"com\.example.*"), None, enabled))
    }
    #[test]
    fn should_filter() -> OperationResult<()> {
        let t = inputs();
        check(cnf(true), &t, false, &t[..2])?;
        Ok(())
    }
    #[test]
    fn should_filter_not() -> OperationResult<()> {
        let t = inputs();
        check(cnf(true), &t, true, &t[2..])?;
        Ok(())
    }
    #[test]
    fn disabled() -> OperationResult<()> {
        let t = inputs();
        for not in [false, true] {
            check(cnf(false), &t, not, &t)?;
        }
        Ok(())
    }
}

mod annotation_data_filter_test {
    use super::*;
    fn inputs() -> Vec<Test> {
        let mut a = annotated("com.example.AnnotationOne", MetaValue::String(".*".into()));
        a.meta_properties
            .push(property("com.example.AnnotationTwo", MetaValue::String(".*".into())));
        vec![
            a,
            annotated("com.example.AnnotationOne", MetaValue::String(".*".into())),
            test("com.sample", "SimpleTest", "fakeMethod"),
        ]
    }
    fn cnf(exact: bool) -> C {
        C::AnnotationData {
            name_regex: regex(if exact {
                r"com\.example\.CorrectAnnotation"
            } else {
                r"com\.example.*"
            }),
            value_regex: regex(if exact { "CORRECT_VALUE" } else { ".*" }),
            enabled: true,
        }
    }
    fn array_test() -> Test {
        annotated(
            "com.example.CorrectAnnotation",
            array(vec![
                MetaValue::String("CORRECT_VALUE".into()),
                MetaValue::String("INCORRECT_VALUE".into()),
            ]),
        )
    }
    #[test]
    fn should_filter() -> OperationResult<()> {
        let t = inputs();
        check(cnf(false), &t, false, &t[..2])?;
        Ok(())
    }
    #[test]
    fn should_filter_not() -> OperationResult<()> {
        let t = inputs();
        check(cnf(false), &t, true, &t[2..])?;
        Ok(())
    }
    #[test]
    fn should_be_empty_list_for_filter() -> OperationResult<()> {
        let mut t = annotated(
            "com.example.IncorrectAnnotation",
            MetaValue::String("CORRECT_VALUE".into()),
        );
        t.meta_properties.push(property(
            "com.example.CorrectAnnotation",
            MetaValue::String("INCORRECT_VALUE".into()),
        ));
        check(cnf(true), &[t], false, &[])?;
        Ok(())
    }
    #[test]
    fn should_not_fail_if_value_field_is_not_exist() -> OperationResult<()> {
        let mut t = test("com.sample", "SimpleTest", "fakeMethod");
        t.meta_properties.push(MetaProperty {
            name: "com.example.CorrectAnnotation".into(),
            values: [("testKey".into(), MetaValue::String("testValue".into()))]
                .into_iter()
                .collect(),
        });
        check(cnf(false), &[t], false, &[])?;
        Ok(())
    }
    #[test]
    fn should_filter_with_array_string() -> OperationResult<()> {
        let mut t = inputs();
        let a = array_test();
        t.push(a.clone());
        let selected = apply(filter(&cnf(true)), t, false)?;
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].meta_properties, a.meta_properties);
        Ok(())
    }
    #[test]
    fn should_filter_not_with_array_string() -> OperationResult<()> {
        let original = inputs();
        let mut t = original.clone();
        t.push(array_test());
        let selected = apply(filter(&cnf(true)), t, true)?;
        assert_eq!(selected.len(), original.len());
        for (a, b) in selected.iter().zip(&original) {
            assert_eq!(a.meta_properties, b.meta_properties);
        }
        Ok(())
    }
    #[test]
    fn should_not_fail_with_array_another_type() -> OperationResult<()> {
        // P3: test-only projection of the custom object's toString; no generic model object.
        let render = |value: &str| {
            let _ = value;
            MetaValue::String("com.malinskiy.marathon.execution.filter.AnnotationValue@1234".into())
        };
        let mut t = inputs();
        t.push(annotated(
            "com.example.CorrectAnnotation",
            array(vec![render("CORRECT_VALUE"), render("INCORRECT_VALUE")]),
        ));
        check(cnf(true), &t, false, &[])?;
        Ok(())
    }
}

mod test_package_filter_test {
    use super::*;
    fn inputs() -> Vec<Test> {
        ["com.example", "com.example.subpackage", "com.sample"]
            .into_iter()
            .map(|p| test(p, "SimpleTest", "fakeMethod"))
            .collect()
    }
    fn cnf(re: bool, subpackages: bool, enabled: bool) -> C {
        let c = config(
            if re { Some(r"com\.example.*") } else { None },
            if re { None } else { Some(&["com.example"]) },
            enabled,
        );
        C::TestPackage(TestPackageFilter {
            regex: c.regex,
            values: c.values,
            file: None,
            subpackages,
            enabled,
        })
    }
    #[test]
    fn should_filter_by_regex() -> OperationResult<()> {
        let t = inputs();
        check(cnf(true, true, true), &t, false, &t[..2])?;
        Ok(())
    }
    #[test]
    fn should_filter_not_by_regex() -> OperationResult<()> {
        let t = inputs();
        check(cnf(true, true, true), &t, true, &t[2..])?;
        Ok(())
    }
    #[test]
    fn should_filter_by_values() -> OperationResult<()> {
        let t = inputs();
        check(cnf(false, false, true), &t, false, &t[..1])?;
        Ok(())
    }
    #[test]
    fn should_filter_not_by_values() -> OperationResult<()> {
        let t = inputs();
        check(cnf(false, false, true), &t, true, &t[1..])?;
        Ok(())
    }
    #[test]
    fn should_filter_by_values_with_subpackages() -> OperationResult<()> {
        let t = inputs();
        check(cnf(false, true, true), &t, false, &t[..2])?;
        Ok(())
    }
    #[test]
    fn should_filter_by_values_with_subpackages_by_partial_package_name() -> OperationResult<()> {
        check(
            cnf(false, true, true),
            &[test("com.example2", "SimpleTest", "fakeMethod")],
            false,
            &[],
        )?;
        Ok(())
    }
    #[test]
    fn should_filter_not_by_values_with_subpackages() -> OperationResult<()> {
        let t = inputs();
        check(cnf(false, true, true), &t, true, &t[2..])?;
        Ok(())
    }
    #[test]
    fn disabled() -> OperationResult<()> {
        let t = inputs();
        check(cnf(false, true, false), &t, true, &t)?;
        Ok(())
    }
}

mod composition_filter_test {
    use super::*;
    fn inputs() -> Vec<Test> {
        ["Dog", "Cat", "Horse", "Cube"]
            .into_iter()
            .map(|c| {
                let mut t = test(
                    "com.sample",
                    &format!(
                        "Filter{}Test",
                        if c == "Cube" {
                            c.to_string()
                        } else {
                            format!("Animal{c}")
                        }
                    ),
                    "fakeMethod",
                );
                t.meta_properties.push(MetaProperty {
                    name: if c == "Dog" { "com.example.BestAnimal" } else { "" }.into(),
                    values: empty_map(),
                });
                t
            })
            .collect()
    }
    fn cnf(op: Op) -> C {
        C::Composition {
            filters: vec![
                C::SimpleClassname(config(
                    Some(match op {
                        Op::Union => ".*Cat.*",
                        Op::Intersection => ".*Dog.*",
                        Op::Subtract => ".*Animal.*",
                    }),
                    None,
                    true,
                )),
                C::Annotation(config(Some("com.example.BestAnimal"), None, true)),
            ],
            op,
            enabled: true,
        }
    }
    #[test]
    fn should_filter_union() -> OperationResult<()> {
        let t = inputs();
        check(cnf(Op::Union), &t[..3], false, &[t[1].clone(), t[0].clone()])?;
        Ok(())
    }
    #[test]
    fn should_filter_not_union() -> OperationResult<()> {
        let t = inputs();
        check(cnf(Op::Union), &t[..3], true, &t[2..3])?;
        Ok(())
    }
    #[test]
    fn should_filter_intersection() -> OperationResult<()> {
        let t = inputs();
        check(cnf(Op::Intersection), &t[..3], false, &t[..1])?;
        Ok(())
    }
    #[test]
    fn should_filter_not_intersection() -> OperationResult<()> {
        let t = inputs();
        check(cnf(Op::Intersection), &t[..3], true, &t[1..3])?;
        Ok(())
    }
    #[test]
    fn should_filter_subtraction() -> OperationResult<()> {
        let t = inputs();
        check(cnf(Op::Subtract), &t, false, &t[1..3])?;
        Ok(())
    }
    #[test]
    fn should_filter_not_subtraction() -> OperationResult<()> {
        let t = inputs();
        check(cnf(Op::Subtract), &t, true, &[t[0].clone(), t[3].clone()])?;
        Ok(())
    }
}

mod fragmentation_filter_test {
    use super::*;

    #[test]
    fn test_fragments_properly() -> OperationResult<()> {
        let t = generate_tests(15);
        let mut total = 0;
        for index in 0..3 {
            let c = C::Fragmentation {
                index,
                count: 3,
                enabled: true,
            };
            assert!(c.validate().is_ok());
            total += apply(filter(&c), t.clone(), false)?.len();
        }
        assert_eq!(total, 15);
        Ok(())
    }
    #[test]
    fn test_filter_not() -> OperationResult<()> {
        let t = generate_tests(15);
        let a = apply(
            filter(&C::Fragmentation {
                index: 0,
                count: 3,
                enabled: true,
            }),
            t.clone(),
            true,
        )?;
        let mut b = apply(
            filter(&C::Fragmentation {
                index: 1,
                count: 3,
                enabled: true,
            }),
            t.clone(),
            false,
        )?;
        b.extend(apply(
            filter(&C::Fragmentation {
                index: 2,
                count: 3,
                enabled: true,
            }),
            t,
            false,
        )?);
        assert_eq!(a.len(), b.len());
        for item in a {
            assert!(b.contains(&item));
        }
        Ok(())
    }
    #[test]
    fn test_throws_exception() -> OperationResult<()> {
        for (index, count) in [(-1, 3), (1, -1), (5, 2)] {
            assert!(
                C::Fragmentation {
                    index,
                    count,
                    enabled: true
                }
                .validate()
                .is_err()
            );
        }
        Ok(())
    }
}

mod filter_values_file_reader_test {
    use super::*;

    #[test]
    fn should_ignore_comments() -> OperationResult<()> {
        let values = read(&fixture("valuelist_1"))?;
        let expected = [
            "com.example.test1",
            "com.example.test2",
            "com.example",
            "ClassName#test4",
            "SimpleTestName",
            "SimpleTestName2",
        ]
        .map(String::from);
        assert_eq!(values, expected);
        Ok(())
    }
}

mod single_value_test_filter_test {
    use super::*;

    #[test]
    fn should_filter_block_list_from_value_file() -> OperationResult<()> {
        let path = fixture("valuelist_2");
        let t = vec![test("pkg", "clazz", "method"), test("pkg", "clazz2", "method")];
        check(file_config(path), &t, true, &t[1..])?;
        Ok(())
    }
    #[test]
    fn if_empty_block_list_file_all_tests_should_be_allowed() -> OperationResult<()> {
        let path = fixture("emptylist");
        let t = vec![test("pkg", "clazz", "method"), test("pkg", "clazz", "method2")];
        check(file_config(path), &t, true, &t)?;
        Ok(())
    }
    #[test]
    fn if_block_list_file_is_not_exist_all_tests_should_be_allowed() -> OperationResult<()> {
        let temp = Temp::new()?;
        let path = temp.path("notExistFile");
        let t = vec![test("pkg", "clazz", "method"), test("pkg", "clazz", "method2")];
        check(file_config(path), &t, true, &t)?;
        Ok(())
    }
    #[test]
    fn should_filter_allow_list_from_value_file() -> OperationResult<()> {
        let path = fixture("valuelist_2");
        let t = vec![test("pkg", "clazz2", "method"), test("pkg", "clazz", "method")];
        check(file_config(path), &t, false, &t[1..])?;
        Ok(())
    }
    #[test]
    fn if_empty_allow_list_file_all_tests_should_be_blocked() -> OperationResult<()> {
        let path = fixture("emptylist");
        let t = vec![test("pkg", "clazz", "method"), test("pkg", "clazz", "method2")];
        check(file_config(path), &t, false, &[])?;
        Ok(())
    }
    #[test]
    fn if_allow_list_file_is_not_exist_all_tests_should_be_blocked() -> OperationResult<()> {
        let temp = Temp::new()?;
        let path = temp.path("notExistFile");
        let t = vec![test("pkg", "clazz", "method"), test("pkg", "clazz", "method2")];
        check(file_config(path), &t, false, &[])?;
        Ok(())
    }
}

mod allure_filter_test {
    use super::*;
    fn stub(annotation: &str, value: &str, method: &str) -> Test {
        let mut t = test("com.sample", "SimpleTest", method);
        t.meta_properties
            .push(property(annotation, MetaValue::String(value.into())));
        t
    }
    fn inputs() -> Vec<Test> {
        (1..=3)
            .map(|i| stub("io.qameta.allure.AllureId", &i.to_string(), &format!("test{i}")))
            .collect()
    }
    fn plan() -> TestPlan {
        TestPlan::V1_0(vec![
            PlanTest {
                id: Some("".into()),
                selector: Some("com.sample.SimpleTest.test2".into()),
            },
            PlanTest {
                id: Some("1".into()),
                selector: Some("com.sample.SimpleTest.test1".into()),
            },
        ])
    }
    fn f(p: Option<TestPlan>) -> TestFilter {
        TestFilter::from_config(&C::Allure { enabled: true }, &deps(p, Arc::default()))
    }
    #[test]
    fn test_filter_by_allure_id() -> OperationResult<()> {
        let t = inputs();
        for annotation in ["io.qameta.allure.AllureId", "io.qameta.allure.kotlin.AllureId"] {
            let a = stub(annotation, "1", "customName");
            let input = vec![a.clone(), t[2].clone()];
            assert_eq!(apply(f(Some(plan())), input.clone(), false)?, vec![a]);
            assert_eq!(apply(f(Some(plan())), input, true)?, t[2..]);
        }
        Ok(())
    }
    #[test]
    fn test_filter_by_test_name() -> OperationResult<()> {
        let t = inputs();
        let a = stub("non.default.package.AllureId", "-2", "test2");
        let input = vec![a.clone(), t[2].clone()];
        assert_eq!(apply(f(Some(plan())), input.clone(), false)?, vec![a]);
        assert_eq!(apply(f(Some(plan())), input, true)?, t[2..]);
        Ok(())
    }
    #[test]
    fn test_not_filtered() -> OperationResult<()> {
        let mut t = vec![stub("com.test.AllureId", "-2", "test3")];
        t.push(inputs()[2].clone());
        assert!(apply(f(Some(plan())), t.clone(), false)?.is_empty());
        assert_eq!(apply(f(Some(plan())), t.clone(), true)?, t);
        Ok(())
    }
    #[test]
    fn test_filter() -> OperationResult<()> {
        let t = inputs();
        assert_eq!(apply(f(Some(plan())), t.clone(), false)?, t[..2]);
        Ok(())
    }
    #[test]
    fn test_not_filter() -> OperationResult<()> {
        let t = inputs();
        assert_eq!(apply(f(Some(plan())), t.clone(), true)?, t[2..]);
        Ok(())
    }
    #[test]
    fn test_filter_null_test_plan() -> OperationResult<()> {
        let t = inputs();
        assert_eq!(apply(f(None), t.clone(), false)?, t);
        Ok(())
    }
    #[test]
    fn tes_not_filter_null_test_plan() -> OperationResult<()> {
        assert!(apply(f(None), inputs(), true)?.is_empty());
        Ok(())
    }
}
