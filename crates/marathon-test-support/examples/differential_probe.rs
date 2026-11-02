//! Fresh native replay capture through the merged scenario driver.
use anyhow::{Result, ensure};
use std::path::Path;
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    ensure!(
        args.len() == 2 && args[0] == "parse",
        "usage: differential_probe parse NEW_DIRECTORY"
    );
    marathon_test_support::differential::capture_parse(Path::new(&args[1]))
}
