//! Port of core/src/main/kotlin/com/malinskiy/marathon/analytics/external/influx/InfluxDbProvider.kt
//! and external/influx2/InfluxDb2Provider.kt (injected constructors for TrackerFactory.kt).
use crate::{graphite::*, http::text, influx::*, influx2::*};
use jcompat::writers::JavaString;
use marathon_config::*;
use marathon_core::{
    analytics::{
        remote::{graphite::GraphiteQuery, influx::InfluxQuery, influx2::FluxQuery},
        *,
    },
    error::*,
    executor::shared::failure,
    runtime::*,
};
use std::sync::Arc;

#[derive(Clone)]
pub struct AnalyticsClients {
    pub context: MetricsContext,
    pub io: Arc<dyn VendorRuntime>,
    graphite_endpoint: Option<JavaString>,
}
impl AnalyticsClients {
    pub fn new(context: MetricsContext, io: Arc<dyn VendorRuntime>) -> Self {
        Self {
            context,
            io,
            graphite_endpoint: None,
        }
    }
    pub fn graphite_client(&self, host: &str) -> QueryableGraphiteClient {
        match &self.graphite_endpoint {
            Some(url) => QueryableGraphiteClient::with_endpoint(url, self.context.clone()),
            None => QueryableGraphiteClient::new(host, self.context.clone()),
        }
    }
    pub fn with_graphite_endpoint(mut self, endpoint: JavaString) -> Self {
        self.graphite_endpoint = Some(endpoint);
        self
    }
}
impl MetricsSetup for AnalyticsClients {
    fn influx<'a>(&'a self, config: &'a InfluxDbConfiguration, debug: bool) -> ServiceFuture<'a, Arc<dyn InfluxQuery>> {
        Box::pin(async move {
            Ok(
                InfluxClient::connect(config, debug, self.context.clone(), self.io.clone()).await?
                    as Arc<dyn InfluxQuery>,
            )
        })
    }
    fn influx2<'a>(&'a self, config: &'a InfluxDb2Configuration, debug: bool) -> ServiceFuture<'a, Arc<dyn FluxQuery>> {
        Box::pin(
            async move { Ok(Influx2Client::connect(config, debug, self.context.clone()).await? as Arc<dyn FluxQuery>) },
        )
    }
    fn graphite<'a>(&'a self, host: &'a str) -> ServiceFuture<'a, Arc<dyn GraphiteQuery>> {
        Box::pin(async move { Ok(Arc::new(self.graphite_client(host)) as Arc<dyn GraphiteQuery>) })
    }
}
/// CLI injects this factory into the merged core pipeline, alongside report
/// subscribers. Calling MetricsSetup separately preserves Kotlin's two clients.
pub struct BackendClients {
    pub clients: Arc<AnalyticsClients>,
    pub config: AnalyticsConfiguration,
    pub debug: bool,
}
impl BackendFactory for BackendClients {
    fn create(&self, request: BackendRequest) -> ServiceFuture<'_, Result<Option<Arc<dyn TrackerInternal>>, Failure>> {
        Box::pin(async move {
            let tracker: Arc<dyn TrackerInternal> = match &self.config {
                AnalyticsConfiguration::Disabled => return Ok(Ok(None)),
                AnalyticsConfiguration::InfluxDb(config) => {
                    let client = match InfluxClient::connect(
                        config,
                        self.debug,
                        self.clients.context.clone(),
                        self.clients.io.clone(),
                    )
                    .await
                    {
                        Ok(client) => client,
                        Err(OperationError::Failed(f)) => return Ok(Err(f)),
                        Err(e) => return Err(e),
                    };
                    Arc::new(InfluxTracker {
                        client,
                        read_only: config.read_only,
                    })
                }
                AnalyticsConfiguration::InfluxDb2(config) => {
                    let client = match Influx2Client::connect(config, self.debug, self.clients.context.clone()).await {
                        Ok(client) => client,
                        Err(OperationError::Failed(f)) => return Ok(Err(f)),
                        Err(e) => return Err(e),
                    };
                    Arc::new(Influx2Tracker::new(client, config.read_only)?)
                }
                AnalyticsConfiguration::Graphite(config) => {
                    let port = match request {
                        BackendRequest::Graphite { port } => port,
                        _ => config.port,
                    }
                    .unwrap_or(2003);
                    // Socket validates the port only when the first eligible event writes.
                    let (port, sender): (u16, Arc<dyn DataSender>) = match u16::try_from(port) {
                        Ok(port) => (port, Arc::new(SocketSender::default())),
                        Err(_) => (0, Arc::new(InvalidPortSender(port))),
                    };
                    Arc::new(GraphiteTracker {
                        client: BasicGraphiteClient {
                            host: text(&config.host)?,
                            port,
                            prefix: config.prefix.as_ref().map(text).transpose()?,
                            sender,
                            cancellation: self.clients.context.cancellation.clone(),
                        },
                        read_only: config.read_only,
                    })
                }
            };
            Ok(Ok(Some(tracker)))
        })
    }
}

struct InvalidPortSender(i32);
impl DataSender for InvalidPortSender {
    fn send_data<'a>(&'a self, _: &'a str, _: u16, _: String, _: Arc<dyn CancellationSignal>) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            Err(failure(
                "java.lang.IllegalArgumentException",
                &format!("port out of range:{}", self.0),
            )
            .into())
        })
    }
}
