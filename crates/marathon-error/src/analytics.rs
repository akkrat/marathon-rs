//! Port of core/src/main/kotlin/com/malinskiy/marathon/analytics/external/influx/InfluxDbProvider.kt

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AnalyticsKind {
    BadRequest,
    Forbidden,
    InfluxV2,
    InternalServerError,
    NotFound,
    ServiceUnavailable,
    Unauthorized,
    UnprocessableEntity,
    FluxCsvParser,
    FluxQuery,
    Graphite,
    InfluxV2Provider,
    JsonEncoding,
    InfluxIo,
    InfluxMapping,
    InfluxV1 {
        problem: InfluxV1Kind,
        retry: RetryDisposition,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RetryDisposition {
    Retry,
    Never,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InfluxV1Kind {
    General,
    AuthorizationFailed,
    CacheMaxMemorySizeExceeded,
    DatabaseNotFound,
    FieldTypeConflict,
    HintedHandOffQueueNotEmpty,
    PointsBeyondRetentionPolicy,
    UnableToParse,
}

impl InfluxV1Kind {
    pub const fn description(&self) -> &'static str {
        match self {
            Self::General => "InfluxDB v1 failure",
            Self::AuthorizationFailed => "InfluxDB v1 authorization failed",
            Self::CacheMaxMemorySizeExceeded => "InfluxDB v1 cache memory limit exceeded",
            Self::DatabaseNotFound => "InfluxDB v1 database not found",
            Self::FieldTypeConflict => "InfluxDB v1 field type conflict",
            Self::HintedHandOffQueueNotEmpty => "InfluxDB v1 pending handoff queue",
            Self::PointsBeyondRetentionPolicy => "InfluxDB v1 points outside retention policy",
            Self::UnableToParse => "InfluxDB v1 input parsing failure",
        }
    }

    /// Stable machine identifier, independent of user-facing wording.
    pub const fn identifier(&self) -> &'static str {
        match self {
            Self::General => "analytics.influx-v1.general",
            Self::AuthorizationFailed => "analytics.influx-v1.authorization-failed",
            Self::CacheMaxMemorySizeExceeded => "analytics.influx-v1.cache-max-memory-size-exceeded",
            Self::DatabaseNotFound => "analytics.influx-v1.database-not-found",
            Self::FieldTypeConflict => "analytics.influx-v1.field-type-conflict",
            Self::HintedHandOffQueueNotEmpty => "analytics.influx-v1.hinted-hand-off-queue-not-empty",
            Self::PointsBeyondRetentionPolicy => "analytics.influx-v1.points-beyond-retention-policy",
            Self::UnableToParse => "analytics.influx-v1.unable-to-parse",
        }
    }
}

impl AnalyticsKind {
    pub const fn description(&self) -> &'static str {
        match self {
            Self::BadRequest => "InfluxDB bad request",
            Self::Forbidden => "InfluxDB access forbidden",
            Self::InfluxV2 => "InfluxDB v2 failure",
            Self::InternalServerError => "InfluxDB internal server failure",
            Self::NotFound => "InfluxDB resource not found",
            Self::ServiceUnavailable => "InfluxDB service unavailable",
            Self::Unauthorized => "InfluxDB authentication required",
            Self::UnprocessableEntity => "unprocessable InfluxDB request",
            Self::FluxCsvParser => "Flux CSV parsing failure",
            Self::FluxQuery => "Flux query failure",
            Self::Graphite => "Graphite failure",
            Self::InfluxV2Provider => "InfluxDB v2 provider failure",
            Self::JsonEncoding => "JSON encoding failure",
            Self::InfluxIo => "InfluxDB I/O error",
            Self::InfluxMapping => "InfluxDB mapping failure",
            Self::InfluxV1 { problem, .. } => problem.description(),
        }
    }

    /// Stable machine identifier, independent of user-facing wording.
    pub const fn identifier(&self) -> &'static str {
        match self {
            Self::BadRequest => "analytics.bad-request",
            Self::Forbidden => "analytics.forbidden",
            Self::InfluxV2 => "analytics.influx-v2",
            Self::InternalServerError => "analytics.internal-server-error",
            Self::NotFound => "analytics.not-found",
            Self::ServiceUnavailable => "analytics.service-unavailable",
            Self::Unauthorized => "analytics.unauthorized",
            Self::UnprocessableEntity => "analytics.unprocessable-entity",
            Self::FluxCsvParser => "analytics.flux-csv-parser",
            Self::FluxQuery => "analytics.flux-query",
            Self::Graphite => "analytics.graphite",
            Self::InfluxV2Provider => "analytics.influx-v2-provider",
            Self::JsonEncoding => "analytics.json-encoding",
            Self::InfluxIo => "analytics.influx-io",
            Self::InfluxMapping => "analytics.influx-mapping",
            Self::InfluxV1 { problem, .. } => problem.identifier(),
        }
    }
}

/// Parse the server's error protocol once; the category and retry rule have different precedence.
pub fn parse_v1_error_body(body: &str) -> (InfluxV1Kind, RetryDisposition) {
    const PHRASES: [(&str, InfluxV1Kind, RetryDisposition); 10] = [
        (
            "database not found",
            InfluxV1Kind::DatabaseNotFound,
            RetryDisposition::Never,
        ),
        (
            "points beyond retention policy",
            InfluxV1Kind::PointsBeyondRetentionPolicy,
            RetryDisposition::Never,
        ),
        (
            "field type conflict",
            InfluxV1Kind::FieldTypeConflict,
            RetryDisposition::Never,
        ),
        ("unable to parse", InfluxV1Kind::UnableToParse, RetryDisposition::Never),
        (
            "hinted handoff queue not empty",
            InfluxV1Kind::HintedHandOffQueueNotEmpty,
            RetryDisposition::Never,
        ),
        (
            "cache-max-memory-size exceeded",
            InfluxV1Kind::CacheMaxMemorySizeExceeded,
            RetryDisposition::Retry,
        ),
        (
            "user is required to write to database",
            InfluxV1Kind::AuthorizationFailed,
            RetryDisposition::Never,
        ),
        (
            "user is not authorized to write to database",
            InfluxV1Kind::AuthorizationFailed,
            RetryDisposition::Never,
        ),
        (
            "authorization failed",
            InfluxV1Kind::AuthorizationFailed,
            RetryDisposition::Never,
        ),
        (
            "username required",
            InfluxV1Kind::AuthorizationFailed,
            RetryDisposition::Never,
        ),
    ];
    let mut problem = None;
    let mut retry = RetryDisposition::Retry;
    for (phrase, category, disposition) in PHRASES {
        if body.contains(phrase) {
            problem.get_or_insert(category);
            if disposition == RetryDisposition::Never {
                retry = disposition;
            }
        }
    }
    (problem.unwrap_or(InfluxV1Kind::General), retry)
}

impl std::fmt::Display for AnalyticsKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.description())
    }
}

impl std::fmt::Display for InfluxV1Kind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.description())
    }
}
