//! Kanon Storage Module.
//!
//! Provides embedded persistence and per-plugin isolated data directories: the central
//! key-value store ([`KvStore`]) and each plugin's own directory ([`PluginDataDir`]).

pub mod dir;
pub mod id;
pub mod kv;

pub use dir::PluginDataDir;
pub use id::{PluginId, PluginIdError};
pub use kv::{DEFAULT_KV_FILE, KvError, KvStore, MAX_KEY_BYTES, MAX_VALUE_BYTES};
