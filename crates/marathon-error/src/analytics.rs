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
    Influxdb2,
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
    pub const fn legacy_class(&self) -> &'static str {
        match self {
            Self::General => "org.influxdb.InfluxDBException",
            Self::AuthorizationFailed => "org.influxdb.InfluxDBException$AuthorizationFailedException",
            Self::CacheMaxMemorySizeExceeded => "org.influxdb.InfluxDBException$CacheMaxMemorySizeExceededException",
            Self::DatabaseNotFound => "org.influxdb.InfluxDBException$DatabaseNotFoundException",
            Self::FieldTypeConflict => "org.influxdb.InfluxDBException$FieldTypeConflictException",
            Self::HintedHandOffQueueNotEmpty => "org.influxdb.InfluxDBException$HintedHandOffQueueNotEmptyException",
            Self::PointsBeyondRetentionPolicy => "org.influxdb.InfluxDBException$PointsBeyondRetentionPolicyException",
            Self::UnableToParse => "org.influxdb.InfluxDBException$UnableToParseException",
        }
    }
}

impl AnalyticsKind {
    pub const fn legacy_class(&self) -> &'static str {
        match self {
            Self::BadRequest => "com.influxdb.exceptions.BadRequestException",
            Self::Forbidden => "com.influxdb.exceptions.ForbiddenException",
            Self::InfluxV2 => "com.influxdb.exceptions.InfluxException",
            Self::InternalServerError => "com.influxdb.exceptions.InternalServerErrorException",
            Self::NotFound => "com.influxdb.exceptions.NotFoundException",
            Self::ServiceUnavailable => "com.influxdb.exceptions.ServiceUnavailableException",
            Self::Unauthorized => "com.influxdb.exceptions.UnauthorizedException",
            Self::UnprocessableEntity => "com.influxdb.exceptions.UnprocessableEntityException",
            Self::FluxCsvParser => "com.influxdb.query.exceptions.FluxCsvParserException",
            Self::FluxQuery => "com.influxdb.query.exceptions.FluxQueryException",
            Self::Graphite => "com.malinskiy.marathon.analytics.external.graphite.GraphiteException",
            Self::Influxdb2 => "com.malinskiy.marathon.analytics.external.influx2.Influxdb2Exception",
            Self::JsonEncoding => "com.squareup.moshi.JsonEncodingException",
            Self::InfluxIo => "org.influxdb.InfluxDBIOException",
            Self::InfluxMapping => "org.influxdb.InfluxDBMapperException",
            Self::InfluxV1 { problem, .. } => problem.legacy_class(),
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
