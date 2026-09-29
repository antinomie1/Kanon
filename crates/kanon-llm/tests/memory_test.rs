//! Tests for the append-only conversation memory.
//!
//! The property that matters is not a feature but an absence: nothing shortens a session's history
//! except an explicit compaction, so the prompt prefix a provider cached last turn is still there
//! this turn.

use kanon_llm::gateway::types::{ChatMessage, Role};
use kanon_llm::memory::{InMemory, Memory, MemorySnapshot, SessionMemory};
use std::sync::Arc;

#[test]
fn history_is_never_trimmed_however_long_it_grows() {
    let mut session = SessionMemory::new();
    for i in 0..1000 {
        session.push_message(ChatMessage::user(format!("message {i}")));
    }

    // A sliding window would have dropped the oldest messages to stay bounded.
    assert_eq!(session.len(), 1000);
    assert_eq!(session.messages()[0].content.as_deref(), Some("message 0"));
    assert_eq!(
        session.messages()[999].content.as_deref(),
        Some("message 999")
    );
}

#[test]
fn a_session_never_holds_a_system_message() {
    // The persona and skill catalog are composed into each request; storing one per session would
    // be a second owner of the system prompt.
    let mut session = SessionMemory::new();
    session.push_message(ChatMessage::user("hi"));
    session.push_message(ChatMessage::assistant("hello"));
    assert!(session.messages().iter().all(|m| m.role != Role::System));
    assert!(session.summary().is_none());
}

#[test]
fn compaction_folds_the_covered_prefix_and_keeps_what_came_after() {
    let mut session = SessionMemory::new();
    for i in 1..=6 {
        session.push_message(ChatMessage::user(format!("q{i}")));
    }

    session
        .compact(4, "summary of q1-q4".to_string())
        .expect("compacted");

    assert_eq!(session.summary(), Some("summary of q1-q4"));
    let kept: Vec<&str> = session
        .messages()
        .iter()
        .filter_map(|m| m.content.as_deref())
        .collect();
    assert_eq!(kept, vec!["q5", "q6"]);

    // A second compaction replaces the summary: it was written from a prompt that contained it.
    session
        .compact(1, "summary of q1-q5".to_string())
        .expect("compacted");
    assert_eq!(session.summary(), Some("summary of q1-q5"));
    assert_eq!(session.len(), 1);
}

#[test]
fn compacting_more_than_exists_is_an_error_not_a_clamp() {
    let mut session = SessionMemory::new();
    session.push_message(ChatMessage::user("only one"));

    let error = session
        .compact(2, "s".to_string())
        .expect_err("cannot cover a message that is not there");
    assert!(error.to_string().contains("only holds 1"), "{error}");
    assert_eq!(session.len(), 1, "a refused compaction changes nothing");
    assert!(session.summary().is_none());
}

#[tokio::test]
async fn test_memory_trait_interface() {
    let memory: Arc<dyn Memory> = Arc::new(InMemory::new());
    let key = InMemory::make_session_key("chan_1", "user_alice");

    memory
        .push_message(&key, ChatMessage::user("Hello"))
        .await
        .unwrap();
    memory
        .push_message(&key, ChatMessage::assistant("Hi Alice!"))
        .await
        .unwrap();

    let history = memory.get_messages(&key).await.unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(memory.session_count().await.unwrap(), 1);

    memory.clear(&key).await.unwrap();
    assert_eq!(memory.session_count().await.unwrap(), 0);
    assert_eq!(
        memory.snapshot(&key).await.unwrap(),
        MemorySnapshot::default(),
        "an unknown session reads as empty"
    );
}

#[tokio::test]
async fn compaction_keeps_messages_appended_while_it_was_running() {
    // A compaction reads a snapshot, spends seconds asking a model for the summary, then folds. The
    // conversation does not stop meanwhile: what arrived in between must survive.
    let memory = InMemory::new();
    for i in 1..=4 {
        Memory::push_message(&memory, "s", ChatMessage::user(format!("old {i}")))
            .await
            .unwrap();
    }

    let snapshot = memory.snapshot("s").await.unwrap();
    let covered = snapshot.messages.len();

    // ... the summarization request is in flight; a new turn arrives ...
    Memory::push_message(&memory, "s", ChatMessage::user("arrived meanwhile"))
        .await
        .unwrap();

    memory
        .compact_history("s", covered, "summary".to_string())
        .await
        .unwrap();

    let after = memory.snapshot("s").await.unwrap();
    assert_eq!(after.summary.as_deref(), Some("summary"));
    assert_eq!(after.messages.len(), 1);
    assert_eq!(
        after.messages[0].content.as_deref(),
        Some("arrived meanwhile")
    );
}

#[tokio::test]
async fn clearing_a_session_removes_its_summary_too() {
    let memory = InMemory::new();
    Memory::push_message(&memory, "s", ChatMessage::user("hi"))
        .await
        .unwrap();
    memory
        .compact_history("s", 1, "sum".to_string())
        .await
        .unwrap();
    assert!(memory.snapshot("s").await.unwrap().summary.is_some());

    Memory::clear(&memory, "s").await.unwrap();
    assert_eq!(
        memory.snapshot("s").await.unwrap(),
        MemorySnapshot::default()
    );
}

#[tokio::test]
async fn sessions_are_isolated() {
    let memory = InMemory::new();
    Memory::push_message(&memory, "a", ChatMessage::user("for a"))
        .await
        .unwrap();
    Memory::push_message(&memory, "b", ChatMessage::user("for b"))
        .await
        .unwrap();
    memory
        .compact_history("a", 1, "sum a".to_string())
        .await
        .unwrap();

    assert_eq!(
        memory.snapshot("a").await.unwrap().summary.as_deref(),
        Some("sum a")
    );
    let b = memory.snapshot("b").await.unwrap();
    assert!(b.summary.is_none());
    assert_eq!(b.messages.len(), 1);
}
