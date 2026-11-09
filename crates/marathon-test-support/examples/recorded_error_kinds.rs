//! Port of the recorded error fields in tools/oracle/module/src/main/kotlin/oracle/ConfigDump.kt.

use marathon_test_support::recorded_errors::RECORDED_KINDS;
use serde_json::{Map, Value, json};
use std::io::{self, Write};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let values: Map<String, Value> = RECORDED_KINDS
        .iter()
        .map(|(name, kind)| {
            let diagnostic = matches!(kind,
            marathon_error::FailureKind::Config(k) if k.is_json() || matches!(k,
                marathon_error::ConfigKind::Yaml | marathon_error::ConfigKind::Parser |
                marathon_error::ConfigKind::Reader | marathon_error::ConfigKind::Scanner));
            (
                (*name).to_owned(),
                json!({"kind": kind.identifier(), "description": kind.to_string(), "diagnostic": diagnostic}),
            )
        })
        .collect();
    let mut stdout = io::stdout().lock();
    serde_json::to_writer(&mut stdout, &values)?;
    stdout.write_all(b"\n")?;
    Ok(())
}
