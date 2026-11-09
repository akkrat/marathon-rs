//! Port of core/src/main/kotlin/com/malinskiy/marathon/report/allure/AllureReporter.kt

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ReportKind {
    AllureResultsWrite,
    ImageIo,
    ImageMetadata,
    Transformer,
    Sax,
}

impl ReportKind {
    pub const fn description(&self) -> &'static str {
        match self {
            Self::AllureResultsWrite => "Allure results write failure",
            Self::ImageIo => "image I/O error",
            Self::ImageMetadata => "invalid image metadata",
            Self::Transformer => "XML transformation failure",
            Self::Sax => "XML parsing failure",
        }
    }

    /// Stable machine identifier, independent of user-facing wording.
    pub const fn identifier(&self) -> &'static str {
        match self {
            Self::AllureResultsWrite => "report.allure-results-write",
            Self::ImageIo => "report.image-io",
            Self::ImageMetadata => "report.image-metadata",
            Self::Transformer => "report.transformer",
            Self::Sax => "report.sax",
        }
    }
}

impl std::fmt::Display for ReportKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.description())
    }
}
