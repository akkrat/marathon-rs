use jcompat::writers::{JacksonYamlWriter, WriterValue as V};
use marathon_config::{
    WriterProjection,
    binding::{BindingContext, Profile},
    loader::MarathonConfigurationLoader,
};
use std::{
    error::Error,
    io::{self, Write},
    path::PathBuf,
};
use yaml_writer_prototype::{Projection, write};

fn main() -> Result<(), Box<dyn Error>> {
    let parse = V::Object(vec![(
        "tests".into(),
        V::Array(vec![V::Object(vec![
            ("pkg".into(), "yes".into()),
            ("clazz".into(), "Class".into()),
            ("method".into(), "line one\nline two".into()),
            ("metaProperties".into(), V::Array(vec![])),
        ])]),
    )]);
    let context = BindingContext {
        profile: Profile::Marathonfile,
        marathonfile: PathBuf::from("/work/Marathonfile"),
        marathonfile_dir: PathBuf::from("/work"),
        process_dir: PathBuf::from("/work"),
        environment: vec![],
        now: jcompat::jtime::instant_of(0, 0).ok_or("reference instant")?,
        analytics_tracking: true,
        bugsnag_reporting: true,
    };
    let config = MarathonConfigurationLoader.parse_sync(
        b"name: \"on\"\noutputDir: out\nvendorConfiguration: {type: stub}\nanalyticsConfiguration:\n  type: graphite\n  host: localhost\n  defaults:\n    successRate: \"NaN\"\n", &context,
    ).map_err(|e| format!("{e:?}"))?.writer_value();
    let mut out = io::stdout().lock();
    for (name, value) in [("parse", parse), ("configuration", config)] {
        writeln!(
            out,
            "{name}\nTODAY\n{}SAPHYR\n{}YAML_SERDE\n{}",
            JacksonYamlWriter.write(&value),
            write(&value)?,
            yaml_serde::to_string(&Projection(&value))?
        )?;
    }
    Ok(())
}
