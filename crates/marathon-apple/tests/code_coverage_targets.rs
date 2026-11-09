//! Port of vendor/vendor-apple/base/src/test/kotlin/com/malinskiy/marathon/apple/xctestrun/CodeCoverageTargetsTest.kt

use base64::{Engine, engine::general_purpose::STANDARD};
use marathon_apple::{
    plist::{PlistValue, PropertyList},
    xctestrun::{code_coverage_targets::to_remote, mach_o},
};
use serde_json::{Value, json};
use std::{fs, path::Path};
type TestResult = Result<(), Box<dyn std::error::Error>>;

fn value(value: PlistValue) -> Value {
    match value {
        PlistValue::String(s) => json!(s),
        PlistValue::Bool(b) => json!(b),
        PlistValue::Array(a) => a.into_iter().map(value_json).collect(),
        PlistValue::Dict(d) => Value::Object(d.snapshot().into_iter().map(|(k, v)| (k, value_json(v))).collect()),
        other => panic!("unexpected coverage field {other:?}"),
    }
}
fn value_json(v: PlistValue) -> Value {
    value(v)
}
fn replay(name: &str) -> TestResult {
    let row = include_str!("../../../fixtures/oracle/coverage-c95b2128/targets.jsonl")
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .find(|r| r["test"] == name)
        .ok_or("missing JVM witness")?;
    let root = tempfile::tempdir()?;
    let root_text = root.path().to_str().ok_or("UTF-8 temp root")?;
    let expand = |s: &str| s.replace("<root>", root_text);
    for file in row["files"].as_array().ok_or("files")? {
        let path = root.path().join(file["path"].as_str().ok_or("path")?);
        fs::create_dir_all(path.parent().ok_or("parent")?)?;
        if file["directory"] == true {
            fs::create_dir_all(&path)?;
        } else if let Some(target) = file["symlink"].as_str() {
            #[cfg(unix)]
            std::os::unix::fs::symlink(expand(target), &path)?;
        } else {
            let bytes = STANDARD.decode(file["bytes"].as_str().ok_or("bytes")?)?;
            let bytes = if path.extension().is_some_and(|s| s == "xctestrun") {
                expand(std::str::from_utf8(&bytes)?).into_bytes()
            } else {
                bytes
            };
            fs::write(&path, bytes)?;
        }
    }
    if name == "directory symlinks inside bundles are not followed" {
        for link in ["Resources/up", "Resources/again"] {
            let path = root
                .path()
                .join("Build/Products/Debug-iphonesimulator/CloudApp.app")
                .join(link);
            assert!(fs::symlink_metadata(path)?.file_type().is_symlink());
        }
    }
    for call in row["calls"].as_array().ok_or("calls")? {
        let roots = call["roots"]
            .as_array()
            .ok_or("roots")?
            .iter()
            .map(|r| {
                Ok((
                    expand(r[0].as_str().ok_or("local")?).into(),
                    expand(r[1].as_str().ok_or("remote")?),
                ))
            })
            .collect::<Result<Vec<_>, Box<dyn std::error::Error>>>()?;
        let result = to_remote(
            Path::new(&expand(call["xctestrun"].as_str().ok_or("xctestrun")?)),
            &roots,
            &expand(call["directory"].as_str().ok_or("directory")?),
            |base, parts| {
                std::iter::once(base)
                    .chain(parts.iter().copied())
                    .collect::<Vec<_>>()
                    .join("/")
            },
        );
        let observed = match result {
            Ok(remote) => {
                json!({"infos":remote.infos.iter().map(|i| value(PlistValue::Dict(i.dict().clone()))).collect::<Vec<_>>(), "staticLibraries":remote.static_libraries.iter().map(|(p,r)| json!([p,r])).collect::<Vec<_>>() })
            }
            Err(error) => {
                let failure = error.failure();
                json!({"error":{"class":failure.kind.identifier(), "message":failure.message.as_ref().map(|m| m.to_owned())}})
            }
        };
        let observed: Value = serde_json::from_str(&serde_json::to_string(&observed)?.replace(root_text, "<root>"))?;
        let mut call = call.clone();
        if let Some(error) = call.get_mut("error") {
            marathon_test_support::recorded_errors::error_object(error);
        }
        for key in ["infos", "staticLibraries", "error"] {
            assert_eq!(observed.get(key), call.get(key), "{name}: {key}");
        }
    }
    for witness in row["mach"].as_array().ok_or("mach")? {
        let path = root.path().join(witness["path"].as_str().ok_or("path")?);
        assert_eq!(json!(mach_o::is_static(&path)), witness["static"]);
        let slices: Vec<_> = mach_o::slices(&path).iter().map(|s| json!({"architecture":s.architecture,"uuid":s.uuid.map(|bytes| bytes.iter().map(|b| format!("{b:02x}")).collect::<String>())})).collect();
        assert_eq!(json!(slices), witness["slices"], "{name}: slices");
    }
    Ok(())
}

