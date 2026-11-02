//! Port of core/src/main/kotlin/com/malinskiy/marathon/analytics/metrics/remote/RemoteDataSource.kt
pub mod graphite;
pub mod influx;
pub mod influx2;

use crate::runtime::ServiceFuture;
use jcompat::{jtime::Instant, writers::JavaString};

#[derive(Clone, Debug, PartialEq)]
pub struct ExecutionTime {
    pub test_name: JavaString,
    pub percentile: f64,
}
#[derive(Clone, Debug, PartialEq)]
pub struct SuccessRate {
    pub test_name: JavaString,
    pub mean: f64,
}
pub trait RemoteDataSource: Send + Sync {
    fn request_all_success_rates(&self, limit: Instant) -> ServiceFuture<'_, Vec<SuccessRate>>;
    fn request_all_execution_times(&self, percentile: f64, limit: Instant) -> ServiceFuture<'_, Vec<ExecutionTime>>;
    fn close(&self) -> ServiceFuture<'_, ()>;
}
