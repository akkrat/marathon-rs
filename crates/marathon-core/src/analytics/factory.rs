//! Port of core/src/main/kotlin/com/malinskiy/marathon/analytics/external/MetricsProviderFactory.kt
//! and analytics/external/AnalyticsFactory.kt.
use super::*;
use marathon_config::{AnalyticsConfiguration, Configuration, InfluxDb2Configuration, InfluxDbConfiguration};
use marathon_error::FailureKind;
use remote::{
    graphite::{GraphiteDataSource, GraphiteQuery},
    influx::{InfluxDBDataSource, InfluxQuery},
    influx2::{FluxQuery, InfluxDB2DataSource},
};

/// WP 3.4b owns setup side effects and transports. Graphite receives only host:
/// configuration.port is carbon's port and Kotlin's queries always use 80.
pub trait MetricsSetup: Send + Sync {
    fn influx<'a>(&'a self, config: &'a InfluxDbConfiguration, debug: bool) -> ServiceFuture<'a, Arc<dyn InfluxQuery>>;
    fn influx2<'a>(&'a self, config: &'a InfluxDb2Configuration, debug: bool) -> ServiceFuture<'a, Arc<dyn FluxQuery>>;
    fn graphite<'a>(&'a self, host: &'a str) -> ServiceFuture<'a, Arc<dyn GraphiteQuery>>;
}
pub struct MetricsProviderFactory {
    context: MetricsContext,
    setup: Arc<dyn MetricsSetup>,
}
pub type AnalyticsFactory = MetricsProviderFactory;
impl MetricsProviderFactory {
    pub fn new(context: MetricsContext, setup: Arc<dyn MetricsSetup>) -> OperationResult<Self> {
        context.register("marathon_core::analytics::factory")?;
        Ok(Self { context, setup })
    }
    pub async fn create(&self, configuration: &Configuration) -> OperationResult<Analytics> {
        self.create_for(&configuration.analytics_configuration, configuration.debug)
            .await
    }
    pub async fn create_for(&self, config: &AnalyticsConfiguration, debug: bool) -> OperationResult<Analytics> {
        // Reject before setup (including Influx's fallback catch): comparator
        // lookups on core have no error channel for Duration.toMillis overflow.
        let defaults = match config {
            AnalyticsConfiguration::Disabled => None,
            AnalyticsConfiguration::InfluxDb(c) => Some(c.defaults),
            AnalyticsConfiguration::InfluxDb2(c) => Some(c.defaults),
            AnalyticsConfiguration::Graphite(c) => Some(c.defaults),
        };
        if defaults.is_some_and(|d| d.duration.to_millis().is_none()) {
            return Err(failure(
                FailureKind::IllegalArgument,
                "analyticsConfiguration.defaults.duration exceeds the millisecond range of a Java Long",
            )
            .into());
        }
        let (source, defaults): (OperationResult<Arc<dyn remote::RemoteDataSource>>, _) = match config {
            AnalyticsConfiguration::Disabled => {
                return Ok(Analytics::noop(NoOpMetricsProvider::new(false, self.context.clone())?));
            }
            AnalyticsConfiguration::InfluxDb(config) => {
                // Kotlin eagerly constructs the fallback even when setup succeeds.
                self.context.register("marathon_core::analytics::noop")?;
                (
                    self.setup.influx(config, debug).await.map(|client| {
                        Arc::new(InfluxDBDataSource {
                            client,
                            database: config.db_name.clone(),
                            retention_policy: config.retention_policy_configuration.name.clone(),
                        }) as Arc<dyn remote::RemoteDataSource>
                    }),
                    config.defaults,
                )
            }
            AnalyticsConfiguration::InfluxDb2(config) => {
                self.context.register("marathon_core::analytics::noop")?;
                (
                    self.setup.influx2(config, debug).await.map(|client| {
                        Arc::new(InfluxDB2DataSource {
                            client,
                            bucket: config.bucket.clone(),
                        }) as Arc<dyn remote::RemoteDataSource>
                    }),
                    config.defaults,
                )
            }
            AnalyticsConfiguration::Graphite(config) => {
                let client = self.setup.graphite(&config.host).await?;
                let prefix = config.prefix.clone();
                return Ok(Analytics::new(MetricsProviderImpl::new(
                    Arc::new(GraphiteDataSource {
                        client,
                        prefix,
                        context: Some(self.context.clone()),
                    }),
                    config.defaults,
                    self.context.clone(),
                )?));
            }
        };
        match source {
            Ok(source) => Ok(Analytics::new(MetricsProviderImpl::new(
                source,
                defaults,
                self.context.clone(),
            )?)),
            Err(error) => {
                let cause = match error {
                    crate::error::OperationError::Failed(f) => f,
                    crate::error::OperationError::Cancelled(c) => c.failure().clone(),
                };
                self.context.log(
                    "marathon_core::analytics::factory",
                    Level::WARN,
                    "Cannot read history from the analytics backend".into(),
                    Some(cause),
                );
                Ok(Analytics::noop(NoOpMetricsProvider::new(true, self.context.clone())?))
            }
        }
    }
}
