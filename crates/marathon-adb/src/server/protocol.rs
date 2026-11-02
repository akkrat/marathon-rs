//! Wire framing shared by the host and device services (AOSP `protocol.txt`, `SERVICES.TXT`, `SYNC.TXT`).

use std::io;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub(crate) const OKAY: &[u8; 4] = b"OKAY";
pub(crate) const FAIL: &[u8; 4] = b"FAIL";

/// Shell protocol v2 packet ids (`adb/shell_protocol.h`).
pub(crate) mod shell_v2 {
    pub const STDOUT: u8 = 1;
    pub const STDERR: u8 = 2;
    pub const EXIT: u8 = 3;
}

/// The longest request the real server accepts (`MAX_PAYLOAD`); anything longer is a framing error.
const MAX_REQUEST_LENGTH: usize = 1024 * 1024;

/// Reads one `<4 hex digits><payload>` request. Returns `None` on a clean EOF before the first byte.
///
/// adam sends the length with `%04X` and the real client with `%04x`, so both cases are accepted.
pub(crate) async fn read_request<R: AsyncRead + Unpin>(r: &mut R) -> io::Result<Option<String>> {
    let mut len = [0u8; 4];
    let mut got = 0;
    while got < 4 {
        let n = r.read(&mut len[got..]).await?;
        if n == 0 {
            return if got == 0 {
                Ok(None)
            } else {
                Err(io::Error::new(io::ErrorKind::UnexpectedEof, "truncated request length"))
            };
        }
        got += n;
    }
    let len = std::str::from_utf8(&len)
        .ok()
        .and_then(|s| usize::from_str_radix(s, 16).ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, format!("bad request length {len:?}")))?;
    if len > MAX_REQUEST_LENGTH {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("request too long: {len}"),
        ));
    }
    let mut payload = vec![0u8; len];
    r.read_exact(&mut payload).await?;
    String::from_utf8(payload)
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// `<4 hex digits><bytes>`, the "protocol string" of `protocol.txt`.
pub(crate) fn protocol_string(s: &[u8]) -> Vec<u8> {
    let mut out = format!("{:04x}", s.len()).into_bytes();
    out.extend_from_slice(s);
    out
}

pub(crate) async fn write_okay<W: AsyncWrite + Unpin>(w: &mut W) -> io::Result<()> {
    w.write_all(OKAY).await
}

pub(crate) async fn write_fail<W: AsyncWrite + Unpin>(w: &mut W, message: &str) -> io::Result<()> {
    let mut out = FAIL.to_vec();
    out.extend_from_slice(&protocol_string(message.as_bytes()));
    w.write_all(&out).await
}

pub(crate) fn shell_v2_packet(id: u8, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + data.len());
    out.push(id);
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(data);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn read_request_accepts_both_hex_cases() {
        let mut data: &[u8] = b"000Chost:version000ahost:kill";
        assert_eq!(read_request(&mut data).await.unwrap().as_deref(), Some("host:version"));
        let mut data: &[u8] = b"000chost:version";
        assert_eq!(read_request(&mut data).await.unwrap().as_deref(), Some("host:version"));
    }

    #[tokio::test]
    async fn read_request_eof_and_garbage() {
        let mut empty: &[u8] = b"";
        assert_eq!(read_request(&mut empty).await.unwrap(), None);
        let mut short: &[u8] = b"00";
        assert!(read_request(&mut short).await.is_err());
        let mut garbage: &[u8] = b"zzzzhost";
        assert!(read_request(&mut garbage).await.is_err());
    }

    #[test]
    fn framing_helpers() {
        assert_eq!(protocol_string(b"0029"), b"00040029");
        assert_eq!(shell_v2_packet(shell_v2::EXIT, &[3]), vec![3, 1, 0, 0, 0, 3]);
    }
}