#[test]
fn lc_uuid_is_compared_within_the_product_s_architecture() -> TestResult {
    replay("LC_UUID is compared within the product's architecture")
}

#[test]
fn a_variant_keeps_the_architecture_of_the_installed_binary() -> TestResult {
    replay("a variant keeps the architecture of the installed binary")
}

#[test]
fn absolute_product_paths_are_not_moved_under_the_xctestrun_directory() -> TestResult {
    replay("absolute product paths are not moved under the xctestrun directory")
}

#[test]
fn bundles_extracted_from_archives_keep_their_targets() -> TestResult {
    replay("bundles extracted from archives keep their targets")
}

#[test]
fn directory_symlinks_inside_bundles_are_not_followed() -> TestResult {
    replay("directory symlinks inside bundles are not followed")
}

#[test]
fn executables_and_dynamic_libraries_are_not_static_products() -> TestResult {
    replay("executables and dynamic libraries are not static products")
}

#[test]
fn format_1_targets_move_to_the_bundles_on_the_worker() -> TestResult {
    replay("format 1 targets move to the bundles on the worker")
}

#[test]
fn format_2_targets_keep_their_source_prefix_and_every_deployed_variant() -> TestResult {
    replay("format 2 targets keep their source prefix and every deployed variant")
}

#[test]
fn frameworks_are_found_in_nested_bundles_the_shallowest_copy_first() -> TestResult {
    replay("frameworks are found in nested bundles, the shallowest copy first")
}

#[test]
fn malformed_load_commands_give_a_slice_without_lc_uuid() -> TestResult {
    replay("malformed load commands give a slice without LC_UUID")
}

#[test]
fn products_missing_from_a_bundle_are_dropped() -> TestResult {
    replay("products missing from a bundle are dropped")
}

#[test]
fn same_named_bundles_are_told_apart_by_their_enclosing_bundles_and_lc_uuid() -> TestResult {
    replay("same-named bundles are told apart by their enclosing bundles and LC_UUID")
}

#[test]
fn static_products_are_pushed_whatever_isstatic_says() -> TestResult {
    replay("static products are pushed whatever IsStatic says")
}

#[test]
fn static_targets_point_at_their_library_pushed_to_the_worker() -> TestResult {
    replay("static targets point at their library pushed to the worker")
}

#[test]
fn the_variant_moved_along_with_a_bundle_wins_over_one_found_by_name() -> TestResult {
    replay("the variant moved along with a bundle wins over one found by name")
}

#[test]
fn versioned_macos_frameworks_keep_their_path_inside_the_framework() -> TestResult {
    replay("versioned macOS frameworks keep their path inside the framework")
}

#[test]
fn xctestrun_built_without_coverage_is_a_configuration_error() -> TestResult {
    replay("xctestrun built without coverage is a configuration error")
}
