//! Port of core/src/main/kotlin/com/malinskiy/marathon/log/MarathonLogging.kt (native event configuration).

use std::sync::Arc;
use tracing::level_filters::LevelFilter;
use tracing_subscriber::filter::Targets;
pub use tracing::Level;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputTarget { Terminal, Pipe, File }

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Vendor {
    Startup,
    Android,
    Apple { compact_output: bool },
}

/// Stable Rust targets replace logger registration order and inherited JVM package levels.
pub fn filter(configurator: Vendor, debug: bool) -> Targets {
    let ordinary = if debug || matches!(configurator, Vendor::Startup) {
        LevelFilter::DEBUG
    } else {
        LevelFilter::ERROR
    };
    let mut targets = Targets::new()
        .with_default(LevelFilter::ERROR)
        .with_targets(
            [
                "marathon_cli",
                "marathon_core",
                "marathon_config",
                "marathon_android",
                "marathon_apple",
                "marathon_ios",
                "marathon_macos",
                "marathon_analytics",
                "marathon_dex",
                "marathon_test_support",
            ]
            .map(|target| (target, ordinary)),
        )
        .with_target("marathon_cli::application", LevelFilter::DEBUG)
        .with_target("marathon_adb", LevelFilter::DEBUG)
        .with_target("marathon_core::analytics::factory", LevelFilter::DEBUG)
        .with_target("marathon_core::analytics::provider", LevelFilter::DEBUG)
        .with_target("marathon_core::analytics::noop", LevelFilter::DEBUG)
        .with_target("marathon_core::reporters::timeline", LevelFilter::DEBUG)
        .with_target("marathon_apple::cmd::remote::ssh::sshj::wire", LevelFilter::DEBUG)
        .with_target("marathon_core::timing", LevelFilter::INFO)
        .with_target("marathon_ios::device::timing", LevelFilter::INFO)
        .with_target("marathon_apple::xcodebuild_stderr", LevelFilter::WARN);
    if matches!(configurator, Vendor::Apple { .. }) {
        targets = targets.with_target("marathon_core::reporters::timeline", LevelFilter::ERROR);
    }
    targets
}

/// Identifies an event producer; it carries no mutable logger level or formatting state.
#[derive(Clone, Debug)]
pub struct Component { name: String, target: String, wire_debug: Option<bool> }
impl Component {
    pub fn name(&self) -> &str { &self.name }
    pub fn target(&self) -> &str { &self.target }
    pub fn wire(debug: bool) -> Self { Self { name: "marathon_apple::cmd::remote::ssh::sshj::wire".into(), target: "marathon_apple::cmd::remote::ssh::sshj::wire".into(), wire_debug: Some(debug) } }
}

pub type Configure = Arc<dyn Fn(bool, Vendor) + Send + Sync>;
#[derive(Clone)]
pub struct LogSettings {
    pub debug: bool,
    vendor: Vendor,
    configure: Option<Configure>,
}
impl Default for LogSettings {
    fn default() -> Self { Self { debug: true, vendor: Vendor::Startup, configure: None } }
}
impl std::fmt::Debug for LogSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LogSettings").field("debug", &self.debug).field("vendor", &self.vendor).finish()
    }
}
impl LogSettings {
    pub fn with_configure(configure: Configure) -> Self { Self { configure: Some(configure), ..Self::default() } }
    #[track_caller]
    pub fn component(&self, name: impl Into<String>) -> Component {
        let name = name.into();
        let target = if name.starts_with("marathon_") { name.clone() } else { module_target(std::panic::Location::caller().file()) };
        Component { name, target, wire_debug: None }
    }
    pub fn configure(&mut self, debug: bool, vendor: Vendor) {
        self.debug = debug; self.vendor = vendor;
        if let Some(configure) = &self.configure { configure(debug, vendor); }
    }
    pub fn vendor(&self) -> Vendor { self.vendor }
    pub fn enabled(&self, component: &Component, level: Level) -> bool { component.wire_debug.is_none_or(|debug| debug || level == Level::ERROR) && self.enabled_target(component.target(), level) }
    pub fn enabled_target(&self, target: &str, level: Level) -> bool { filter(self.vendor, self.debug).would_enable(target, &level) }
    pub fn timings(&self) -> Component { self.component("marathon_core::timing") }
    pub fn xcodebuild(&self) -> Component { self.component("marathon_apple::xcodebuild_stderr") }
    pub fn timing_message(&self, elapsed: impl FnOnce() -> Option<i64>, message: impl FnOnce() -> String) -> Option<String> {
        self.enabled(&self.timings(), Level::INFO).then(|| { let text = message(); match elapsed() { Some(elapsed) => format!("{text} (+{elapsed} ms since start)"), None => text } })
    }
    pub fn xcodebuild_message(&self, device: &str, line: &str) -> Option<String> {
        self.enabled(&self.xcodebuild(), Level::WARN).then(|| format!("{device}: stderr={line}"))
    }
}

