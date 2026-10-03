//! The central KV store: namespaces, expiry, limits and durability across reopen.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use kanon_storage::{KvError, KvStore, MAX_KEY_BYTES, MAX_VALUE_BYTES};

#[test]
fn values_survive_reopening_and_plugins_never_see_each_others_keys() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("data").join("kv.db");
    {
        let kv = KvStore::open(&path).unwrap();
        kv.set("weather", "city", b"Paris", None).unwrap();
        kv.set("weather", "city", b"Tokyo", None).unwrap();
        kv.set("other", "city", b"Berlin", None).unwrap();
    }
    let kv = KvStore::open(&path).unwrap();
    assert_eq!(
        kv.get("weather", "city").unwrap().as_deref(),
        Some(&b"Tokyo"[..])
    );
    assert_eq!(
        kv.get("other", "city").unwrap().as_deref(),
        Some(&b"Berlin"[..])
    );

    assert!(kv.delete("weather", "city").unwrap());
    assert!(!kv.delete("weather", "city").unwrap(), "already gone");
    assert_eq!(kv.get("weather", "city").unwrap(), None);
    assert!(kv.get("other", "city").unwrap().is_some());
}

#[test]
fn listing_is_per_plugin_sorted_and_the_prefix_is_literal() {
    let kv = KvStore::open_in_memory().unwrap();
    for key in ["user:2", "user:1", "user_1", "config"] {
        kv.set("p", key, b"x", None).unwrap();
    }
    kv.set("q", "user:3", b"x", None).unwrap();

    assert_eq!(kv.list("p", "user:").unwrap(), ["user:1", "user:2"]);
    assert_eq!(kv.list("p", "user_").unwrap(), ["user_1"]);
    assert_eq!(kv.list("p", "").unwrap().len(), 4);
}

#[test]
fn an_expired_key_is_invisible_everywhere() {
    let kv = KvStore::open_in_memory().unwrap();
    kv.set("p", "token", b"secret", Some(Duration::from_millis(1)))
        .unwrap();
    kv.set("p", "kept", b"v", Some(Duration::from_secs(3600)))
        .unwrap();
    // Deadlines round up to a whole clock second, adding less than one second to the TTL.
    std::thread::sleep(Duration::from_millis(2100));

    assert_eq!(kv.get("p", "token").unwrap(), None);
    assert_eq!(kv.list("p", "").unwrap(), ["kept"]);
    assert!(!kv.delete("p", "token").unwrap());

    // Setting again without a TTL makes the key permanent.
    kv.set("p", "kept", b"v2", None).unwrap();
    assert_eq!(kv.get("p", "kept").unwrap().as_deref(), Some(&b"v2"[..]));
}

#[test]
fn ttl_rounds_the_deadline_up_instead_of_expiring_early() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("kv.db");
    let kv = KvStore::open(&path).unwrap();
    let conn = rusqlite::Connection::open(&path).unwrap();

    for (key, ttl) in [
        ("whole", Duration::from_secs(10)),
        ("fractional", Duration::from_millis(10_250)),
    ] {
        let before = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
        kv.set("p", key, b"v", Some(ttl)).unwrap();
        let after = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
        let expires_at: i64 = conn
            .query_row("SELECT expires_at FROM kv WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .unwrap();
        let deadline = Duration::from_secs(expires_at.try_into().unwrap());
        assert!(deadline >= before + ttl, "{key} expires before its TTL");
        assert!(
            deadline < after + ttl + Duration::from_secs(1),
            "{key} rounds by more than one second"
        );
    }
}

#[test]
fn maximum_ttl_saturates_and_zero_ttl_expires_immediately() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("kv.db");
    let kv = KvStore::open(&path).unwrap();
    kv.set("p", "maximum", b"v", Some(Duration::MAX)).unwrap();
    kv.set("p", "zero", b"v", Some(Duration::ZERO)).unwrap();
    assert_eq!(kv.list("p", "").unwrap(), ["maximum"]);
    assert_eq!(kv.get("p", "zero").unwrap(), None);
    drop(kv);

    let reopened = KvStore::open(&path).unwrap();
    assert_eq!(
        reopened.get("p", "maximum").unwrap().as_deref(),
        Some(&b"v"[..])
    );
    let conn = rusqlite::Connection::open(&path).unwrap();
    let expires_at: i64 = conn
        .query_row(
            "SELECT expires_at FROM kv WHERE key = 'maximum'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(expires_at, i64::MAX);
}

#[test]
fn invalid_input_is_refused_before_it_reaches_the_database() {
    let kv = KvStore::open_in_memory().unwrap();
    assert!(matches!(
        kv.set("../x", "k", b"v", None),
        Err(KvError::PluginId(_))
    ));
    assert!(matches!(kv.set("p", "", b"v", None), Err(KvError::Key(0))));
    let long_key = "k".repeat(MAX_KEY_BYTES + 1);
    assert!(matches!(kv.get("p", &long_key), Err(KvError::Key(_))));
    let big = vec![0u8; MAX_VALUE_BYTES + 1];
    assert!(matches!(
        kv.set("p", "k", &big, None),
        Err(KvError::ValueTooLarge(_))
    ));
    assert!(kv.list("p", "").unwrap().is_empty());
}
