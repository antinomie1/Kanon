//! Retired group readers must not accumulate after their observed lines leave the log.

use kanon_core::pipeline::group_log::GroupLog;
use std::time::Duration;

#[tokio::test(start_paused = true)]
async fn recording_reclaims_expired_groups_without_revisiting_them() {
    let log = GroupLog::default();
    let old_seq = log.record("retired-group", "speaker", "expired line");
    log.mark_seen("retired-group", "retired-reader", old_seq);
    tokio::time::advance(Duration::from_secs(29 * 60)).await;
    let active_seq = log.record("active-group", "speaker", "recent line");
    log.mark_seen("active-group", "active-reader", active_seq);
    tokio::time::advance(Duration::from_secs(2 * 60)).await;

    log.record("new-group", "speaker", "new line");
    let retained = format!("{log:?}");
    assert!(!retained.contains("retired-group"));
    assert!(!retained.contains("retired-reader"));
    assert!(log.unseen("active-group", "active-reader").is_empty());
    assert_eq!(
        log.unseen("active-group", "new-reader"),
        vec![("speaker".into(), "recent line".into())]
    );

    let seq = log.record("retired-group", "speaker", "returned");
    assert!(seq > old_seq);
    assert_eq!(
        log.unseen("retired-group", "retired-reader"),
        vec![("speaker".into(), "returned".into())]
    );
}

#[test]
fn obsolete_cursors_are_reclaimed_without_replaying_seen_lines() {
    let log = GroupLog::default();
    for index in 0..100 {
        let seq = log.record("group", "speaker", &format!("line-{index}"));
        log.mark_seen("group", &format!("retired-reader-{index:03}"), seq);
        log.mark_seen("group", "active-reader", seq);
    }

    // The existing diagnostic view lets this regression verify retained keys without exposing
    // cache internals as new production API solely for the test.
    let retained = format!("{log:?}");
    assert!(!retained.contains("retired-reader-000"));
    assert!(retained.matches("retired-reader-").count() <= 30);
    assert!(retained.contains("active-reader"));
    assert!(log.unseen("group", "active-reader").is_empty());

    // An evicted cursor is indistinguishable from a new reader: both see all surviving lines.
    let unseen = log.unseen("group", "retired-reader-000");
    assert_eq!(unseen, log.unseen("group", "new-reader"));
    assert_eq!(unseen.len(), 30);
    assert_eq!(unseen.first().unwrap().1, "line-70");
    assert_eq!(unseen.last().unwrap().1, "line-99");

    let seq = log.record("group", "speaker", "new line");
    assert_eq!(
        log.unseen("group", "active-reader"),
        vec![("speaker".into(), "new line".into())]
    );
    log.mark_seen("group", "active-reader", seq);
    assert!(log.unseen("group", "active-reader").is_empty());
}
