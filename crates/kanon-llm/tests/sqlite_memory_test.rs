//! Integration tests for embedded SQLite-backed conversation memory (`SqliteMemory`).

use kanon_llm::gateway::types::{ChatMessage, Role, ToolCall};
use kanon_llm::memory::Memory;
use kanon_llm::sqlite_memory::SqliteMemory;
use std::sync::Arc;
use tempfile::tempdir;

#[tokio::test]
async fn test_sqlite_memory_in_memory_isolation() {
    let memory = SqliteMemory::open_in_memory().expect("Failed to initialize in-memory SQLite");

    memory
        .push_message("session_a", ChatMessage::user("Hello A"))
        .await
        .unwrap();
    memory
        .push_message("session_b", ChatMessage::user("Hello B"))
        .await
        .unwrap();

    // Verify session isolation
    let msgs_a = memory.get_messages("session_a").await.unwrap();
    assert_eq!(msgs_a.len(), 1);
    assert_eq!(msgs_a[0].content.as_deref(), Some("Hello A"));

    let msgs_b = memory.get_messages("session_b").await.unwrap();
    assert_eq!(msgs_b.len(), 1);
    assert_eq!(msgs_b[0].content.as_deref(), Some("Hello B"));

    assert_eq!(memory.session_count().await.unwrap(), 2);
}

#[tokio::test]
async fn test_sqlite_memory_file_persistence_and_reload() {
    let dir = tempdir().expect("Failed to create temporary directory");
    let db_path = dir.path().join("test_memory.db");

    // Phase 1: Write messages with tool calls and drop the memory instance
    {
        let memory = SqliteMemory::open(&db_path).expect("Failed to open SQLite database");

        memory
            .push_message("sess_tools", ChatMessage::user("Check weather in Tokyo"))
            .await
            .unwrap();

        let tool_call = ToolCall {
            id: "call_tokyo_001".to_string(),
            name: "get_weather".to_string(),
            arguments: serde_json::json!({ "city": "Tokyo", "units": "celsius" }),
        };
        memory
            .push_message(
                "sess_tools",
                ChatMessage::assistant_tool_calls(vec![tool_call], None),
            )
            .await
            .unwrap();

        memory
            .push_message(
                "sess_tools",
                ChatMessage::tool_response(
                    "call_tokyo_001",
                    r#"{"temp": 18, "condition": "Cloudy"}"#,
                ),
            )
            .await
            .unwrap();

        memory
            .push_message(
                "sess_tools",
                ChatMessage::assistant("It is currently 18°C and cloudy in Tokyo."),
            )
            .await
            .unwrap();
    }

    // Phase 2: Open a fresh SqliteMemory instance on the persisted database file
    {
        let reloaded = SqliteMemory::open(&db_path).expect("Failed to reload SQLite database");

        let messages = reloaded.get_messages("sess_tools").await.unwrap();
        // 1 user + 1 assistant tool call + 1 tool response + 1 assistant final = 4
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0].role, Role::User);

        // Verify tool call round-trip deserialization
        assert_eq!(messages[1].role, Role::Assistant);
        let calls = messages[1].tool_calls.as_ref().expect("Tool calls missing");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_tokyo_001");
        assert_eq!(calls[0].name, "get_weather");
        assert_eq!(calls[0].arguments["city"], "Tokyo");

        // Verify tool response
        assert_eq!(messages[2].role, Role::Tool);
        assert_eq!(messages[2].tool_call_id.as_deref(), Some("call_tokyo_001"));
        assert!(messages[2].content.as_deref().unwrap().contains("Cloudy"));

        // Verify final assistant message
        assert_eq!(messages[3].role, Role::Assistant);
        assert!(messages[3].content.as_deref().unwrap().contains("18°C"));
    }
}

#[tokio::test]
async fn test_sqlite_history_is_append_only_on_disk_and_in_the_cache() {
    let dir = tempdir().expect("Failed to create temporary directory");
    let db_path = dir.path().join("append_only.db");

    let memory = SqliteMemory::open(&db_path).expect("Failed to open SQLite database");
    for i in 1..=50 {
        memory
            .push_message("sess", ChatMessage::user(format!("Message {i}")))
            .await
            .unwrap();
    }

    // Nothing was pruned: a sliding window would have dropped the oldest messages by now.
    let msgs = memory.get_messages("sess").await.unwrap();
    assert_eq!(msgs.len(), 50);
    assert_eq!(msgs[0].content.as_deref(), Some("Message 1"));
    assert_eq!(msgs[49].content.as_deref(), Some("Message 50"));

    let reloaded = SqliteMemory::open(&db_path).expect("Failed to reload SQLite database");
    let disk_msgs = reloaded.get_messages("sess").await.unwrap();
    assert_eq!(disk_msgs.len(), 50);
    assert_eq!(disk_msgs[0].content.as_deref(), Some("Message 1"));
}

