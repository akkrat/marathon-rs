//! Port of configuration/src/main/kotlin/com/malinskiy/marathon/config/serialization/ConfigurationFactory.kt
//! and configuration/src/main/kotlin/com/malinskiy/marathon/config/serialization/yaml/SerializeModule.kt.
//!
//! Source locations and nodes are shared with the production YAML parser.

use crate::{Configuration, MarathonDevices, VendorConfiguration};
use jcompat::jtime::Instant;
use std::path::PathBuf;
use std::{future::Future, pin::Pin};

pub type BindingFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, ConfigurationError>> + Send + 'a>>;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    Marathonfile,
    Marathondevices,
    OutputYaml,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValidationStage {
    Parse,
    Logical,
    Provider,
}
pub use snakeyaml::node::{Mark as SourceMark, Mark, Node, NodeKind, Span as SourceSpan, Span};
#[derive(Clone, Debug)]
pub struct TokenError {
    pub stage: String,
    pub class: String,
    pub message: String,
    pub context_mark: Option<SourceMark>,
    pub problem_mark: Option<SourceMark>,
    pub jackson_location: Option<SourceMark>,
}
#[derive(Clone, Debug)]
pub enum PathPart {
    Key(String),
    Index(usize),
}
#[derive(Clone, Debug)]
pub enum BindingCause {
    Stream(TokenError),
    Coercion(String),
    Custom(String),
}
#[derive(Clone, Debug)]
pub struct ConfigurationError {
    pub stage: ValidationStage,
    pub class: String,
    pub message: Option<String>,
    /// Nested exception classes/messages in outermost-first order (config-dump).
    pub causes: Vec<ConfigurationCause>,
    pub path: Vec<PathPart>,
    pub span: Option<SourceSpan>,
    pub cause: BindingCause,
}
#[derive(Clone, Debug)]
pub struct ConfigurationCause {
    pub class: String,
    pub message: Option<String>,
}
/// P1 resolves File against marathonfile_dir; P2 uses process_dir for auth keys.
/// Environment order/spelling and injectable now are inputs, not global lookups.
pub struct BindingContext {
    pub profile: Profile,
    pub marathonfile: PathBuf,
    pub analytics_tracking: bool,
    pub bugsnag_reporting: bool,
    pub marathonfile_dir: PathBuf,
    pub process_dir: PathBuf,
    pub environment: Vec<(String, String)>,
    pub now: Instant,
}
pub trait ConfigurationLoader: Send + Sync {
    /// Complete stage-(a) parse, including vendor post-processing/extraction and
    /// CLI tracking-flag merge. Logical validation is a separate stage-(b) call.
    fn parse<'a>(&'a self, bytes: &'a [u8], context: &'a BindingContext) -> BindingFuture<'a, Configuration>;
    fn devices<'a>(&'a self, bytes: &'a [u8], context: &'a BindingContext) -> BindingFuture<'a, MarathonDevices>;
}
/// Called at source-defined stages. Post-processing may extract/delete files;
/// implementations run off core. Logical validation deliberately skips macOS's
/// validate method, as in Kotlin LogicalConfigurationValidator (F6).
pub trait VendorConfigurationHooks: Send + Sync {
    fn post_process<'a>(
        &'a self,
        config: &'a mut VendorConfiguration,
        context: &'a BindingContext,
    ) -> BindingFuture<'a, ()>;
    fn validate<'a>(&'a self, config: &'a VendorConfiguration) -> BindingFuture<'a, ()>;
}
pub trait ConfigurationValidator: Send + Sync {
    fn validate<'a>(&'a self, config: &'a Configuration) -> BindingFuture<'a, ()>;
}
