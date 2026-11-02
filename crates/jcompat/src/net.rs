//! Java network text semantics.
//!
//! Java's socket address text, as netty puts it in connect errors (`AbstractChannel.java:1172-1174`):
//! JDK 21 `InetSocketAddress.toString()` over `InetAddress.toString()`.
//!
//! URLEncoder form encoding, OkHttp query encoding and OkHttp Basic credentials,
//! as the analytics clients use them (`QueryableGraphiteClient.kt`, the Influx providers).

use std::net::IpAddr;

/// `InetSocketAddress.toString()` for a resolved address: `<name>/<ip>:<port>`, the name empty for an
/// address made from an IP literal. An IPv6 address is bracketed and written in full, each group in
/// lowercase hex without leading zeros (`Inet6Address.getHostAddress`): `/[0:0:0:0:0:0:0:1]:5037`. An
/// IPv4-mapped IPv6 address is an `Inet4Address` in Java and prints as IPv4.
pub fn inet_socket_address(host_name: Option<&str>, ip: IpAddr, port: u16) -> String {
    let name = host_name.unwrap_or("");
    match ip {
        IpAddr::V4(v4) => format!("{name}/{v4}:{port}"),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => format!("{name}/{v4}:{port}"),
            None => {
                let groups: Vec<String> = v6.segments().iter().map(|g| format!("{g:x}")).collect();
                format!("{name}/[{}]:{port}", groups.join(":"))
            }
        },
    }
}

/// Java URLEncoder.encode(text, UTF_8), for valid UTF-8 (analytics P4).
pub fn form_encode(text: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::new();
    for byte in text.bytes() {
        match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'.' | b'-' | b'*' | b'_' => out.push(char::from(byte)),
            b' ' => out.push('+'),
            _ => {
                out.push('%');
                out.push(char::from(HEX[usize::from(byte >> 4)]));
                out.push(char::from(HEX[usize::from(byte & 15)]));
            }
        }
    }
    out
}

/// OkHttp @Query encoding differs from URLEncoder for spaces.
pub fn query_encode(text: &str) -> String {
    form_encode(text).replace('+', "%20")
}

/// OkHttp Credentials.basic uses the JVM ISO-8859-1 encoder by default.
pub fn basic_credentials(user: &str, password: &str) -> Vec<u8> {
    user.chars()
        .chain(std::iter::once(':'))
        .chain(password.chars())
        .map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?'))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The JVM row `inet-socket-address` (`tools/oracle/adb-client-probes`): JDK 21's text for each address
    /// at port 5037, joined by ` | `.
    #[test]
    fn matches_the_jvm() {
        let row = include_str!("../../../fixtures/oracle/adb-client-probes.jsonl")
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .find(|row| row["case"] == "inet-socket-address")
            .unwrap();
        let cases: [(Option<&str>, &str); 8] = [
            (None, "127.0.0.1"),
            (None, "::1"),
            (None, "2001:db8::a:0:1"),
            (None, "fe80::1"),
            (None, "::ffff:10.0.0.1"),
            (None, "::"),
            (Some("adb.example"), "::1"),
            (Some("adb.example"), "10.0.0.2"),
        ];
        let rust: Vec<String> = cases
            .iter()
            .map(|(name, ip)| inet_socket_address(*name, ip.parse().unwrap(), 5037))
            .collect();
        assert_eq!(rust.join(" | "), row["value"].as_str().unwrap());
    }
}