#[tokio::test]
async fn test_sqlite_memory_clear_and_session_count() {
    let memory = SqliteMemory::open_in_memory().expect("Failed to open SQLite database");

    memory
        .push_message("s1", ChatMessage::user("Hello 1"))
        .await
        .unwrap();
    memory
        .push_message("s2", ChatMessage::user("Hello 2"))
        .await
        .unwrap();

    assert_eq!(memory.session_count().await.unwrap(), 2);

    memory.clear("s1").await.unwrap();
    assert_eq!(memory.session_count().await.unwrap(), 1);
    assert!(memory.get_messages("s1").await.unwrap().is_empty());
    assert_eq!(memory.get_messages("s2").await.unwrap().len(), 1);
}

#[tokio::test]
async fn test_sqlite_memory_lru_cache_eviction_and_reload() {
    let dir = tempdir().expect("Failed to create temporary directory");
    let db_path = dir.path().join("lru_test.db");

    // Configure memory with capacity for only 2 cached sessions in RAM
    let memory = SqliteMemory::open(&db_path)
        .expect("Failed to open SQLite database")
        .with_cache_capacity(2);

    assert_eq!(memory.cache_capacity(), 2);

    // Populate session 1, then compact it so the summary must survive eviction too
    memory
        .push_message("s1", ChatMessage::user("Hello from 1"))
        .await
        .unwrap();
    memory
        .push_message("s1", ChatMessage::assistant("Reply to 1"))
        .await
        .unwrap();
    memory
        .compact_history("s1", 1, "Summary 1".to_string())
        .await
        .unwrap();

    memory
        .push_message("s2", ChatMessage::user("Hello from 2"))
        .await
        .unwrap();

    // Populate session 3 (this triggers LRU eviction of the least recently used session, s1)
    memory
        .push_message("s3", ChatMessage::user("Hello from 3"))
        .await
        .unwrap();

    // Total sessions tracked in SQLite is 3
    assert_eq!(memory.session_count().await.unwrap(), 3);

    // Now query s1: It was evicted from RAM cache, but should be transparently
    // reloaded from SQLite back into cache — summary included.
    let s1 = memory.snapshot("s1").await.unwrap();
    assert_eq!(s1.summary.as_deref(), Some("Summary 1"));
    assert_eq!(s1.messages.len(), 1);
    assert_eq!(s1.messages[0].content.as_deref(), Some("Reply to 1"));

    // Query s3: Should also still be valid
    let s3_msgs = memory.get_messages("s3").await.unwrap();
    assert_eq!(s3_msgs.len(), 1);
    assert_eq!(s3_msgs[0].content.as_deref(), Some("Hello from 3"));
}

#[tokio::test]
async fn test_sqlite_memory_open_in_dir_and_commit_points() {
    use kanon_llm::PersistentMemory;

    let dir = tempdir().expect("Failed to create temporary directory");
    let nested_dir = dir.path().join("isolated_plugin_dir");

    // Test PersistentMemory alias and open_in_dir
    let memory: PersistentMemory = PersistentMemory::open_in_dir(&nested_dir, "plugin_memory.db")
        .expect("Failed to open SQLite database in dir");

    assert!(
        nested_dir.exists(),
        "Directory should have been automatically created"
    );
    assert!(
        nested_dir.join("plugin_memory.db").exists(),
        "DB file should exist"
    );

    memory
        .push_message("commit_sess", ChatMessage::user("Testing commit"))
        .await
        .unwrap();

    // Test commit point and session commit
    memory
        .commit_point()
        .await
        .expect("WAL checkpoint commit failed");
    memory.flush().await.expect("Flush failed");
    memory
        .commit_session("commit_sess")
        .await
        .expect("Session commit failed");

    let msgs = memory.get_messages("commit_sess").await.unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].content.as_deref(), Some("Testing commit"));
}

#[tokio::test]
async fn test_sqlite_compaction_is_atomic_and_keeps_later_messages() {
    let dir = tempdir().expect("Failed to create temporary directory");
    let db_path = dir.path().join("compaction.db");

    let memory = SqliteMemory::open(&db_path).expect("Failed to open SQLite database");
    for i in 1..=5 {
        memory
            .push_message("sess", ChatMessage::user(format!("Old Message {i}")))
            .await
            .unwrap();
    }

    let snapshot = memory.snapshot("sess").await.unwrap();
    assert_eq!(snapshot.messages.len(), 5);
    assert!(snapshot.summary.is_none());

    // A message arrives after the snapshot was taken but before the compaction lands.
    memory
        .push_message("sess", ChatMessage::user("Arrived meanwhile"))
        .await
        .unwrap();

    memory
        .compact_history(
            "sess",
            snapshot.messages.len(),
            "Discussed items 1 to 5.".to_string(),
        )
        .await
        .unwrap();

    // Verify cache has been updated
    let after = memory.snapshot("sess").await.unwrap();
    assert_eq!(after.summary.as_deref(), Some("Discussed items 1 to 5."));
    assert_eq!(after.messages.len(), 1);
    assert_eq!(
        after.messages[0].content.as_deref(),
        Some("Arrived meanwhile")
    );

    // Verify disk has also been updated by reloading through a cold connection
    let reloaded = SqliteMemory::open(&db_path).expect("Failed to reload SQLite database");
    let disk = reloaded.snapshot("sess").await.unwrap();
    assert_eq!(disk.summary.as_deref(), Some("Discussed items 1 to 5."));
    assert_eq!(disk.messages.len(), 1);
    assert_eq!(
        disk.messages[0].content.as_deref(),
        Some("Arrived meanwhile")
    );

    // New messages append after the surviving ones, in order.
    reloaded
        .push_message("sess", ChatMessage::assistant("Later"))
        .await
        .unwrap();
    let order: Vec<String> = reloaded
        .get_messages("sess")
        .await
        .unwrap()
        .into_iter()
        .filter_map(|m| m.content)
        .collect();
    assert_eq!(order, vec!["Arrived meanwhile", "Later"]);
}

