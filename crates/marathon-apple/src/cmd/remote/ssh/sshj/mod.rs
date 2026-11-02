//! Kotlin package `com.malinskiy.marathon.apple.cmd.remote.ssh.sshj`: the SSH command executor on the vendored
//! russh client. Modules follow the Kotlin files; `client`, `channel`, `known_hosts`, `keep_alive` and `error` port
//! the sshj 0.39.0 behaviour those files depend on.

pub mod auth;
pub mod channel;
pub mod client;
pub mod config;
pub mod error;
pub mod executor;
pub mod factory;
pub mod host;
pub mod keep_alive;
pub mod known_hosts;
pub mod session;

pub use auth::SshAuthentication;
pub use error::{DisconnectReason, SshFailure};
pub use executor::{SshCommandExecutor, shell_command};
pub use factory::SshjCommandExecutorFactory;
pub use host::SshCommandHost;
