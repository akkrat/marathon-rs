//! Port of vendor/vendor-apple/macos/src/test/kotlin/com/malinskiy/marathon/apple/macos/AppleMacosProviderTest.kt
mod support;
use marathon_apple::apple_device::AppleDevice;
use marathon_apple::xctest_parser::ParserLibraries;
use marathon_core::{
    error::VendorError,
    vendor::{DeviceEvent, DeviceProvider},
};
use std::sync::{Arc, atomic::Ordering};
use support::*;

#[tokio::test]
async fn initialize_logs_the_effective_xcresulttool_timeout() -> R {
    let mut h = Harness::new("  timeoutConfiguration:\n    shell: PT5M\n")?;
    Arc::make_mut(&mut h.options.vendor).devices_file = None;
    std::fs::write(h.root.path().join("Marathondevices"), "workers: []\n")?;
    let (p, c) = h.provider();
    assert!(matches!(
        p.initialize(h.context.clone()).await,
        Err(VendorError::NoDevices(_))
    ));
    assert_eq!(c.creates.load(Ordering::SeqCst), 0);
    let messages = h
        .logs
        .records()
        .into_iter()
        .filter(|r| r.message.to_owned().starts_with("xcresulttool"))
        .collect::<Vec<_>>();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].logger, "AppleMacosProvider");
    assert_eq!(messages[0].level, jcompat::logging::Level::Info);
    assert_eq!(
        messages[0].message.to_owned(),
        "xcresulttool timeout is PT5M (timeoutConfiguration.xcresulttool; when not set, the larger of shell PT5M and PT2M)"
    );
    v(p.terminate().await)?;
    Ok(())
}
#[tokio::test]
async fn file_boundaries_and_equal_transport_overwrite() -> R {
    for (input, class) in [
        (None, "java.io.FileNotFoundException"),
        (
            Some("workers: null"),
            "com.malinskiy.marathon.exceptions.NoDevicesException",
        ),
        (
            Some("workers: ["),
            "com.malinskiy.marathon.exceptions.NoDevicesException",
        ),
        (Some("null"), "java.lang.NullPointerException"),
        (
            Some("@"),
            "com.fasterxml.jackson.dataformat.yaml.snakeyaml.error.MarkedYAMLException",
        ),
        (
            Some("workers: []"),
            "com.malinskiy.marathon.exceptions.NoDevicesException",
        ),
    ] {
        let mut h = Harness::new("")?;
        Arc::make_mut(&mut h.options.vendor).devices_file = None;
        if let Some(input) = input {
            std::fs::write(h.root.path().join("Marathondevices"), input)?;
        }
        let (p, _) = h.provider();
        let e = p.initialize(h.context.clone()).await.err().ok_or("expected error")?;
        assert_eq!(e.failure().class.to_owned(), class);
        v(p.terminate().await)?;
    }
    let mut h = Harness::new("")?;
    Arc::make_mut(&mut h.options.vendor).devices_file = None;
    std::fs::write(
        h.root.path().join("Marathondevices"),
        "workers:\n- transport: {type: local}\n  devices: [{type: host}]\n- transport: {type: local}\n  devices: [{type: simulator, udid: ignored}]\n",
    )?;
    let (p, c) = h.provider();
    v(p.initialize(h.context.clone()).await)?;
    assert!(p.snapshot().is_empty());
    assert_eq!(c.creates.load(Ordering::SeqCst), 0);
    assert_eq!(
        h.logs
            .records()
            .iter()
            .filter(|r| r.level == jcompat::logging::Level::Warn)
            .count(),
        1
    );
    v(p.terminate().await)?;
    Ok(())
}
#[tokio::test]
async fn static_host_lifecycle_identity_queue_and_retained_leases() -> R {
    let mut h = Harness::new("")?;
    Arc::make_mut(&mut h.options.vendor).devices_file = None;
    std::fs::write(
        h.root.path().join("Marathondevices"),
        "workers:\n- transport: {type: local}\n  devices: [{type: host}, {type: host}]\n",
    )?;
    let (p, c) = h.provider();
    assert!(Arc::ptr_eq(&p.subscribe(), &p.subscribe()));
    v(p.initialize(h.context.clone()).await)?;
    v(p.join_pending().await)?;
    let first = v(p.borrow(h.context.clone()).await)?;
    let event = v(p.subscribe().receive().await)?.ok_or("event")?;
    let DeviceEvent::Connected(event) = event else {
        return Err("disconnected".into());
    };
    assert_eq!(first, event);
    // D48: a Mac is connected once, so a pool that lost its Macs has nothing to wait for.
    assert_eq!(first.0.return_wait_used_millis(), i64::MAX);
    assert_eq!(first.0.info().serial_number, "udid@local".to_owned());
    assert_eq!(first.0.info().operating_system.version, "15.0".to_owned());
    assert_eq!(first.0.abi(), "arm64".to_owned());
    v(p.initialize(h.context.clone()).await)?;
    v(p.join_pending().await)?;
    let second = v(p.borrow(h.context.clone()).await)?;
    assert_ne!(first, second);
    v(p.terminate().await)?;
    assert_eq!(c.creates.load(Ordering::SeqCst), 2);
    assert_eq!(c.releases.load(Ordering::SeqCst), 1);
    assert!(matches!(
        v(p.subscribe().receive().await)?,
        Some(DeviceEvent::Connected(_))
    ));
    assert!(v(p.subscribe().receive().await)?.is_none());
    v(p.join_pending().await)?;
    Ok(())
}
#[tokio::test]
async fn null_executor_setup_failure_and_udid_fallback() -> R {
    for mode in ["null", "setup-failure", "fallback"] {
        let mut h = Harness::new("")?;
        Arc::make_mut(&mut h.options.vendor).devices_file = None;
        std::fs::write(
            h.root.path().join("Marathondevices"),
            "workers:\n- transport: {type: local}\n  devices: [{type: host}]\n",
        )?;
        let original = h.worker.fake.before.lock().unwrap().clone();
        *h.worker.fake.before.lock().unwrap() = Arc::new(move |cmd| {
            let original = original.clone();
            Box::pin(async move {
                if mode == "fallback" && cmd.iter().any(|s| s.contains("system_profiler")) {
                    return Ok(Some(marathon_test_support::apple::fake_simulator_worker::ok([" "])));
                }
                if mode == "setup-failure" && cmd == ["env"] {
                    return Ok(Some(marathon_apple::cmd::CommandResult {
                        stdout: vec![],
                        stderr: vec![],
                        exit_code: Some(1),
                    }));
                }
                original(cmd).await
            })
        });
        let (p, c) = h.provider();
        c.null.store(mode == "null", Ordering::SeqCst);
        let outcome = p.initialize(h.context.clone()).await;
        if mode == "setup-failure" {
            assert!(matches!(outcome, Err(VendorError::DeviceSetup(_))));
        } else {
            v(outcome)?;
        }
        if mode == "fallback" {
            assert_eq!(p.snapshot()[0].udid(), "fallback");
        }
        v(p.join_pending().await)?;
        v(p.terminate().await)?;
        assert_eq!(c.releases.load(Ordering::SeqCst), usize::from(mode == "fallback"));
    }
    Ok(())
}
#[tokio::test]
async fn factory_construction_is_inert_for_both_parsers() -> R {
    for parser in ["nm", "xctest"] {
        let h = Harness::new(&format!("  testParserConfiguration:\n    type: {parser}\n"))?;
        let supplied = h.root.path().join("supplied-parser");
        std::fs::create_dir_all(supplied.join("MacOSX"))?;
        std::fs::write(
            supplied.join("MacOSX/libxctest-parser.dylib"),
            b"fake stand-in, written by a test\n",
        )?;
        let services = v(marathon_macos::vendor::construct_with_parser_libraries(
            h.options.dependencies.clone(),
            move || {
                assert_eq!(parser, "xctest", "only the xctest parser may look for the library");
                ParserLibraries::new(supplied)
            },
        )
        .await)?;
        assert!(services.bundle_identifier.is_some());
        assert!(services.log_configurator.is_some());
        assert!(matches!(
            services.parser,
            Some(marathon_core::vendor::TestParser::Remote(_))
        ));
        assert_eq!(services.provider.subscribe().state().queued, 0);
        v(services.provider.terminate().await)?;
    }
    Ok(())
}

