//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/bin/codesign/Codesign.kt

use super::{Tools, argv, device_setup};
use crate::cmd::CommandResult;
use marathon_core::{error::VendorResult, vendor::VendorContext};

pub struct Codesign {
    tools: Tools,
}
impl Codesign {
    pub fn new(tools: Tools) -> Self {
        Self { tools }
    }
    pub async fn get_identity_for(&self, path: &str, context: &VendorContext) -> VendorResult<String> {
        self.get_property(path, "Authority", context).await
    }
    pub async fn get_development_team(&self, path: &str, context: &VendorContext) -> VendorResult<String> {
        self.get_property(path, "TeamIdentifier", context).await
    }
    /// Kept: the argument list starts with a second `codesign` token (A38).
    pub async fn sign(
        &self,
        bundle_path: &str,
        entitlements_path: Option<&str>,
        identity: Option<&str>,
        context: &VendorContext,
    ) -> VendorResult<()> {
        let identity = match identity {
            Some(identity) => identity.to_owned(),
            None => self.get_identity_for(bundle_path, context).await?,
        };
        let mut args = argv(["codesign", "-f"]);
        match entitlements_path {
            None => args.push("--preserve-metadata=identifier,entitlements".into()),
            Some(path) => args.extend(["--entitlements".into(), path.to_owned()]),
        }
        args.extend(["--timestamp=none".into(), "-s".into(), identity, bundle_path.to_owned()]);
        self.exec(args, context).await.map(|_| ())
    }
    /// Kept: `codesign -dvv` reports on stderr, but this reads stdout (A39).
    async fn get_property(&self, path: &str, property: &str, context: &VendorContext) -> VendorResult<String> {
        let error = || device_setup(format!("failed to extract {property} from {path}"));
        let stdout = self
            .exec(argv(["-dvv", path]), context)
            .await?
            .successful_or_null()
            .map(|result| result.stdout)
            .ok_or_else(error)?;
        let prefix = format!("{property}=");
        let line = stdout.iter().find(|line| line.starts_with(&prefix)).ok_or_else(error)?;
        Ok(line[prefix.len()..].to_owned())
    }
    async fn exec(&self, args: Vec<String>, context: &VendorContext) -> VendorResult<CommandResult> {
        let mut command = argv(["codesign"]);
        command.extend(args);
        self.tools.critical(command, self.tools.timeouts.shell, context).await
    }
}
