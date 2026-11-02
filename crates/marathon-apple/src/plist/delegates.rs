//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/plist/Delegates.kt
//!
//! The five `NSDictionary` delegate flavours as typed accessors over the live, shared
//! dictionary. Reads are lazy exactly as in Kotlin: nothing converts until a property is
//! touched, so a wrong-typed value under an untouched key never fails. The conversion
//! rules are dd-plist's `toJavaObject` rules, as probed (`tools/oracle/plist-probes`):
//! an `Int` read truncates integers and reals like Java's `Number.intValue`, while array
//! elements are converted element-wise with the stricter typed conversion (an integer
//! array rejects a real element). Collection element types are checked at access (P9);
//! in Kotlin the unchecked cast let a non-string env value through to explode later.
//!
//! A `null` write is a no-op, for every flavour: Kotlin writes
//! `NSObject.fromJavaObject(null)`, which is Java `null`, and dd-plist's
//! `NSDictionary.put` ignores a null value — an absent key stays absent, a present key
//! keeps its value (probes `null-writes`, `put-null-direct`).

use super::{PlistDict, PlistError, PlistResult, PlistValue, PropertyList};

/// A string-to-string plist dictionary in key order. Kotlin passes these as `Map`
/// (a `LinkedHashMap` from `mapOf`); the order is writer-visible, so the Rust type
/// is an ordered list of pairs, never a hash map.
pub type OrderedStringMap = Vec<(String, String)>;

fn missing(key: &str) -> PlistError {
    PlistError::MissingKey { key: key.to_owned() }
}

fn wrong_type(key: &str, expected: &'static str, value: &PlistValue) -> PlistError {
    PlistError::WrongType {
        key: key.to_owned(),
        expected,
        found: value.type_name(),
    }
}

/// The scalar conversions `delegateFor`/`arrayDelegateFor` perform, per target type.
pub trait PlistScalar: Sized {
    /// `delegateFor<T>` read: `toJavaObject()` then the (unchecked) cast, with Kotlin's
    /// numeric conversions for `Int`.
    fn from_value(key: &str, value: &PlistValue) -> PlistResult<Self>;
    /// `arrayDelegateFor<T>` element read: `toJavaObject(T::class.java)`, stricter for `Int`.
    fn from_array_element(key: &str, value: &PlistValue) -> PlistResult<Self>;
    fn into_value(self) -> PlistValue;
}

impl PlistScalar for String {
    fn from_value(key: &str, value: &PlistValue) -> PlistResult<Self> {
        match value {
            PlistValue::String(s) => Ok(s.clone()),
            _ => Err(wrong_type(key, "string", value)),
        }
    }

    fn from_array_element(key: &str, value: &PlistValue) -> PlistResult<Self> {
        Self::from_value(key, value)
    }

    fn into_value(self) -> PlistValue {
        PlistValue::String(self)
    }
}

impl PlistScalar for bool {
    fn from_value(key: &str, value: &PlistValue) -> PlistResult<Self> {
        match value {
            PlistValue::Bool(b) => Ok(*b),
            _ => Err(wrong_type(key, "boolean", value)),
        }
    }

    fn from_array_element(key: &str, value: &PlistValue) -> PlistResult<Self> {
        Self::from_value(key, value)
    }

    fn into_value(self) -> PlistValue {
        PlistValue::Bool(self)
    }
}

impl PlistScalar for i32 {
    fn from_value(key: &str, value: &PlistValue) -> PlistResult<Self> {
        match value {
            // Java Number.intValue: truncate the long, clamp/truncate the double
            // (Rust's `as` on f64 saturates and maps NaN to 0, like the JVM narrowing).
            PlistValue::Integer(i) => Ok(*i as i32),
            PlistValue::Real(r) => Ok((*r) as i32),
            _ => Err(wrong_type(key, "integer", value)),
        }
    }

    fn from_array_element(key: &str, value: &PlistValue) -> PlistResult<Self> {
        match value {
            PlistValue::Integer(i) => Ok(*i as i32),
            _ => Err(wrong_type(key, "integer", value)),
        }
    }

    fn into_value(self) -> PlistValue {
        PlistValue::Integer(i128::from(self))
    }
}

fn value_to_string_map(key: &str, value: &PlistValue) -> PlistResult<OrderedStringMap> {
    match value {
        PlistValue::Dict(dict) => {
            let mut out = Vec::new();
            for (k, v) in dict.snapshot() {
                match v {
                    PlistValue::String(s) => out.push((k, s)),
                    other => return Err(wrong_type(key, "string", &other)),
                }
            }
            Ok(out)
        }
        _ => Err(wrong_type(key, "dictionary", value)),
    }
}

fn string_map_to_value(map: OrderedStringMap) -> PlistValue {
    let dict = PlistDict::new();
    for (k, v) in map {
        dict.insert(&k, PlistValue::String(v));
    }
    PlistValue::Dict(dict)
}

impl PlistDict {
    /// `delegateFor<T>`: absent key is an error.
    pub fn required<T: PlistScalar>(&self, key: &str) -> PlistResult<T> {
        let value = self.get_cloned(key).ok_or_else(|| missing(key))?;
        T::from_value(key, &value)
    }

    /// `optionalDelegateFor<T>`: absent key reads as `None`.
    pub fn optional<T: PlistScalar>(&self, key: &str) -> PlistResult<Option<T>> {
        match self.get_cloned(key) {
            None => Ok(None),
            Some(value) => Ok(Some(T::from_value(key, &value)?)),
        }
    }