#[tokio::test]
async fn xctest_construction_fails_without_its_library_and_nm_never_looks_for_it() -> R {
    let h = Harness::new("  testParserConfiguration:\n    type: xctest\n")?;
    for (libraries, problem) in [
        (ParserLibraries::default(), "is not set"),
        (
            ParserLibraries::new(h.root.path().join("no-such-directory")),
            "MacOSX/libxctest-parser.dylib cannot be read",
        ),
    ] {
        let error =
            match marathon_macos::vendor::construct_with_parser_libraries(h.options.dependencies.clone(), || libraries)
                .await
            {
                Ok(_) => panic!("constructed without a library"),
                Err(error) => error,
            };
        let failure = error.failure();
        assert_eq!(
            failure.class.to_owned(),
            "com.malinskiy.marathon.config.exceptions.ConfigurationException"
        );
        let message = failure.message.as_ref().unwrap().to_owned();
        for part in [problem, "MARATHON_XCTEST_PARSER_DIR", "EULA", "nm test parser"] {
            assert!(message.contains(part), "{part} missing from {message}");
        }
    }
    Ok(())
}

#[tokio::test]
async fn substitution_failure_is_outside_mapping_catch_and_initializations_cancel_siblings() -> R {
    use marathon_apple::{
        apple_device_registry::AppleDeviceRegistry,
        device::connection_factory::{Bridge, Executor},
    };
    use marathon_config::devices::Transport;
    use marathon_core::{runtime::VendorFuture, vendor::VendorContext};
    use marathon_macos::provider::MacosConnections;
    let mut h = Harness::new("")?;
    Arc::make_mut(&mut h.options.vendor).devices_file = None;
    std::fs::write(h.root.path().join("Marathondevices"), "workers: ${LOOP}")?;
    let (_, connections) = h.provider();
    let p = marathon_macos::AppleMacosProvider::new(
        h.options.clone(),
        Arc::new(AppleDeviceRegistry::default()),
        connections,
        std::path::PathBuf::from(h.root.path().to_string_lossy().into_owned()),
        vec![("LOOP".into(), "${LOOP}".into())],
    );
    let error = p
        .initialize(h.context.clone())
        .await
        .err()
        .ok_or("substitution failure")?;
    assert_eq!(error.failure().class, "java.lang.IllegalStateException".to_owned());
    v(p.terminate().await)?;
    struct Failing {
        entered: tokio::sync::Notify,
        cancelled: std::sync::atomic::AtomicBool,
    }
    impl MacosConnections for Failing {
        fn create(&self, transport: Transport, context: VendorContext) -> VendorFuture<'_, (Option<Executor>, Bridge)> {
            Box::pin(async move {
                if transport == Transport::Local {
                    self.entered.notified().await;
                    Err(VendorError::Other(marathon_apple::cmd::failure(
                        "java.io.IOException",
                        "connect failure",
                    )))
                } else {
                    self.entered.notify_one();
                    let reason = context.cancellation.cancelled().await;
                    self.cancelled.store(true, Ordering::SeqCst);
                    Err(VendorError::Cancelled(reason))
                }
            })
        }
        fn release(&self, _: Executor) -> VendorFuture<'_, ()> {
            Box::pin(async { Ok(()) })
        }
    }
    std::fs::write(
        h.root.path().join("Marathondevices"),
        "workers:\n- transport: {type: local}\n  devices: [{type: host}]\n- transport: {type: ssh, addr: fake}\n  devices: [{type: host}]\n",
    )?;
    let connections = Arc::new(Failing {
        entered: tokio::sync::Notify::new(),
        cancelled: std::sync::atomic::AtomicBool::new(false),
    });
    let p = marathon_macos::AppleMacosProvider::new(
        h.options.clone(),
        Arc::new(AppleDeviceRegistry::default()),
        connections.clone(),
        std::path::PathBuf::from(h.root.path().to_string_lossy().into_owned()),
        vec![],
    );
    let error = tokio::time::timeout(std::time::Duration::from_secs(60), p.initialize(h.context.clone()))
        .await?
        .err()
        .ok_or("connect failure")?;
    assert_eq!(error.failure().class, "java.io.IOException".to_owned());
    assert!(connections.cancelled.load(Ordering::SeqCst));
    v(p.terminate().await)?;
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn devices_read_error_chains_match_jvm_oracle() -> R {
    use std::os::unix::fs::PermissionsExt;
    for mode in ["directory-read", "permission-denied-read"] {
        let mut h = Harness::new("")?;
        Arc::make_mut(&mut h.options.vendor).devices_file = None;
        let path = h.root.path().join("Marathondevices");
        if mode == "directory-read" {
            std::fs::create_dir(&path)?;
        } else {
            std::fs::write(&path, "workers: []")?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o0))?;
        }
        let (p, c) = h.provider();
        let outcome = p.initialize(h.context.clone()).await;
        if mode == "permission-denied-read" {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        }
        let error = outcome
            .err()
            .ok_or("expected read error (requires unprivileged user)")?;
        let mut chain = Vec::new();
        let mut current = Some(error.failure());
        while let Some(failure) = current {
            chain.push(serde_json::json!({
                "class": failure.class.to_owned(),
                "message": failure.message.as_ref().map(|m| m.to_owned().replace(
                    &h.root.path().to_string_lossy().to_string(), &format!("$ROOT/{mode}"))),
            }));
            current = failure.cause.as_deref();
        }
        let oracle: serde_json::Value = include_str!("../../../fixtures/oracle/macos/probes.jsonl")
            .lines()
            .map(serde_json::from_str::<serde_json::Value>)
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .find(|row| row["case"] == mode)
            .ok_or("read oracle")?;
        assert_eq!(serde_json::json!(chain), oracle["error"], "{mode}");
        assert_eq!(c.creates.load(Ordering::SeqCst), 0);
        v(p.terminate().await)?;
    }
    Ok(())
}

