use std::{env, error::Error, fs::File, io, path::PathBuf};

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-changed=src/translate/jdk21-unicode.tsv.gz");
    let mut input = flate2::read::GzDecoder::new(File::open("src/translate/jdk21-unicode.tsv.gz")?);
    let mut output = File::create(PathBuf::from(env::var("OUT_DIR")?).join("jdk21-unicode.tsv"))?;
    io::copy(&mut input, &mut output)?;
    Ok(())
}
