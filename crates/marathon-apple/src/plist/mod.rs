//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/plist/PropertyList.kt

pub mod bundle;
pub mod delegates;
mod reader;
pub mod writer;

use std::fmt;
use std::fs::File;
use std::io::{self, BufReader, Cursor, Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

/// Everything that can go wrong reading a plist or a modelled key. One vocabulary for the
/// Java exceptions dd-plist and the delegates throw (WP 4.5a simplification P4): accept/reject
/// and the offending key match Kotlin; class and message text are ours.
#[derive(Debug)]
pub enum PlistError {
    /// The file could not be opened or read; `is_not_found` separates a missing file (which
    /// 4.4/2.3b stage differently) from other I/O failures.
    Io { is_not_found: bool, message: String },
    /// Not an XML or binary plist, or malformed within one.
    NotAPlist { message: String },
    /// An OpenStep/ASCII plist. dd-plist accepts it; real bundles never use it (P1).
    UnsupportedFormat,
    /// The root object is not a dictionary; every model is dictionary-rooted.
    RootNotDictionary { found: &'static str },
    /// A required delegate read of an absent key (`plist does not define property …` in Kotlin).
    MissingKey { key: String },
    /// A modelled key (or array element under one) carries a value of the wrong type.
    WrongType {
        key: String,
        expected: &'static str,
        found: &'static str,
    },
}

pub type PlistResult<T> = Result<T, PlistError>;

impl fmt::Display for PlistError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlistError::Io { message, .. } => write!(f, "plist I/O error: {message}"),
            PlistError::NotAPlist { message } => write!(f, "not a plist: {message}"),
            PlistError::UnsupportedFormat => {
                write!(f, "unsupported plist format (OpenStep/ASCII plists are not supported)")
            }
            PlistError::RootNotDictionary { found } => {
                write!(f, "plist root is a {found}, not a dictionary")
            }
            PlistError::MissingKey { key } => write!(f, "plist does not define property {key}"),
            PlistError::WrongType { key, expected, found } => {
                write!(f, "plist property {key} is a {found}, not a {expected}")
            }
        }
    }
}

impl std::error::Error for PlistError {}

impl From<io::Error> for PlistError {
    fn from(error: io::Error) -> Self {
        PlistError::Io {
            is_not_found: error.kind() == io::ErrorKind::NotFound,
            message: error.to_string(),
        }
    }
}

/// One plist value. Dictionaries are shared nodes: the Kotlin models wrap an `NSDictionary`
/// by reference, so a nested model and its root see each other's mutations.
#[derive(Clone, Debug)]
pub enum PlistValue {
    String(String),
    Integer(i128),
    Real(f64),
    Bool(bool),
    Date(plist::Date),
    Data(Vec<u8>),
    Array(Vec<PlistValue>),
    Dict(PlistDict),
    Uid(u64),
}

impl PlistValue {
    pub fn type_name(&self) -> &'static str {
        match self {
            PlistValue::String(_) => "string",
            PlistValue::Integer(_) => "integer",
            PlistValue::Real(_) => "real",
            PlistValue::Bool(_) => "boolean",
            PlistValue::Date(_) => "date",
            PlistValue::Data(_) => "data",
            PlistValue::Array(_) => "array",
            PlistValue::Dict(_) => "dictionary",
            PlistValue::Uid(_) => "uid",
        }
    }
}

/// Dictionary equality is the Java map equality dd-plist uses: key order does not matter,
/// arrays stay ordered, an integer never equals a real (`tools/oracle/plist-probes`).
impl PartialEq for PlistValue {
    fn eq(&self, other: &Self) -> bool {
        use PlistValue::*;
        match (self, other) {
            (String(a), String(b)) => a == b,
            (Integer(a), Integer(b)) => a == b,
            (Real(a), Real(b)) => a == b,
            (Bool(a), Bool(b)) => a == b,
            (Date(a), Date(b)) => a == b,
            (Data(a), Data(b)) => a == b,
            (Array(a), Array(b)) => a == b,
            (Dict(a), Dict(b)) => a == b,
            (Uid(a), Uid(b)) => a == b,
            _ => false,
        }
    }
}

/// An ordered, shared plist dictionary: dd-plist preserves key order from parse through
/// write; replacing a key keeps its position, a new key appends. Duplicates cannot exist
/// (on parse, the last value wins at the first position, as dd-plist does).
#[derive(Clone, Debug, Default)]
pub struct PlistDict {
    entries: Arc<Mutex<Vec<(String, PlistValue)>>>,
}

