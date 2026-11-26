use std::{
    io::Write,
    process::{Command, Stdio},
};

use super::*;

#[tokio::test]
async fn legacy_sqlite_persists_and_cleans_up() -> Result<()> {
    const CHAT: i64 = -1009876543210;
    const SHARED: i64 = 123456;
    const FEED: &str = "https://example.org/feed";
    const HASH: &str = "c37ebe96";

    // Seed with native SQLite to exercise a rollback-mode file, not a file
    // already created by the engine under test.
    let fixture = tempfile::tempdir()?;
    let path = fixture.path().join("legacy.db");
    let mut sqlite = Command::new("sqlite3")
        .args(["-batch", "-bail"])
        .arg(&path)
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("sqlite3 is required to create the native SQLite fixture")?;
    sqlite
        .stdin
        .take()
        .unwrap()
        .write_all(include_bytes!("legacy.sql"))?;
    let result = sqlite.wait_with_output()?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let store = Store::open_sqlite(&path, false).await?;
    let source = store.find_source(FEED).await?.unwrap();
    assert_eq!(
        source,
        Source {
            id: 42,
            title: "Existing feed".into(),
            link: FEED.into(),
            error_count: 2
        }
    );
    assert_eq!(store.sources().await?.len(), 2);
    assert!(store.contains_content(HASH).await?);
    let mut subscriptions = store.subscriptions(CHAT).await?;
    assert_eq!(
        subscriptions.iter().map(|(s, _)| s.id).collect::<Vec<_>>(),
        [42, 20]
    );
    assert_eq!(subscriptions[1].0.error_count, 0);
    let mut subscription = subscriptions.remove(0).1;
    assert_eq!(
        subscription,
        Subscription {
            user_id: CHAT,
            source_id: source.id,
            enable_notification: false,
            enable_telegraph: true,
            interval: 11,
            wait_time: 3,
            tag: "news".into(),
        }
    );
    assert_eq!(
        store.subscription(SHARED, source.id).await?.unwrap().tag,
        ""
    );
    assert!(!store.add_subscription(CHAT, source.id, 30).await?);
    subscription.enable_notification = true;
    subscription.enable_telegraph = false;
    subscription.interval = 17;
    subscription.wait_time = 0;
    subscription.tag.clear();
    store.save_subscription(&subscription).await?;
    store.set_error_count(source.id, 0).await?;

    let added = store
        .add_source("New feed", "https://example.org/new", 7)
        .await?;
    assert_eq!(added.id, 43);
    assert!(store.add_subscription(SHARED, added.id, 8).await?);
    let article = Content {
        source_id: source.id,
        hash_id: "1234abcd".into(),
        raw_id: "new-id".into(),
        raw_link: "https://example.org/new-item".into(),
        title: "新文章".into(),
        description: "transient HTML".into(),
        telegraph_url: "https://telegra.ph/new".into(),
    };
    store.add_content(&article).await?;
    drop(store);

    let store = Store::open_sqlite(&path, false).await?;
    assert_eq!(
        store.subscription(CHAT, source.id).await?,
        Some(subscription)
    );
    assert_eq!(store.source(source.id).await?.unwrap().error_count, 0);
    assert_eq!(store.source(added.id).await?, Some(added.clone()));
    assert!(store.contains_content(HASH).await?);
    assert!(store.contains_content(&article.hash_id).await?);
    assert_eq!(
        store.subscription(SHARED, added.id).await?.unwrap(),
        Subscription {
            user_id: SHARED,
            source_id: added.id,
            enable_notification: true,
            enable_telegraph: true,
            interval: 8,
            wait_time: 8,
            tag: String::new(),
        }
    );
    assert!(!store.remove_subscription(0, source.id).await?);
    assert!(store.remove_subscription(CHAT, source.id).await?);
    assert!(store.source(source.id).await?.is_some());
    assert!(store.contains_content(HASH).await?);
    assert_eq!(store.subscribers(source.id).await?.len(), 1);
    assert!(store.remove_subscription(SHARED, source.id).await?);
    assert!(store.source(source.id).await?.is_none());
    assert!(!store.contains_content(HASH).await?);
    assert!(!store.contains_content(&article.hash_id).await?);
    assert!(store.source(added.id).await?.is_some());
    assert!(!store.remove_subscription(SHARED, source.id).await?);
    drop(store);

    let result = Command::new("sqlite3")
        .arg(&path)
        .arg("SELECT id,link FROM sources ORDER BY id")
        .output()?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        String::from_utf8(result.stdout)?,
        "20|https://example.org/old\n43|https://example.org/new\n"
    );
    Ok(())
}

#[tokio::test]
async fn sqlite_rolls_back_failed_orphan_cleanup() -> Result<()> {
    let store = Store::open_sqlite(Path::new(":memory:"), false).await?;
    let source = store
        .add_source("Feed", "https://example.org/feed", 0)
        .await?;
    let user = 123;
    assert!(store.add_subscription(user, source.id, 10).await?);
    let content = Content {
        source_id: source.id,
        title: "Article".into(),
        description: String::new(),
        raw_id: "article".into(),
        hash_id: "hash".into(),
        raw_link: "https://example.org/article".into(),
        telegraph_url: String::new(),
    };
    store.add_content(&content).await?;

    let Backend::Sqlite(connection) = &store.backend else {
        unreachable!("expected SQLite store");
    };
    sqlite(connection, |connection| {
        connection.execute_batch(
            "CREATE TEMP TRIGGER reject_source_delete BEFORE DELETE ON sources
             BEGIN SELECT RAISE(ABORT, 'reject source delete'); END;",
        )?;
        Ok(())
    })
    .await?;

    let error = store
        .remove_subscription(user, source.id)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("reject source delete"));
    assert!(store.subscription(user, source.id).await?.is_some());
    assert!(store.contains_content(&content.hash_id).await?);
    assert!(store.source(source.id).await?.is_some());

    sqlite(connection, |connection| {
        connection.execute_batch("DROP TRIGGER reject_source_delete")?;
        Ok(())
    })
    .await?;
    assert!(store.remove_subscription(user, source.id).await?);
    assert!(store.subscription(user, source.id).await?.is_none());
    assert!(!store.contains_content(&content.hash_id).await?);
    assert!(store.source(source.id).await?.is_none());
    Ok(())
}
