//! Port of configuration/src/test/kotlin/com/malinskiy/marathon/config/serialization/ConfigurationSerializationTest.kt.
#[path = "support/load.rs"]
mod load;
use marathon_config::{AnalyticsConfiguration, VendorConfiguration, loader::MarathonConfigurationLoader, serialize};
use marathon_test_support::yaml::{self, expected_data, field};
use serde_json::json;
use std::{
    error::Error,
    fs,
    path::{Path, PathBuf},
};
type Result<T = ()> = std::result::Result<T, Box<dyn Error>>;

#[test]
fn configuration_string_properties_round_trip_and_match_independent_data() -> Result {
    let (_temp, dir) = load::temp()?;
    let context = load::context(&dir.join("Marathonfile"), &dir, None);
    let mut config = MarathonConfigurationLoader
        .parse_sync(
            b"name: test\noutputDir: out\nvendorConfiguration: {type: iOS}\n",
            &context,
        )
        .map_err(|e| format!("{e:?}"))?;
    let baseline = yaml::independent_documents(&[serialize(&config)?])?.remove(0);
    let mut documents = vec![];
    let mut expected = vec![];
    for text in yaml::property_strings() {
        config.name = text.clone();
        if let VendorConfiguration::Ios(c) = &mut config.vendor_configuration {
            c.xctestrun_env.app_envs = vec![(text.clone(), text.clone())];
            c.xctestrun_env.test_envs = vec![(text.clone(), text.clone())];
        } else {
            return Err("iOS configuration".into());
        }
        let document = serialize(&config)?;
        let recreated = MarathonConfigurationLoader
            .parse_sync(document.as_bytes(), &context)
            .map_err(|e| format!("{text:?}: {e:?}"))?;
        assert_eq!(config, recreated, "{text:?}");
        let mut data = baseline[0].clone();
        *field(&mut data, "name") = expected_data(&json!(text));
        let env = field(field(&mut data, "vendorConfiguration"), "xctestrunEnv");
        let entries = json!([
            "map",
            null,
            [[expected_data(&json!(text)), expected_data(&json!(text))]]
        ]);
        *field(env, "app") = entries.clone();
        *field(env, "test") = entries;
        documents.push(document);
        expected.push(json!([data]));
    }
    assert_eq!(yaml::independent_documents(&documents)?, expected);
    Ok(())
}

#[test]
fn configuration_numeric_edges_retain_legacy_data_types_and_round_trip() -> Result {
    let (_temp, dir) = load::temp()?;
    let context = load::context(&dir.join("Marathonfile"), &dir, None);
    let mut config = MarathonConfigurationLoader.parse_sync(b"name: test\noutputDir: out\nvendorConfiguration: {type: stub}\nanalyticsConfiguration: {type: graphite, host: localhost}\n", &context).map_err(|e| format!("{e:?}"))?;
    config.test_batch_timeout_millis = i64::MIN;
    config.test_output_timeout_millis = i64::MAX;
    for (value, spelling) in [
        (f64::NAN, "NaN"),
        (f64::INFINITY, "Infinity"),
        (f64::NEG_INFINITY, "-Infinity"),
        (-0.0, "-0.0"),
        (f64::MAX, "1.7976931348623157E308"),
        (f64::MIN_POSITIVE, "2.2250738585072014E-308"),
        (f64::from_bits(1), "4.9E-324"),
    ] {
        if let AnalyticsConfiguration::Graphite(c) = &mut config.analytics_configuration {
            c.defaults.success_rate = value;
        }
        let document = serialize(&config)?;
        let recreated = MarathonConfigurationLoader
            .parse_sync(document.as_bytes(), &context)
            .map_err(|e| format!("{e:?}"))?;
        assert_eq!(config, recreated);
        let data = yaml::independent_documents(&[document])?.remove(0);
        let success = &mut data[0].clone();
        let scalar = field(
            field(field(success, "analyticsConfiguration"), "defaults"),
            "successRate",
        )
        .clone();
        let expected = yaml::independent_documents(&[format!("successRate: {spelling}\n")])?.remove(0);
        let mut expected = expected[0].clone();
        assert_eq!(scalar, *field(&mut expected, "successRate"));
        assert_eq!(
            *field(success, "testBatchTimeoutMillis"),
            expected_data(&json!(i64::MIN))
        );
        assert_eq!(
            *field(success, "testOutputTimeoutMillis"),
            expected_data(&json!(i64::MAX))
        );
    }
    Ok(())
}

fn collect_yaml(root: &Path, cases: &mut Vec<PathBuf>) -> Result {
    for entry in fs::read_dir(root)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_yaml(&path, cases)?;
        } else if path.extension().is_some_and(|e| e == "yaml" || e == "yml") {
            cases.push(path);
        }
    }
    Ok(())
}

#[test]
fn every_parseable_configuration_fixture_round_trips_through_typed_views() -> Result {
    let (_temp, root) = load::fixture_copy()?;
    let mut cases = vec![];
    collect_yaml(&root, &mut cases)?;
    cases.sort();
    let mut accepted = 0;
    for file in cases {
        let context = load::context(&file, file.parent().ok_or("fixture parent")?, Some("/android-sdk"));
        if let Ok(config) = load::parse(&file, &context) {
            let document = serialize(&config)?;
            let recreated = MarathonConfigurationLoader
                .parse_sync(document.as_bytes(), &context)
                .map_err(|e| format!("{file:?}: {e:?}"))?;
            assert_eq!(config, recreated, "{file:?}");
            accepted += 1;
        }
    }
    assert_eq!(accepted, 25);
    Ok(())
}