impl PlistDict {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, Vec<(String, PlistValue)>> {
        self.entries.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn get_cloned(&self, key: &str) -> Option<PlistValue> {
        self.lock().iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.lock().iter().any(|(k, _)| k == key)
    }

    /// Insert keeping an existing key's position (dd-plist, probe `replace-keeps-position`).
    pub fn insert(&self, key: &str, value: PlistValue) {
        let mut entries = self.lock();
        if let Some((_, existing)) = entries.iter_mut().find(|(k, _)| k == key) {
            *existing = value;
        } else {
            entries.push((key.to_owned(), value));
        }
    }

    pub fn keys(&self) -> Vec<String> {
        self.lock().iter().map(|(k, _)| k.clone()).collect()
    }

    pub fn snapshot(&self) -> Vec<(String, PlistValue)> {
        self.lock().clone()
    }

    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }
}

impl PartialEq for PlistDict {
    fn eq(&self, other: &Self) -> bool {
        if Arc::ptr_eq(&self.entries, &other.entries) {
            return true;
        }
        // Compare snapshots: holding either lock across the recursive value comparison
        // deadlocks when one dictionary contains the other (shared acyclic trees are
        // normal here — a nested model and its root wrap the same nodes).
        let mine = self.snapshot();
        let theirs = other.snapshot();
        mine.len() == theirs.len()
            && mine
                .iter()
                .all(|(k, v)| theirs.iter().find(|(ok, _)| ok == k).is_some_and(|(_, ov)| ov == v))
    }
}

/// Reject OpenStep/ASCII up front (P1): the `plist` crate would accept it, real bundles
/// never contain it, and accepting it here would silently diverge from the approved scope.
/// Format sniff, mirroring dd-plist's: binary plists start `bplist`, XML plists start `<`.
fn sniff_format(bytes: &[u8]) -> PlistResult<()> {
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    let bytes = bytes.strip_prefix(b"\xfe\xff").unwrap_or(bytes);
    let bytes = bytes.strip_prefix(b"\xff\xfe").unwrap_or(bytes);
    let start = bytes
        .iter()
        .position(|b| !matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0))
        .unwrap_or(bytes.len());
    let head = &bytes[start..];
    if head.starts_with(b"bplist") || head.starts_with(b"<") {
        Ok(())
    } else {
        Err(PlistError::UnsupportedFormat)
    }
}

fn convert(value: plist::Value) -> PlistResult<PlistValue> {
    Ok(match value {
        plist::Value::String(s) => PlistValue::String(s),
        plist::Value::Integer(i) => PlistValue::Integer(
            i.as_signed()
                .map(i128::from)
                .unwrap_or_else(|| i128::from(i.as_unsigned().unwrap_or(u64::MAX))),
        ),
        plist::Value::Real(r) => PlistValue::Real(r),
        plist::Value::Boolean(b) => PlistValue::Bool(b),
        plist::Value::Date(d) => PlistValue::Date(d),
        plist::Value::Data(d) => PlistValue::Data(d),
        plist::Value::Array(items) => PlistValue::Array(items.into_iter().map(convert).collect::<PlistResult<_>>()?),
        plist::Value::Dictionary(dict) => {
            let shared = PlistDict::new();
            {
                let mut entries = shared.lock();
                for (k, v) in dict.into_iter() {
                    // plist::Dictionary already merged duplicates like dd-plist (IndexMap:
                    // last value, first position).
                    entries.push((k, convert(v)?));
                }
            }
            PlistValue::Dict(shared)
        }
        plist::Value::Uid(uid) => PlistValue::Uid(uid.get()),
        // plist::Value is non-exhaustive; a future variant is a parse failure here,
        // never a panic.
        other => {
            return Err(PlistError::NotAPlist {
                message: format!("unsupported plist value {other:?}"),
            });
        }
    })
}

fn parse_value(bytes: Vec<u8>) -> PlistResult<PlistValue> {
    sniff_format(&bytes)?;
    if bytes.starts_with(b"bplist") {
        let value = plist::Value::from_reader(Cursor::new(bytes)).map_err(|e| {
            // plist surfaces some I/O errors through its own error type; reading from an
            // in-memory cursor, what remains is a parse failure.
            PlistError::NotAPlist { message: e.to_string() }
        })?;
        convert(value)
    } else {
        reader::parse_xml_value(&bytes)
    }
}

fn require_dict(value: PlistValue) -> PlistResult<PlistDict> {
    match value {
        PlistValue::Dict(dict) => Ok(dict),
        other => Err(PlistError::RootNotDictionary {
            found: other.type_name(),
        }),
    }
}

