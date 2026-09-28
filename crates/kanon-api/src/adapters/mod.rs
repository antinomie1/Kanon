//! Bundled platform adapters.
//!
//! In-process adapters (Milky, OneBot) live in their own crates and are registered by the
//! composition root; platform-specific adapters written as plugins implement the same
//! [`kanon_core::PlatformAdapter`] contract and are registered the same way. The contract — not
//! any particular bridge — is the extension point.
