//! Port of com/linkedin/dex/spec/DexException.kt (dex-test-parser 2.3.4)

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DexKind {
    Decode,
}

impl DexKind {
    pub const fn legacy_class(&self) -> &'static str {
        match self {
            Self::Decode => "com.linkedin.dex.spec.DexException",
        }
    }
}
