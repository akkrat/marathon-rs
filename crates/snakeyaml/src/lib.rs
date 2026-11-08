//! SnakeYAML 2.2 event parser and Jackson 2.17 YAML token adapter.
//! Apache-2.0 parser attribution is retained in snakeyaml/LICENSE.
pub mod node;
pub mod resolve;
pub mod snakeyaml;
pub mod tokens;
pub use jcompat::jnum;
