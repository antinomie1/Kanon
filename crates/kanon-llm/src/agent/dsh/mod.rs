//! Optional DSH agent adapter. The independent client crate owns the remote protocol;
//! these functions adapt Kanon's current message and read-only legacy conversation view.

mod history;
mod message;

pub use history::conversation_messages;
pub use kanon_dsh::*;
pub use message::message_content;
