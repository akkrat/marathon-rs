//! Reproduces R2-sol-11's delegated scan costs; run individual large inputs under an external watchdog.
use java_regex_spike::backends::fancy::JavaRegex;
use std::{error::Error, time::Instant};

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let size: usize = args.next().ok_or("usage: scans SIZE [negative]")?.parse()?;
    let pattern = if args.next().as_deref() == Some("negative") {
        r"(?!(?:a*\z))b"
    } else {
        r"(?=a*\z)b"
    };
    let re = JavaRegex::compile(pattern).map_err(|e| format!("{e:?}"))?;
    let input = "a".repeat(size);
    let started = Instant::now();
    let result = re.find(&input);
    println!(
        "{pattern} {size} chars: {result:?}, {:.6} s",
        started.elapsed().as_secs_f64()
    );
    Ok(())
}