/// Bridges owned records drained by an I/O task into native tracing events.
/// Metadata is interned off core; targets are Rust module paths supplied by producers.
pub fn emit(target: &str, level: Level, message: &str, error: Option<&str>, component: Option<&str>) {
    use std::{collections::HashMap, sync::{Mutex, OnceLock}};
    use tracing::{Metadata, callsite::{Callsite, Identifier}, field::{FieldSet, Value}, subscriber::Interest};
    struct Site(OnceLock<Metadata<'static>>);
    impl Callsite for Site {
        fn set_interest(&self, _: Interest) {}
        fn metadata(&self) -> &Metadata<'_> { self.0.get().unwrap_or_else(|| unreachable!("metadata is initialized before registration")) }
    }
    static SITES: OnceLock<Mutex<HashMap<(String, Level), &'static Metadata<'static>>>> = OnceLock::new();
    let metadata = {
        let mut sites = SITES.get_or_init(Mutex::default).lock().unwrap_or_else(|e| e.into_inner());
        *sites.entry((target.to_owned(), level)).or_insert_with(|| {
            let site: &'static Site = Box::leak(Box::new(Site(OnceLock::new())));
            let target = Box::leak(target.to_owned().into_boxed_str());
            let fields = FieldSet::new(&["message", "error", "component"], Identifier(site));
            let metadata = Metadata::new("queued event", target, level, None, None, None, fields, tracing::metadata::Kind::EVENT);
            let _ = site.0.set(metadata);
            tracing::callsite::register(site);
            site.metadata()
        })
    };
    tracing::dispatcher::get_default(|dispatch| {
        if !dispatch.enabled(metadata) { return; }
        let fields = metadata.fields();
        let values: [Option<&dyn Value>; 3] = [Some(&message), error.as_ref().map(|v| v as &dyn Value), component.as_ref().map(|v| v as &dyn Value)];
        let values = fields.value_set_all(&values);
        let event = tracing::Event::new(metadata, &values);
        dispatch.event(&event);
    });
}

/// Derive a Rust module identity from the compiler's source location, without filesystem access.
pub fn module_target(file: &str) -> String {
    let Some((_, relative)) = file.rsplit_once("crates/") else { return "marathon_core::logging".into() };
    let Some((crate_name, module)) = relative.split_once("/src/") else { return "marathon_core::logging".into() };
    let module = module.strip_suffix(".rs").unwrap_or(module);
    let module = module.strip_suffix("/mod").unwrap_or(module);
    let mut target = crate_name.replace('-', "_");
    if !matches!(module, "lib" | "mod") { target.push_str("::"); target.push_str(&module.replace('/', "::")); }
    target
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    fn directives(vendor: Vendor, debug: bool, ordinary: LevelFilter, timeline: LevelFilter) {
        let mut expected: BTreeMap<&str, LevelFilter> = [
            "marathon_cli", "marathon_core", "marathon_config", "marathon_android", "marathon_apple", "marathon_ios", "marathon_macos", "marathon_analytics", "marathon_dex", "marathon_test_support",
        ].into_iter().map(|target| (target, ordinary)).collect();
        expected.extend([
            ("marathon_cli::application", LevelFilter::DEBUG),
            ("marathon_adb", LevelFilter::DEBUG),
            ("marathon_core::analytics::factory", LevelFilter::DEBUG),
            ("marathon_core::analytics::provider", LevelFilter::DEBUG),
            ("marathon_core::analytics::noop", LevelFilter::DEBUG),
            ("marathon_core::reporters::timeline", timeline),
            ("marathon_apple::cmd::remote::ssh::sshj::wire", LevelFilter::DEBUG),
            ("marathon_core::timing", LevelFilter::INFO),
            ("marathon_ios::device::timing", LevelFilter::INFO),
            ("marathon_apple::xcodebuild_stderr", LevelFilter::WARN),
        ]);
        let actual = filter(vendor, debug);
        assert_eq!(actual.default_level(), Some(LevelFilter::ERROR));
        assert_eq!(actual.iter().collect::<BTreeMap<_, _>>(), expected);
        for (target, threshold) in expected {
            for level in [Level::TRACE, Level::DEBUG, Level::INFO, Level::WARN, Level::ERROR] {
                assert_eq!(actual.would_enable(&format!("{target}::child"), &level), level <= threshold, "{target} {level}");
            }
        }
    }
    #[test] fn startup_debug_false() { directives(Vendor::Startup, false, LevelFilter::DEBUG, LevelFilter::DEBUG); }
    #[test] fn startup_debug_true() { directives(Vendor::Startup, true, LevelFilter::DEBUG, LevelFilter::DEBUG); }
    #[test] fn android_debug_false() { directives(Vendor::Android, false, LevelFilter::ERROR, LevelFilter::DEBUG); }
    #[test] fn android_debug_true() { directives(Vendor::Android, true, LevelFilter::DEBUG, LevelFilter::DEBUG); }
    #[test] fn apple_regular_debug_false() { directives(Vendor::Apple { compact_output: false }, false, LevelFilter::ERROR, LevelFilter::ERROR); }
    #[test] fn apple_regular_debug_true() { directives(Vendor::Apple { compact_output: false }, true, LevelFilter::DEBUG, LevelFilter::ERROR); }
    #[test] fn apple_compact_debug_false() { directives(Vendor::Apple { compact_output: true }, false, LevelFilter::ERROR, LevelFilter::ERROR); }
    #[test] fn apple_compact_debug_true() { directives(Vendor::Apple { compact_output: true }, true, LevelFilter::DEBUG, LevelFilter::ERROR); }
}
