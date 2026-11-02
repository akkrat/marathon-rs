//! Subprocess isolation turns stack overflow into an observable test failure.
#[test]
fn fifty_thousand_flow_levels_do_not_overflow() -> Result<(), Box<dyn std::error::Error>> {
    let output = std::process::Command::new(std::env::current_exe()?)
        .args(["--exact", "depth_child", "--nocapture"])
        .env("MARATHON_YAML_DEPTH_CHILD", "1")
        .output()?;
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Ok(())
}
#[test]
fn depth_child() {
    if std::env::var_os("MARATHON_YAML_DEPTH_CHILD").is_none() {
        return;
    }
    for closed in [false, true] {
        let text = format!(
            "{}{}",
            "[".repeat(50_000),
            if closed {
                format!("0{}", "]".repeat(50_000))
            } else {
                String::new()
            }
        );
        let stream = snakeyaml::node::load(&text);
        let tokens = snakeyaml::tokens::tokens(&stream);
        assert_eq!(tokens.tokens.len(), 1000);
        assert_eq!(
            tokens.error.unwrap().class,
            snakeyaml::resolve::STREAM_CONSTRAINTS_EXCEPTION
        );
    }
}
