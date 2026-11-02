//! Port of adam 0.5.8 `adam/src/main/kotlin/com/malinskiy/adam/Const.kt`.
//! adam portions: Copyright (C) 2019-2021 Anton Malinskiy; Apache-2.0.
//!
//! `DEFAULT_TRANSPORT_ENCODING` is always UTF-8 and has no constant here. The sync messages, file types,
//! protobuf and logcat-metric constants are left to the WPs whose requests use them.

pub const MAX_REMOTE_PATH_LENGTH: usize = 1024;
pub const DEFAULT_BUFFER_SIZE: usize = 1024;
pub const DEFAULT_ADB_HOST: &str = "127.0.0.1";
pub const DEFAULT_ADB_PORT: u16 = 5037;
pub const SERVER_PORT_ENV_VAR: &str = "ANDROID_ADB_SERVER_PORT";
/// The shell v2 read cap: a larger packet is handed on in pieces of at most this size.
pub const MAX_PACKET_LENGTH: usize = 16384;
/// The shell v1 stream read cap: one read gives at most this many bytes.
pub const MAX_FILE_PACKET_LENGTH: usize = 64 * 1024;

/// The pooled `withDefaultBuffer` size (`transport/BufferFactory.kt:29`), used as the read size of
/// `copyTo` callers that borrow it.
pub(crate) const POOLED_DEFAULT_BUFFER_SIZE: usize = 4088;

pub mod message {
    pub const OKAY: &[u8; 4] = b"OKAY";
    pub const FAIL: &[u8; 4] = b"FAIL";
}
