#[allow(dead_code)]
#[path = "../examples/corpus.rs"]
mod corpus;
#[test]
fn production_yaml_corpora() -> Result<(), Box<dyn std::error::Error>> {
    for name in ["yaml-corpus", "yaml-review-corpus", "yaml-round3-corpus"] {
        corpus::run(vec![
            "corpus".into(),
            format!("{}/../../fixtures/oracle/{name}.jsonl", env!("CARGO_MANIFEST_DIR")),
        ])?;
    }
    Ok(())
}
