//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/listener/DataContainerClearListener.kt
use super::*;
pub struct DataContainerClearListener(pub ListenerContext);
impl AppleTestRunListener for DataContainerClearListener {
    fn before_test_run(&self) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            let c = &self.0;
            if !c.device.services().vendor.data_container_clear {
                return Ok(());
            }
            let mut apps = indexmap::IndexSet::new();
            for test in &c.batch.tests {
                apps.insert(c.device.services().identifier.bundle(test)?.app_id().to_owned());
            }
            for app in apps {
                c.device.clear_app_container(&app, &c.context).await?;
            }
            Ok(())
        })
    }
}
