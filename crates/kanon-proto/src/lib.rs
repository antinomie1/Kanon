#![allow(clippy::result_large_err)]

// Generated protobuf stubs for Kanon
pub mod v1 {
    tonic::include_proto!("kanon.plugin.v1");
}

pub use prost_types;

pub mod json;

/// Optional external-agent bridge, separate from the plugin SDK contract.
#[cfg(feature = "dsh")]
pub mod agent {
    /// External agent protocol version one.
    pub mod v1 {
        tonic::include_proto!("kanon.agent.v1");
    }
}
// Match the protobuf package path used by imported plugin message definitions.
#[cfg(feature = "dsh")]
mod plugin {
    pub use crate::v1;
}
