use jcompat::StringSubstitutor;
use serde_json::{Value, json};

#[test]
fn string_substitutor_jvm_truth_table() {
    check_table(include_str!(
        "../../../fixtures/oracle/jcompat/string_substitutor.jsonl"
    ));
}

#[test]
fn string_substitutor_jvm_boundaries() {
    check_table(include_str!(
        "../../../fixtures/oracle/jcompat/string_substitutor_edges.jsonl"
    ));
}

fn check_table(table: &str) {
    let mut lines = table.lines();
    let header: Value = serde_json::from_str(lines.next().unwrap()).unwrap();
    let mut count = 0;
    for line in lines {
        let row: Value = serde_json::from_str(line).unwrap();
        let lookup = StringSubstitutor::new(|key: &str| row["env"][key].as_str().map(str::to_owned));
        let actual = match lookup.replace(row["input"].as_str().unwrap()) {
            Ok(value) => json!({"ok": value}),
            Err(error) => {
                json!({"error": {"class": marathon_error::FailureKind::IllegalState.identifier(), "message": error.to_string()}})
            }
        };
        let mut expected = row["result"].clone();
        if let Some(class) = expected.pointer_mut("/error/class") {
            *class = json!(marathon_test_support::recorded_errors::kind(class.as_str().unwrap()).identifier());
        }
        assert_eq!(actual, expected, "row {}: {line}", count + 1);
        count += 1;
    }
    assert_eq!(count, header["rows"].as_u64().unwrap());
}