#[tokio::test]
async fn setup_on_a_connection_lost_before_the_last_probe_fails_instead_of_using_the_default_abi_d36() -> R {
    use marathon_apple::cmd::remote::ssh::sshj::error::CONNECTION_EXCEPTION;
    let h = Harness::new("")?;
    let d = h.device();
    let worker = Arc::downgrade(&h.worker);
    let original = h.worker.fake.before.lock().unwrap().clone();
    *h.worker.fake.before.lock().unwrap() = Arc::new(move |cmd| {
        if cmd.first().is_some_and(|c| c == "uname")
            && let Some(worker) = worker.upgrade()
        {
            // What the real executor does: the transport is gone, and safeExecute turns the failure into null.
            worker.fake.connected.store(false, Ordering::SeqCst);
            return Box::pin(async {
                Err(VendorError::Other(marathon_apple::cmd::failure(
                    CONNECTION_EXCEPTION,
                    "Not connected",
                )))
            });
        }
        original(cmd)
    });
    let error = d.setup(&h.context).await.unwrap_err();
    assert_eq!(
        (
            error.failure().class.to_owned(),
            error.failure().message.as_ref().unwrap().to_owned()
        ),
        (CONNECTION_EXCEPTION.to_owned(), "Not connected".to_owned())
    );
    Ok(())
}

