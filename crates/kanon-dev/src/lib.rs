//! Developer toolchain library for Kanon plugin lifecycle management.
//!
//! Provides scaffolding, static manifest linting, distribution archive packaging (.kpk),
//! offline interactive and automated sandbox testing, and hot reload against a running node.

pub mod build;
pub mod dev;
pub mod lint;
pub mod pack;
pub mod sandbox;
pub mod scaffold;

pub use build::{BuildError, build_plugin};
pub use dev::{DEFAULT_NODE_URL, DevError, run_dev};
pub use lint::{LintError, LintReport, lint_plugin};
pub use pack::{PackError, PackReport, pack_plugin};
pub use sandbox::{SandboxError, SandboxOptions, run_sandbox};
pub use scaffold::{ScaffoldError, create_plugin_project};
