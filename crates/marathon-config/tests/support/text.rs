pub fn json(s: &str) -> serde_json::Value {
    s.into()
}
pub fn short_json(s: &str, limit: usize) -> serde_json::Value {
    let len = s.encode_utf16().count();
    if len <= limit {
        return json(s);
    }
    use sha2::Digest;
    let hash = sha2::Sha256::digest(s.as_bytes());
    let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
    format!("<{len} chars, sha256 {hex}>").into()
}
