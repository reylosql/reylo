//! Reylo's analysis engine for MySQL, over the core every database shares
//! ([`reylo_engine_core`]): the server's prepare metadata, read with the statement's syntax tree
//! (`syntax::mysql::ast`) for the row-count proof ([`mysql`]), the nested JSON shapes ([`mysql_shape`]) and the
//! parameters the caller may pass null for ([`mysql_params`]).
//!
//! Its host is `reylo-engine` (`engine/native`), which the oracle links and whose program the
//! `reylo` binary serves.

pub mod api;
pub mod capabilities;
pub mod errors;
pub mod host;
pub mod mysql;
pub mod mysql_collation;
pub mod mysql_derived;
pub mod mysql_keys;
pub mod mysql_labels;
pub mod mysql_literals;
pub mod mysql_nulls;
pub mod mysql_params;
pub mod mysql_shape;
pub mod mysql_types;

use reylo_engine_core::{analysis, dialect, fresh, lex, schema_type, shape, structured, types};
#[cfg(test)]
use reylo_engine_core::stack;

pub use api::Mysql;

/// A fingerprint of the sources this engine was built from: core's, this crate's and the parser's
/// ([`reylo_engine_core::fingerprint`]).
pub const FINGERPRINT: &str = reylo_engine_core::fingerprint!("REYLO_ENGINE_MYSQL_HASH", reylo_engine_core::fingerprint::parse_hex(syntax::SOURCE_HASH));
