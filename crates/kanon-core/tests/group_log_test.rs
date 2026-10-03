//! Retired group readers must not accumulate after their observed lines leave the log.

use kanon_core::pipeline::group_log::GroupLog;

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
