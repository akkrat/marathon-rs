//! `shellCommand` (SshjCommandExecutor.kt:79-102, A12, T03) against the actual Kotlin function's output,
//! `fixtures/oracle/ssh/shell_command.jsonl` (`tools/oracle/ssh-probes/run.sh`).
use marathon_apple::cmd::remote::ssh::sshj::shell_command;

#[test]
fn shell_command_is_byte_identical_to_kotlin() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/oracle/ssh/shell_command.jsonl");
    let text = std::fs::read_to_string(path).unwrap();
    let mut t03 = 0;
    for line in text.lines() {
        let row: serde_json::Value = serde_json::from_str(line).unwrap();
        let command: Vec<String> = serde_json::from_value(row["command"].clone()).unwrap();
        assert_eq!(shell_command(&command), row["wire"].as_str().unwrap(), "{command:?}");
        t03 += usize::from(row["case"] == "T03");
    }
    assert_eq!(t03, 19);
}