#[tokio::test]
async fn a_failed_abi_probe_on_a_live_connection_still_gives_the_default_abi() -> R {
    use marathon_core::vendor::Device;
    let h = Harness::new("")?;
    let d = h.device();
    let original = h.worker.fake.before.lock().unwrap().clone();
    *h.worker.fake.before.lock().unwrap() = Arc::new(move |cmd| {
        if cmd.first().is_some_and(|c| c == "uname") {
            return Box::pin(async {
                Err(VendorError::Other(marathon_apple::cmd::failure(
                    marathon_apple::cmd::remote::ssh::sshj::error::CONNECTION_EXCEPTION,
                    "Request failed",
                )))
            });
        }
        original(cmd)
    });
    v(d.setup(&h.context).await)?;
    assert_eq!(d.abi().to_owned(), "Unknown");
    Ok(())
}

/// Hands out its workers in turn, one per transport.
struct Workers {
    workers: Vec<Arc<Worker>>,
    bridge: Arc<RecordingBridge>,
    next: std::sync::atomic::AtomicUsize,
}
impl marathon_macos::provider::MacosConnections for Workers {
    fn create(
        &self,
        _: marathon_config::devices::Transport,
        _: marathon_core::vendor::VendorContext,
    ) -> marathon_core::runtime::VendorFuture<
        '_,
        (
            Option<marathon_apple::device::connection_factory::Executor>,
            marathon_apple::device::connection_factory::Bridge,
        ),
    > {
        Box::pin(async move {
            let worker = self.workers[self.next.fetch_add(1, Ordering::SeqCst)].clone();
            Ok((
                Some(worker as marathon_apple::device::connection_factory::Executor),
                self.bridge.clone() as marathon_apple::device::connection_factory::Bridge,
            ))
        })
    }
    fn release(
        &self,
        _: marathon_apple::device::connection_factory::Executor,
    ) -> marathon_core::runtime::VendorFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

/// D53 (A103, U171): remote parsing's borrow avoids a Mac it has tried and, as iOS's D44, takes only a Mac whose
/// connection is up, since nothing reconnects a lost one. With every Mac tried it takes any; with none connected it waits.
#[tokio::test]
async fn borrow_untried_avoids_a_tried_mac_and_takes_only_a_connected_one_d53() -> R {
    use marathon_core::vendor::DeviceRef;
    let h = Harness::new("")?;
    let other = Harness::new("")?;
    let original = other.worker.fake.before.lock().unwrap().clone();
    *other.worker.fake.before.lock().unwrap() = Arc::new(move |cmd| {
        if cmd.iter().any(|s| s.contains("system_profiler")) {
            return Box::pin(async {
                Ok(Some(marathon_test_support::apple::fake_simulator_worker::ok([
                    "Provisioning UDID: other",
                ])))
            });
        }
        original(cmd)
    });
    let p = marathon_macos::AppleMacosProvider::new(
        h.options.clone(),
        Arc::new(marathon_apple::apple_device_registry::AppleDeviceRegistry::default()),
        Arc::new(Workers {
            workers: vec![h.worker.clone(), other.worker.clone()],
            bridge: h.bridge.clone(),
            next: std::sync::atomic::AtomicUsize::new(0),
        }),
        std::path::PathBuf::from(h.root.path().to_string_lossy().into_owned()),
        vec![],
    );
    for _ in 0..2 {
        v(
            p.initialize_transport(marathon_config::devices::Transport::Local, h.context.clone())
                .await,
        )?;
    }
    let macs = p.snapshot();
    assert_eq!(macs.len(), 2);
    let (a, b) = (DeviceRef(macs[0].clone()), DeviceRef(macs[1].clone()));
    let (key_a, key_b) = (a.0.info().serial_number, b.0.info().serial_number);
    assert_ne!(key_a, key_b);
    let mut keys = std::collections::HashSet::new();
    for _ in 0..40 {
        let (device, key) = v(p.borrow_untried(vec![], h.context.clone()).await)?;
        assert_eq!(device.0.info().serial_number, key);
        keys.insert(key);
    }
    assert_eq!(keys.len(), 2);
    for _ in 0..20 {
        let (device, key) = v(p.borrow_untried(vec![key_a.clone()], h.context.clone()).await)?;
        assert_eq!((device, key), (b.clone(), key_b.clone()));
    }
    let mut keys = std::collections::HashSet::new();
    for _ in 0..40 {
        keys.insert(
            v(p.borrow_untried(vec![key_a.clone(), key_b.clone()], h.context.clone())
                .await)?
            .1,
        );
    }
    assert_eq!(keys.len(), 2, "every Mac tried: the pick is among all of them");
    // b's connection is lost: only a is left, whatever was tried.
    other.worker.fake.connected.store(false, Ordering::SeqCst);
    for _ in 0..20 {
        assert_eq!(v(p.borrow(h.context.clone()).await)?, a);
        assert_eq!(v(p.borrow_untried(vec![key_a.clone()], h.context.clone()).await)?.0, a);
    }
    // None connected: borrow waits, as for no Mac at all, and takes one that is connected again.
    h.worker.fake.connected.store(false, Ordering::SeqCst);
    let waiting = {
        let (p, context) = (p.clone(), h.context.clone());
        tokio::spawn(async move { p.borrow(context).await })
    };
    tokio::time::sleep(std::time::Duration::from_millis(450)).await;
    assert!(!waiting.is_finished(), "borrow returned a Mac whose connection is lost");
    h.worker.fake.connected.store(true, Ordering::SeqCst);
    let borrowed = v(tokio::time::timeout(std::time::Duration::from_secs(60), waiting).await)?;
    assert_eq!(v(v(borrowed)?)?, a);
    other.worker.fake.connected.store(true, Ordering::SeqCst);
    v(p.terminate().await)?;
    v(p.join_pending().await)?;
    Ok(())
}
