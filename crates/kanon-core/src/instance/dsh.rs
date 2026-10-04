//! Remote routing identities, isolated from builtin history and generation choices.

use super::BotInstance;

impl BotInstance {
    /// Stable, filesystem-safe remote namespace for this instance's chat.
    /// Only the routing generation is stored locally; DSH owns the remote journal and metadata.
    #[cfg(feature = "dsh")]
    pub fn dsh_session_prefix(&self, conversation: &str) -> String {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(self.conversation_session_prefix(conversation).as_bytes());
        format!(
            "kanon-{}-",
            digest
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        )
    }

    /// Remote identity for a chosen generation, distinct from builtin session keys.
    #[cfg(feature = "dsh")]
    pub fn dsh_session_id_at(&self, conversation: &str, generation: u64) -> String {
        format!("{}{generation}", self.dsh_session_prefix(conversation))
    }

    /// Local routing pointer for the DSH backend; builtin keeps its own current generation.
    pub fn dsh_routing_key(conversation: &str) -> String {
        format!("agent:dsh:{conversation}")
    }

    /// Current remote routing generation, without consulting builtin's conversation choice.
    pub fn dsh_session_generation(&self, conversation: &str) -> u64 {
        self.session_generation(&Self::dsh_routing_key(conversation))
    }
}
