//! Recall targets and pending notes retain independent, bounded memory windows.

use kanon_core::notice::RecallLedger;

fn queue_recall(ledger: &RecallLedger, event: &str, conversation: &str) {
    ledger.record_turn(event, conversation, event);
    assert!(ledger.note_recall(event, None));
}

#[test]
fn pending_conversations_evict_oldest_recalls_and_keep_returning_readers_correct() {
    let ledger = RecallLedger::default();
    for index in 0..1024 {
        queue_recall(
            &ledger,
            &format!("message-{index}"),
            &format!("conversation-{index}"),
        );
    }

    // A recent recall keeps this conversation in the window and preserves the existing
    // per-conversation limit: only its five newest notes await the next turn.
    for index in 0..6 {
        queue_recall(&ledger, &format!("refresh-{index}"), "conversation-0");
    }
    queue_recall(&ledger, "overflow", "new-conversation");
    assert!(ledger.take_notes("conversation-1").is_empty());
    let notes = ledger.take_notes("conversation-0");
    assert_eq!(notes.len(), 5);
    assert!(notes.first().unwrap().contains("refresh-1"));
    assert!(notes.last().unwrap().contains("refresh-5"));
    assert!(ledger.take_notes("conversation-0").is_empty());

    // Taking and later requeueing a conversation must not leave an old eviction position.
    queue_recall(&ledger, "returned", "conversation-0");
    queue_recall(&ledger, "next-overflow", "another-conversation");
    assert!(ledger.take_notes("conversation-2").is_empty());
    assert_eq!(ledger.take_notes("conversation-0").len(), 1);
    assert_eq!(ledger.take_notes("new-conversation").len(), 1);
    assert!(ledger.take_notes("conversation-0").is_empty());
}

#[test]
fn rotating_recall_targets_does_not_erase_an_already_queued_note() {
    let ledger = RecallLedger::default();
    queue_recall(&ledger, "seen-and-recalled", "returning-conversation");
    for index in 0..1024 {
        ledger.record_turn(&format!("later-{index}"), "other-conversation", "later");
    }

    let notes = ledger.take_notes("returning-conversation");
    assert_eq!(notes.len(), 1);
    assert!(notes[0].contains("seen-and-recalled"));
    assert!(ledger.take_notes("returning-conversation").is_empty());
    assert!(!ledger.note_recall("seen-and-recalled", None));
}