/// Reading is synchronous over bytes/readers; file opening is blocking I/O, so callers keep
/// it off the core executor (`docs/design/lifecycle.md`).
pub fn dict_from_path(path: &Path) -> PlistResult<PlistDict> {
    let mut file = BufReader::new(File::open(path)?);
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    dict_from_bytes(&bytes)
}

pub fn dict_from_bytes(bytes: &[u8]) -> PlistResult<PlistDict> {
    require_dict(parse_value(bytes.to_vec())?)
}

pub fn dict_from_reader<R: Read>(mut reader: R) -> PlistResult<PlistDict> {
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    dict_from_bytes(&bytes)
}

/// `PropertyList.from` / `saveAsXML` for every dictionary-rooted model. The Kotlin
/// `from(String)` overload takes a file *path*, not content; the Rust entry points are
/// named so that ambiguity cannot recur. `saveAsBinary` has no caller at `e47352f2`
/// and is not ported (P2). Equality is by delegate content and class-sensitive in Kotlin;
/// in Rust each model is its own type deriving `PartialEq` over the shared dictionary,
/// and no Java-compatible `hashCode` is attempted (P8).
pub trait PropertyList: Sized {
    fn wrap(dict: PlistDict) -> Self;
    fn dict(&self) -> &PlistDict;

    fn from_path(path: &Path) -> PlistResult<Self> {
        Ok(Self::wrap(dict_from_path(path)?))
    }

    fn from_bytes(bytes: &[u8]) -> PlistResult<Self> {
        Ok(Self::wrap(dict_from_bytes(bytes)?))
    }

    fn from_reader<R: Read>(reader: R) -> PlistResult<Self> {
        Ok(Self::wrap(dict_from_reader(reader)?))
    }

    fn save_as_xml_to_file(&self, path: &Path) -> PlistResult<()> {
        let mut file = io::BufWriter::new(File::create(path)?);
        self.save_as_xml_to_writer(&mut file)?;
        file.flush()?;
        Ok(())
    }

    fn save_as_xml_to_writer<W: Write>(&self, writer: W) -> PlistResult<()> {
        writer::write_dict_xml(self.dict(), writer)
    }

    fn to_xml_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        // Writing to a Vec cannot fail.
        let _ = writer::write_dict_xml(self.dict(), &mut out);
        out
    }
}

/// dd-plist and delegate failures as the bundle sees them; class and text are WP 4.5a's P4 except for the
/// missing-key message, which A27/A31 depend on.
pub(crate) fn plist_failure(path: &Path, error: PlistError) -> marathon_core::error::VendorError {
    use crate::cmd::failure;
    use marathon_core::error::VendorError;
    match error {
        PlistError::Io { is_not_found: true, .. } => {
            VendorError::Other(failure("java.nio.file.NoSuchFileException", path.display().to_string()))
        }
        PlistError::WrongType { .. } => VendorError::Other(failure("java.lang.ClassCastException", error.to_string())),
        PlistError::Io { message, .. } => VendorError::Other(failure("java.io.IOException", message)),
        other => VendorError::Other(failure("java.lang.IllegalArgumentException", other.to_string())),
    }
}

/// Worker device descriptors use the same dd-plist boundary as bundle and xctestrun reads.
pub fn worker_descriptor(bytes: &[u8]) -> marathon_core::error::VendorResult<PlistDict> {
    dict_from_bytes(bytes).map_err(|error| plist_failure(Path::new(""), error))
}

#[cfg(test)]
mod worker_boundary_test {
    #[test]
    fn descriptor_plist_error_uses_canonical_class_and_message() {
        let error = super::plist_failure(
            std::path::Path::new("device.plist"),
            super::PlistError::Io {
                is_not_found: false,
                message: "read denied".into(),
            },
        );
        assert_eq!(error.failure().class.to_string_lossy(), "java.io.IOException");
        assert_eq!(
            error.failure().message.as_ref().unwrap().to_string_lossy(),
            "read denied"
        );
        let bytes = b"<plist><dict><key>state</key><integer>1</integer></dict></plist>";
        assert_eq!(
            super::worker_descriptor(bytes).unwrap().get_cloned("state"),
            Some(super::PlistValue::Integer(1))
        );
        assert_eq!(
            super::worker_descriptor(b"garbage")
                .unwrap_err()
                .failure()
                .class
                .to_string_lossy(),
            "java.lang.IllegalArgumentException"
        );
    }
}
