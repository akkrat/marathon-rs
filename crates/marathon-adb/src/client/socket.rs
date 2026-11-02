//! Port of adam 0.5.8 `adam/src/main/kotlin/com/malinskiy/adam/extension/Socket.kt` (including the sync/file writers at :142-195).
//! adam portions: Copyright (C) 2019-2021 Anton Malinskiy; Apache-2.0.

use jcompat::utf8::decode_utf8;

use super::consts::{MAX_PACKET_LENGTH, POOLED_DEFAULT_BUFFER_SIZE, message};
use super::error::AdbError;
use super::transform::{ResponseTransformer, StringResponseTransformer};
use super::transport::Socket;

/// `transport/TransportResponse.kt`: the OKAY/FAIL handshake result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransportResponse {
    pub okay: bool,
    pub message: Option<String>,
}

/// Kotlin `toIntOrNull(16)` on a length header (at most four characters after trimming): an optional
/// `+`, then hex digits. A `-` sign is refused rather than read as a negative length, which adam would
/// hand to a buffer as a negative limit; only the ASCII digits `echo`/adbd emit are accepted (P7).
pub(crate) fn parse_hex_length(s: &str) -> Option<usize> {
    let digits = s.strip_prefix('+').unwrap_or(s);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    usize::from_str_radix(digits, 16).ok()
}

impl Socket {
    pub async fn write_sync_request(&self, tag: &[u8; 4], path: &str) -> Result<(), AdbError> {
        let mut bytes = tag.to_vec();
        bytes.extend_from_slice(&(path.len() as u32).to_le_bytes());
        bytes.extend_from_slice(path.as_bytes());
        self.write(&bytes).await
    }

    pub async fn write_sync_v2_request(
        &self,
        tag: &[u8; 4],
        path: &str,
        flags: u32,
        mode: Option<u32>,
    ) -> Result<(), AdbError> {
        self.write_sync_request(tag, path).await?;
        let mut bytes = tag.to_vec();
        if let Some(mode) = mode {
            bytes.extend_from_slice(&mode.to_le_bytes());
        }
        bytes.extend_from_slice(&flags.to_le_bytes());
        self.write(&bytes).await
    }

    pub async fn write_file(&self, reader: &super::file_io::AsyncFileReader) -> Result<(), AdbError> {
        let mut transferred = 0;
        loop {
            let bytes = reader.read(65536).await?;
            if bytes.is_empty() {
                break;
            }
            self.write(&bytes).await?;
            transferred += bytes.len() as u64;
        }
        reader.close().await?;
        if transferred != reader.size {
            return Err(AdbError::Io("Unexpected end of local file".into()));
        }
        Ok(())
    }

    /// `copyTo`: reads into `transformer` with reads of at most `buffer_size` bytes, until `limit` bytes
    /// or the end of the stream. Returns the number of bytes copied.
    pub async fn copy_to<T: ResponseTransformer + ?Sized>(
        &mut self,
        transformer: &mut T,
        buffer_size: usize,
        limit: Option<u64>,
    ) -> u64 {
        let mut buffer = vec![0u8; buffer_size];
        let mut processed = 0u64;
        loop {
            let to_read = match limit {
                Some(limit) if limit - processed <= buffer_size as u64 => (limit - processed) as usize,
                _ => buffer_size,
            };
            if to_read == 0 {
                break;
            }
            let available = self.read_available(&mut buffer[..to_read]).await;
            if available < 0 {
                break;
            }
            transformer.process(&buffer[..available as usize]);
            processed += available as u64;
        }
        processed
    }

    /// `readOptionalProtocolString`: a `<4 hex digits><text>` string, or, if the first four bytes are not
    /// a length, the trimmed rest of the stream (which includes nothing of those four bytes).
    pub async fn read_optional_protocol_string(&mut self) -> String {
        let mut header = StringResponseTransformer::default();
        self.copy_to(&mut header, POOLED_DEFAULT_BUFFER_SIZE, Some(4)).await;
        let header = header.transform().unwrap_or_default();
        match parse_hex_length(&header) {
            None => self.read_status().await,
            Some(length) => {
                let mut body = StringResponseTransformer::default();
                self.copy_to(&mut body, POOLED_DEFAULT_BUFFER_SIZE, Some(length as u64))
                    .await;
                body.transform().unwrap_or_default()
            }
        }
    }

    /// `readProtocolString`: a `<4 hex digits><text>` string, decoded as UTF-8 and not trimmed.
    ///
    /// adam reads the body into a 16 KiB pooled buffer and fails with `IllegalArgumentException` on a
    /// longer string; here any length is read (recorded as an incidental bug in the 6.1 findings).
    pub async fn read_protocol_string(&mut self) -> Result<String, AdbError> {
        let mut header = StringResponseTransformer::default();
        let copied = self.copy_to(&mut header, MAX_PACKET_LENGTH, Some(4)).await;
        let length = header.transform()?;
        let unexpected = || AdbError::RequestRejected(format!("Unexpected string length: {length}"));
        if copied != 4 {
            return Err(unexpected());
        }
        let message_length = parse_hex_length(&length).ok_or_else(unexpected)?;
        let mut body = vec![0u8; message_length];
        let read = self.read_fully(&mut body).await;
        if read != message_length {
            return Err(AdbError::RequestRejected("Incomplete string received".into()));
        }
        Ok(decode_utf8(&body))
    }

    /// `readStatus`: the rest of the stream, trimmed.
    pub async fn read_status(&mut self) -> String {
        let mut transformer = StringResponseTransformer::default();
        self.copy_to(&mut transformer, POOLED_DEFAULT_BUFFER_SIZE, None).await;
        transformer.transform().unwrap_or_default()
    }

    /// `write`.
    pub async fn write(&self, request: &[u8]) -> Result<(), AdbError> {
        self.write_fully(request).await
    }

    /// `readTransportResponse`: OKAY, or the message after anything else.
    ///
    /// A short read is not OKAY, and the message read after it is then empty: a connection closed before
    /// the reply gives `RequestRejectedException("")`, not "no message received" (known bug N15).
    pub async fn read_transport_response(&mut self) -> TransportResponse {
        let mut status = [0u8; 4];
        let read = self.read_fully(&mut status).await;
        let okay = read == 4 && &status == message::OKAY;
        let message = if okay {
            None
        } else {
            Some(self.read_optional_protocol_string().await)
        };
        TransportResponse { okay, message }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_lengths() {
        assert_eq!(parse_hex_length("0029"), Some(0x29));
        assert_eq!(parse_hex_length("00Ab"), Some(0xab));
        assert_eq!(parse_hex_length("+01"), Some(1));
        assert_eq!(parse_hex_length("XXXX"), None);
        assert_eq!(parse_hex_length("-001"), None);
        assert_eq!(parse_hex_length(""), None);
        assert_eq!(parse_hex_length("+"), None);
        // Fullwidth digits, which Kotlin's toIntOrNull would take, are refused.
        assert_eq!(parse_hex_length("\u{FF11}"), None);
    }
}
