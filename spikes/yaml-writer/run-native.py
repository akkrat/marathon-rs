#!/usr/bin/env python3
"""Temporarily wire the milestone-1 emitter into the real output paths, then restore."""
import os
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[2]


def run(*args):
    subprocess.run(args, cwd=ROOT, check=True)


def replace(path, old, new):
    text = path.read_text()
    if text.count(old) != 1:
        raise RuntimeError(f"prototype anchor changed: {path.relative_to(ROOT)}")
    path.write_text(text.replace(old, new))


def main():
    if os.environ.get("MARATHON_BLESS_CONFIG") == "1":
        raise RuntimeError("golden blessing must be disabled")
    if "MARATHON_KT" not in os.environ:
        raise RuntimeError("set MARATHON_KT before running the prototype")
    paths = [ROOT / path for path in [
        "Cargo.lock", "crates/marathon-core/Cargo.toml", "crates/marathon-config/Cargo.toml",
        "crates/marathon-core/src/report/projection.rs", "crates/marathon-core/src/execution/parse_command.rs",
        "crates/marathon-config/src/render.rs", "crates/marathon-cli/src/config_dump.rs",
    ]]
    original = {path: path.read_bytes() for path in paths}
    try:
        for crate in ["marathon-core", "marathon-config"]:
            path = ROOT / f"crates/{crate}/Cargo.toml"
            extra = '\nserde-saphyr = { version = "=2.1.1", default-features = false, features = ["serialize"] }'
            if crate == "marathon-config":
                extra += '\nserde = { version = "=1.0.229", features = ["derive"] }'
            replace(path, "[dependencies]", "[dependencies]" + extra)
        replace(ROOT / "crates/marathon-core/src/report/projection.rs",
                "pub fn parse_results(tests: &[Test], flaky_tests: &[Test]) -> String {",
                '#[path = "../../../../spikes/yaml-writer/src/lib.rs"]\nmod yaml_prototype;\n'
                'pub fn parse_results(tests: &[Test], flaky_tests: &[Test]) -> Result<String, serde_saphyr::ser_error::Error> {')
        replace(ROOT / "crates/marathon-core/src/report/projection.rs",
                "jcompat::writers::JacksonYamlWriter.write(&object(fields))", "yaml_prototype::write(&object(fields))")
        replace(ROOT / "crates/marathon-core/src/execution/parse_command.rs",
                "let result = parse_results(tests, flaky_tests);",
                "let result = parse_results(tests, flaky_tests).map_err(|e| "
                "crate::error::VendorError::Other(crate::error::Failure::from_kind("
                "marathon_error::FailureKind::Io, Some(e.to_string()))))?;")
        replace(ROOT / "crates/marathon-config/src/render.rs",
                "pub fn serialize(configuration: &Configuration) -> String {\n    JacksonYamlWriter.write(&configuration.writer_value())\n}",
                '#[path = "../../../spikes/yaml-writer/src/lib.rs"]\nmod yaml_prototype;\n'
                'pub fn serialize(configuration: &Configuration) -> Result<String, serde_saphyr::ser_error::Error> {\n'
                '    yaml_prototype::write(&configuration.writer_value())\n}')
        replace(ROOT / "crates/marathon-config/src/render.rs",
                "writers::{JacksonYamlWriter, JavaNumber, KotlinValue, WriterValue}",
                "writers::{JavaNumber, KotlinValue, WriterValue}")
        # This hook makes each accepted corpus config use the prototype before dumping.
        # Reusing the same context keeps environment, instant, and path resolution fixed.
        replace(ROOT / "crates/marathon-cli/src/config_dump.rs",
                "MarathonConfigurationLoader\n            .parse_sync(&bytes, &context)\n            .map_err(|e| configuration_failure(&e))",
                "let config = MarathonConfigurationLoader.parse_sync(&bytes, &context)"
                ".map_err(|e| configuration_failure(&e))?;\n"
                "        let yaml = marathon_config::serialize(&config)"
                ".map_err(|e| failure(FailureKind::Io, &e.to_string()))?;\n"
                "        let recreated = MarathonConfigurationLoader.parse_sync(yaml.as_bytes(), &context)"
                ".map_err(|e| configuration_failure(&e))?;\n"
                '        if config != recreated { return Err(failure(FailureKind::IllegalState, "prototype configuration round trip changed data")); }\n'
                "        Ok(recreated)")
        # Cargo does not relink the unchanged helper after the lockfile changes.
        # The differential comparator requires a helper newer than that lockfile.
        run("cargo", "clean", "-p", "snakeyaml")
        run("cargo", "build", "-q", "-p", "snakeyaml", "--example", "yaml_tree")
        run("cargo", "test", "-p", "marathon-test-support", "--test", "differential_parse", "--test", "differential_local")
        run("cargo", "test", "-p", "marathon-cli", "--test", "config_contract", "--test", "differential_vendors")
    finally:
        for path, content in original.items():
            path.write_bytes(content)
        run("cargo", "clean", "-p", "snakeyaml")
        print("Restored production sources, manifests and workspace lockfile", flush=True)


if __name__ == "__main__":
    main()
