//! Port of com/linkedin/dex/spec/DexException.kt (dex-test-parser 2.3.4)

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DexKind {
    Decode,
}

impl DexKind {
    pub const fn description(&self) -> &'static str {
        match self {
            Self::Decode => "DEX decoding failure",
        }
    }

    /// Stable machine identifier, independent of user-facing wording.
    pub const fn identifier(&self) -> &'static str {
        match self {
            Self::Decode => "dex.decode",
        }
    }
}

impl std::fmt::Display for DexKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.description())
    }
}
