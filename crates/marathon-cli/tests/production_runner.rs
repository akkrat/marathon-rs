//! Port of core/src/main/kotlin/com/malinskiy/marathon/di/Modules.kt
//! and cli/src/main/kotlin/com/malinskiy/marathon/cli/ApplicationView.kt (construct, then run).
//!
//! The production runner with the Stub vendor injected (never reachable through the CLI, whose
//! gate rejects the stub vendor): its own core executor thread, the CLI console sink and the
//! real report pipeline.
use jcompat::logging::OutputTarget;
use marathon_cli::{
    VendorFactories,
    application::{CliOutput, CliRunner},
    runner::{ProductionRunner, missing_vendors},
};
use marathon_core::{
    executor::SharedChannel,
    model::{ExecutionCommand, Test},
    vendor::{TestParser, VendorServices},
};
use marathon_test_support::{
    factory::{ConfigurationFactory, MockLogConfigurator, StubTestParser},
    stub_bundle::MockTestBundleIdentifier,
    stub_provider::StubDeviceProvider,
};
use std::{
    io::Write,
    sync::{Arc, Mutex},
};

#[derive(Clone, Default)]
struct Buffer(Arc<Mutex<Vec<u8>>>);
impl Write for Buffer {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(data);
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl Buffer {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

fn stub_factories() -> VendorFactories {
    let mut factories = missing_vendors();
    factories.stub = Box::new(|_| {
        Box::pin(async {
            let test = Test {
                pkg: "test".into(),
                clazz: "SimpleTest".into(),
                method: "test".into(),
                meta_properties: vec![],
            };
            Ok(VendorServices {
                provider: StubDeviceProvider::new(SharedChannel::new()),
                parser: Some(TestParser::Local(Arc::new(StubTestParser(vec![test])))),
                bundle_identifier: Some(Arc::new(MockTestBundleIdentifier)),
                log_configurator: Some(Arc::new(MockLogConfigurator)),
            })
        })
    });
    factories
}

fn output(stdout: &Buffer) -> CliOutput {
    CliOutput::new(
        Box::new(stdout.clone()),
        Box::new(Buffer::default()),
        OutputTarget::Pipe,
        Box::new(|| 0),
    )
}

#[test]
fn stub_parse_runs_end_to_end_through_the_production_runner() {
    let factory = ConfigurationFactory::new();
    let out = factory.output_dir();
    let stdout = Buffer::default();
    let cli = output(&stdout);
    let runner = ProductionRunner {
        factories: Arc::new(stub_factories()),
        cwd: std::env::current_dir().unwrap(),
        environment: vec![],
        signal_hooks: Default::default(),
    };
    let marathon = runner
        .construct(Arc::new(factory.configuration.clone()), cli.logging(), cli.console())
        .unwrap_or_else(|e| panic!("{e:?}"));
    let result = marathon
        .run(ExecutionCommand::Parse {
            output_file_name: Some("parsed".into()),
            include_flaky_tests: false,
        })
        .unwrap();
    assert!(result);
    assert_eq!(
        std::fs::read_to_string(out.join("parsed.yml")).unwrap(),
        "---\ntests:\n- pkg: \"test\"\n  clazz: \"SimpleTest\"\n  method: \"test\"\n  metaProperties: []\n"
    );
    let text = stdout.text();
    assert!(text.contains("System Information:"), "{text}");
    assert!(text.contains("marathon.version: "), "{text}");
    assert!(
        text.contains("DeviceProvider is not required for local parsing"),
        "{text}"
    );
    assert!(text.contains("Parsed 1 tests in "), "{text}");
    assert!(text.contains("Parse execute mode. Result"), "{text}");
}

#[test]
fn a_missing_vendor_factory_fails_construction() {
    let factory = ConfigurationFactory::new();
    let stdout = Buffer::default();
    let cli = output(&stdout);
    let runner = ProductionRunner {
        factories: Arc::new(missing_vendors()),
        cwd: std::env::current_dir().unwrap(),
        environment: vec![],
        signal_hooks: Default::default(),
    };
    let error = match runner.construct(Arc::new(factory.configuration.clone()), cli.logging(), cli.console()) {
        Ok(_) => panic!("construction succeeded"),
        Err(e) => e,
    };
    assert_eq!(
        error.kind.legacy_class().to_owned(),
        "java.lang.UnsupportedOperationException"
    );
    assert_eq!(
        error.message.unwrap().to_owned(),
        "The stub vendor is not supported by this build yet"
    );
}
