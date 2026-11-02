//! Port of vendor/vendor-apple/macos/src/test/kotlin/com/malinskiy/marathon/apple/macos/AppleMacosProviderTest.kt
mod support;
use marathon_apple::apple_device::AppleDevice;
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
        .filter(|r| r.message.to_string_lossy().starts_with("xcresulttool"))
        .collect::<Vec<_>>();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].logger, "AppleMacosProvider");
    assert_eq!(messages[0].level, jcompat::logging::Level::Info);
    assert_eq!(
        messages[0].message.to_string_lossy(),
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
        assert_eq!(e.failure().class.to_string_lossy(), class);
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
    assert_eq!(first.0.info().serial_number, "udid@local".into());
    assert_eq!(first.0.info().operating_system.version, "15.0".into());
    assert_eq!(first.0.abi(), "arm64".into());
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
        let services = v(marathon_macos::construct(h.options.dependencies.clone()).await)?;
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
        jcompat::file_manager::JavaPath::from_text(&h.root.path().to_string_lossy()),
        vec![("LOOP".into(), "${LOOP}".into())],
    );
    let error = p
        .initialize(h.context.clone())
        .await
        .err()
        .ok_or("substitution failure")?;
    assert_eq!(error.failure().class, "java.lang.IllegalStateException".into());
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
        jcompat::file_manager::JavaPath::from_text(&h.root.path().to_string_lossy()),
        vec![],
    );
    let error = tokio::time::timeout(std::time::Duration::from_secs(5), p.initialize(h.context.clone()))
        .await?
        .err()
        .ok_or("connect failure")?;
    assert_eq!(error.failure().class, "java.io.IOException".into());
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
                "class": failure.class.to_string_lossy(),
                "message": failure.message.as_ref().map(|m| m.to_string_lossy().replace(
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
