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
    pub const fn legacy_class(&self) -> &'static str {
        match self {
            Self::AllureResultsWrite => "io.qameta.allure.AllureResultsWriteException",
            Self::ImageIo => "javax.imageio.IIOException",
            Self::ImageMetadata => "javax.imageio.metadata.IIOInvalidTreeException",
            Self::Transformer => "javax.xml.transform.TransformerException",
            Self::Sax => "org.xml.sax.SAXException",
        }
    }
}