    /// A non-null delegate write.
    pub fn set_value<T: PlistScalar>(&self, key: &str, value: T) {
        self.insert(key, value.into_value());
    }

    /// A nullable delegate write (`delegateFor` on a nullable property, or
    /// `optionalDelegateFor`): `None` leaves the dictionary untouched.
    pub fn set_nullable<T: PlistScalar>(&self, key: &str, value: Option<T>) {
        if let Some(value) = value {
            self.insert(key, value.into_value());
        }
    }

    pub fn required_map(&self, key: &str) -> PlistResult<OrderedStringMap> {
        let value = self.get_cloned(key).ok_or_else(|| missing(key))?;
        value_to_string_map(key, &value)
    }

    pub fn optional_map(&self, key: &str) -> PlistResult<Option<OrderedStringMap>> {
        match self.get_cloned(key) {
            None => Ok(None),
            Some(value) => Ok(Some(value_to_string_map(key, &value)?)),
        }
    }

    pub fn set_map(&self, key: &str, value: OrderedStringMap) {
        self.insert(key, string_map_to_value(value));
    }

    pub fn set_nullable_map(&self, key: &str, value: Option<OrderedStringMap>) {
        if let Some(value) = value {
            self.set_map(key, value);
        }
    }

    /// `arrayDelegateFor(optional = false)`.
    pub fn required_array<T: PlistScalar>(&self, key: &str) -> PlistResult<Vec<T>> {
        let value = self.get_cloned(key).ok_or_else(|| missing(key))?;
        self.elements(key, &value)
    }

    /// `arrayDelegateFor(optional = true)` / `optionalArrayDelegateFor`: absent is empty.
    pub fn optional_array<T: PlistScalar>(&self, key: &str) -> PlistResult<Vec<T>> {
        match self.get_cloned(key) {
            None => Ok(Vec::new()),
            Some(value) => self.elements(key, &value),
        }
    }

    fn elements<T: PlistScalar>(&self, key: &str, value: &PlistValue) -> PlistResult<Vec<T>> {
        match value {
            PlistValue::Array(items) => items.iter().map(|item| T::from_array_element(key, item)).collect(),
            _ => Err(wrong_type(key, "array", value)),
        }
    }

    pub fn set_array<T: PlistScalar>(&self, key: &str, values: Vec<T>) {
        self.insert(key, PlistValue::Array(values.into_iter().map(T::into_value).collect()));
    }

    /// `plistDelegateFor`: the sub-model wraps the live nested dictionary.
    pub fn required_model<M: PropertyList>(&self, key: &str) -> PlistResult<M> {
        let value = self.get_cloned(key).ok_or_else(|| missing(key))?;
        model_from_value(key, &value)
    }

    /// `optionalPlistDelegateFor`.
    pub fn optional_model<M: PropertyList>(&self, key: &str) -> PlistResult<Option<M>> {
        match self.get_cloned(key) {
            None => Ok(None),
            Some(value) => Ok(Some(model_from_value(key, &value)?)),
        }
    }

    pub fn set_model<M: PropertyList>(&self, key: &str, model: &M) {
        self.insert(key, PlistValue::Dict(model.dict().clone()));
    }

    /// `optionalPlistDelegateFor` write: `None` never clears the key.
    pub fn set_optional_model<M: PropertyList>(&self, key: &str, model: Option<&M>) {
        if let Some(model) = model {
            self.set_model(key, model);
        }
    }

    /// `plistListDelegateFor(optional = false)`. The Kotlin getter is broken — it converts
    /// the array with `toJavaObject()`, casts to `Array<T>` and throws for every present
    /// array (probe `nested-model-reads`) — and no production path reads it (production
    /// only writes xctestrun files). Per WP 4.5a decision 3 / P5 this implements the
    /// intended read: wrap each element dictionary.
    pub fn required_model_list<M: PropertyList>(&self, key: &str) -> PlistResult<Vec<M>> {
        let value = self.get_cloned(key).ok_or_else(|| missing(key))?;
        model_list_from_value(key, &value)
    }

    /// `plistListDelegateFor(optional = true)`: absent is empty. Same P5 note as above.
    pub fn optional_model_list<M: PropertyList>(&self, key: &str) -> PlistResult<Vec<M>> {
        match self.get_cloned(key) {
            None => Ok(Vec::new()),
            Some(value) => model_list_from_value(key, &value),
        }
    }

    pub fn set_model_list<M: PropertyList>(&self, key: &str, models: &[M]) {
        self.insert(
            key,
            PlistValue::Array(models.iter().map(|m| PlistValue::Dict(m.dict().clone())).collect()),
        );
    }
}

fn model_from_value<M: PropertyList>(key: &str, value: &PlistValue) -> PlistResult<M> {
    match value {
        PlistValue::Dict(dict) => Ok(M::wrap(dict.clone())),
        _ => Err(wrong_type(key, "dictionary", value)),
    }
}

fn model_list_from_value<M: PropertyList>(key: &str, value: &PlistValue) -> PlistResult<Vec<M>> {
    match value {
        PlistValue::Array(items) => items.iter().map(|item| model_from_value(key, item)).collect(),
        _ => Err(wrong_type(key, "array", value)),
    }
}