#[tokio::test]
async fn test_sqlite_compaction_refuses_to_cover_more_than_exists_and_changes_nothing() {
    let memory = SqliteMemory::open_in_memory().expect("Failed to open SQLite database");
    memory
        .push_message("sess", ChatMessage::user("only message"))
        .await
        .unwrap();

    let error = memory
        .compact_history("sess", 3, "bogus".to_string())
        .await
        .expect_err("cannot cover three messages of one");
    assert!(error.to_string().contains("only holds 1"), "{error}");

    let snapshot = memory.snapshot("sess").await.unwrap();
    assert_eq!(
        snapshot.messages.len(),
        1,
        "the refused compaction lost nothing"
    );
    assert!(snapshot.summary.is_none());
}

#[tokio::test]
async fn test_sqlite_database_from_an_earlier_version_is_upgraded_in_place() {
    // Databases written before compaction existed have `sessions.system_prompt` and no
    // `sessions.summary`. They must keep working: the history survives and the stale column is
    // simply ignored, because the persona is composed per request and never stored.
    let dir = tempdir().expect("Failed to create temporary directory");
    let db_path = dir.path().join("legacy.db");
    {
        let conn = rusqlite::Connection::open(&db_path).expect("legacy db");
        conn.execute_batch(
            "CREATE TABLE sessions (
                 session_key TEXT PRIMARY KEY,
                 system_prompt TEXT,
                 updated_at INTEGER NOT NULL
             );
             CREATE TABLE messages (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 session_key TEXT NOT NULL,
                 role TEXT NOT NULL,
                 content TEXT,
                 tool_calls TEXT,
                 tool_call_id TEXT,
                 name TEXT,
                 created_at INTEGER NOT NULL
             );
             INSERT INTO sessions VALUES ('old', 'You are a legacy persona.', 1);
             INSERT INTO messages (session_key, role, content, created_at)
                 VALUES ('old', 'user', 'from the past', 1);",
        )
        .expect("seed legacy schema");
    }

    let memory = SqliteMemory::open(&db_path).expect("open upgrades the schema");
    let snapshot = memory.snapshot("old").await.unwrap();
    assert!(snapshot.summary.is_none());
    assert_eq!(snapshot.messages.len(), 1);
    assert_eq!(
        snapshot.messages[0].content.as_deref(),
        Some("from the past")
    );

    // And it can be appended to and compacted like a fresh database.
    memory
        .push_message("old", ChatMessage::assistant("still works"))
        .await
        .unwrap();
    memory
        .compact_history("old", 1, "legacy summary".to_string())
        .await
        .unwrap();
    let reloaded = SqliteMemory::open(&db_path).expect("reopen");
    let snapshot = reloaded.snapshot("old").await.unwrap();
    assert_eq!(snapshot.summary.as_deref(), Some("legacy summary"));
    assert_eq!(snapshot.messages.len(), 1);
}

#[tokio::test]
async fn test_sqlite_memory_concurrent_same_session_ordering() {
    let dir = tempdir().expect("Failed to create temporary directory");
    let db_path = dir.path().join("concurrent_order.db");

    let memory = Arc::new(SqliteMemory::open(&db_path).expect("Failed to open SQLite database"));
    let session_key = "concurrent_sess";

    let mut handles = Vec::new();
    for i in 0..25 {
        let mem = Arc::clone(&memory);
        handles.push(tokio::spawn(async move {
            mem.push_message(session_key, ChatMessage::user(format!("msg_{i:02}")))
                .await
                .unwrap();
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }

    // 1. Fetch from active memory cache
    let cache_msgs = memory.get_messages(session_key).await.unwrap();
    assert_eq!(cache_msgs.len(), 25);

    // 2. Fetch directly from a cold SQLite connection (bypassing the original in-memory cache)
    let cold_memory = SqliteMemory::open(&db_path).expect("Failed to reload SQLite database");
    let disk_msgs = cold_memory.get_messages(session_key).await.unwrap();
    assert_eq!(disk_msgs.len(), 25);

    // 3. Verify absolute 1:1 order consistency between memory cache and SQLite disk persistence
    for (idx, (c, d)) in cache_msgs.iter().zip(disk_msgs.iter()).enumerate() {
        assert_eq!(
            c.content, d.content,
            "Message mismatch at index {idx}: cache had {:?} but disk had {:?}",
            c.content, d.content
        );
    }
}
