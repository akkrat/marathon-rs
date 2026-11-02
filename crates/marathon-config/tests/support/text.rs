use jcompat::writers::JavaString;
pub fn json(s: &JavaString) -> serde_json::Value {
    match s.as_utf8() {
        Ok(s) => s.into(),
        Err(_) => serde_json::json!({"$utf16": s.0}),
    }
}
pub fn short_json(s: &JavaString, limit: usize) -> serde_json::Value {
    if s.0.len() <= limit {
        return json(s);
    }
    use sha2::Digest;
    let hash = sha2::Sha256::digest(s.to_utf8_bytes());
    let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
    format!("<{} chars, sha256 {hex}>", s.0.len()).into()
}
