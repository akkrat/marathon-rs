//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/bin/lipo/Lipo.kt

use super::{Tools, argv, device_setup, kt};
use crate::{cmd::CommandResult, model::Arch};
use marathon_core::{error::VendorResult, vendor::VendorContext};

pub struct Lipo {
    tools: Tools,
}
impl Lipo {
    pub fn new(tools: Tools) -> Self {
        Self { tools }
    }
    pub async fn get_arch(&self, path: &str, context: &VendorContext) -> VendorResult<Vec<Arch>> {
        let stdout = self
            .exec(argv([path, "-archs"]), context)
            .await?
            .successful_or_null()
            .map(|result| result.combined_stdout())
            .ok_or_else(|| device_setup(format!("failed to detect binary format for {path}")))?;
        // Split on single spaces only; a line break stays inside a token.
        stdout
            .split(' ')
            .map(kt::trim)
            .filter(|token| !kt::is_blank(token))
            .map(|raw| {
                Arch::ALL.into_iter().find(|arch| arch.value() == raw).ok_or_else(|| {
                    let supported = Arch::ALL.map(Arch::value).join(", ");
                    device_setup(format!("Unknown arch={raw}, supported values are [{supported}]"))
                })
            })
            .collect()
    }
    pub async fn remove_arch(&self, path: &str, arch: Arch, context: &VendorContext) -> VendorResult<()> {
        self.exec(argv([path, "-remove", arch.value(), "-output", path]), context)
            .await
            .map(|_| ())
    }
    async fn exec(&self, args: Vec<String>, context: &VendorContext) -> VendorResult<CommandResult> {
        let mut command = argv(["lipo"]);
        command.extend(args);
        self.tools.critical(command, self.tools.timeouts.shell, context).await
    }
}
