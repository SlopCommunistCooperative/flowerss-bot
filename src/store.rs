use std::path::Path;

use anyhow::{Context, Result};
use sqlx::{
    ConnectOptions, Database, MySqlPool, Row as _, SqlitePool,
    mysql::{MySqlConnectOptions, MySqlPoolOptions, MySqlRow, MySqlSslMode},
    query::Query,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions, SqliteRow},
};

use crate::config::Config;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    pub id: i64,
    pub title: String,
    pub link: String,
    pub error_count: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Subscription {
    pub user_id: i64,
    pub source_id: i64,
    pub enable_notification: bool,
    pub interval: i64,
    pub wait_time: i64,
    pub tag: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Content {
    pub source_id: i64,
    pub title: String,
    pub description: String,
    pub raw_id: String,
    pub hash_id: String,
    pub raw_link: String,
}

#[derive(Clone)]
pub struct Store {
    backend: Backend,
    db_log: bool,
}

#[derive(Clone)]
enum Backend {
    Sqlite(SqlitePool),
    Mysql(MySqlPool),
}

// Share SQL and transaction logic without erasing native row types (MySQL has unsigned IDs).
macro_rules! with_pool {
    ($store:expr, |$pool:ident| $body:expr) => {
        match &$store.backend {
            Backend::Sqlite($pool) => $body,
            Backend::Mysql($pool) => $body,
        }
    };
}

// Existing GORM databases can have extra columns. The bot uses only the fields below.
const SQLITE_SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS users (
        id INTEGER, created_at DATETIME, updated_at DATETIME, PRIMARY KEY (id)
    )",
    "CREATE TABLE IF NOT EXISTS sources (
        id INTEGER, link TEXT, title TEXT, error_count INTEGER,
        created_at DATETIME, updated_at DATETIME, PRIMARY KEY (id)
    )",
    "CREATE TABLE IF NOT EXISTS contents (
        source_id INTEGER, hash_id TEXT, raw_id TEXT, raw_link TEXT,
        title TEXT, created_at DATETIME, updated_at DATETIME,
        PRIMARY KEY (hash_id),
        CONSTRAINT fk_sources_content FOREIGN KEY (source_id) REFERENCES sources (id)
    )",
    "CREATE TABLE IF NOT EXISTS subscribes (
        id INTEGER, user_id INTEGER, source_id INTEGER, enable_notification INTEGER,
        tag TEXT, `interval` INTEGER, wait_time INTEGER,
        created_at DATETIME, updated_at DATETIME, PRIMARY KEY (id)
    )",
];

const MYSQL_SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS users (
        id BIGINT AUTO_INCREMENT PRIMARY KEY, created_at DATETIME(3), updated_at DATETIME(3)
    )",
    "CREATE TABLE IF NOT EXISTS sources (
        id BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY, link LONGTEXT, title LONGTEXT,
        error_count BIGINT UNSIGNED, created_at DATETIME(3), updated_at DATETIME(3)
    )",
    "CREATE TABLE IF NOT EXISTS contents (
        source_id BIGINT UNSIGNED, hash_id VARCHAR(191) PRIMARY KEY, raw_id LONGTEXT,
        raw_link LONGTEXT, title LONGTEXT,
        created_at DATETIME(3), updated_at DATETIME(3),
        CONSTRAINT fk_sources_content FOREIGN KEY (source_id) REFERENCES sources (id)
    )",
    "CREATE TABLE IF NOT EXISTS subscribes (
        id BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY, user_id BIGINT,
        source_id BIGINT UNSIGNED, enable_notification BIGINT,
        tag LONGTEXT, `interval` BIGINT, wait_time BIGINT,
        created_at DATETIME(3), updated_at DATETIME(3)
    )",
];

const INSERT_SOURCE: &str = "INSERT INTO sources (title, link, error_count, created_at, updated_at)
                             VALUES (?, ?, ?, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)";
const FIND_SUBSCRIPTION: &str =
    "SELECT 1 FROM subscribes WHERE user_id = ? AND source_id = ? LIMIT 1";
const INSERT_SUBSCRIPTION: &str =
    "INSERT INTO subscribes (user_id, source_id, enable_notification, tag,
                             `interval`, wait_time, created_at, updated_at)
     VALUES (?, ?, 1, '', ?, ?, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)";
const DELETE_SUBSCRIPTION: &str = "DELETE FROM subscribes WHERE user_id = ? AND source_id = ?";
const FIND_SUBSCRIBER: &str = "SELECT 1 FROM subscribes WHERE source_id = ? LIMIT 1";
const DELETE_CONTENTS: &str = "DELETE FROM contents WHERE source_id = ?";
const DELETE_SOURCE: &str = "DELETE FROM sources WHERE id = ?";

impl Store {
    pub async fn open(config: &Config) -> Result<Self> {
        if config.mysql.host.is_empty() {
            return Self::open_sqlite(&config.sqlite.path, config.log.db_log).await;
        }

        let options = MySqlConnectOptions::new()
            .host(&config.mysql.host)
            .port(config.mysql.port)
            .username(&config.mysql.user)
            .password(&config.mysql.password)
            .database(&config.mysql.database)
            .ssl_mode(MySqlSslMode::Disabled)
            .disable_statement_logging();
        // The legacy schema has no unique constraint on (user_id, source_id).
        // Serialize operations, including duplicate checks and orphan cleanup.
        let pool = MySqlPoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .context("connect to MySQL")?;
        let store = Self {
            backend: Backend::Mysql(pool),
            db_log: config.log.db_log,
        };
        store.create_tables(MYSQL_SCHEMA).await?;
        Ok(store)
    }

    async fn open_sqlite(path: &Path, db_log: bool) -> Result<Self> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            tokio::fs::create_dir_all(parent).await?;
        }
        let options = SqliteConnectOptions::new()
            .filename(path)
            .in_memory(path == Path::new(":memory:"))
            .create_if_missing(true)
            // Match legacy SQLite behavior.
            .foreign_keys(false)
            .disable_statement_logging();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .idle_timeout(None)
            .max_lifetime(None)
            .test_before_acquire(false)
            .connect_with(options)
            .await
            .with_context(|| format!("open SQLite database {}", path.display()))?;
        let store = Self {
            backend: Backend::Sqlite(pool),
            db_log,
        };
        store.create_tables(SQLITE_SCHEMA).await?;
        Ok(store)
    }

    async fn create_tables(&self, schema: &[&str]) -> Result<()> {
        with_pool!(self, |pool| {
            let mut conn = pool.acquire().await?;
            for &sql in schema {
                self.query(sql).execute(&mut *conn).await?;
            }
        });
        Ok(())
    }

    pub async fn find_source(&self, link: &str) -> Result<Option<Source>> {
        Ok(with_pool!(self, |pool| {
            self.query(
                "SELECT id, title, link, error_count FROM sources WHERE link = ? ORDER BY id LIMIT 1",
            )
            .bind(link)
            .try_map(|row| DbRow::from(row).source())
            .fetch_optional(pool)
            .await?
        }))
    }

    pub async fn source(&self, id: i64) -> Result<Option<Source>> {
        Ok(with_pool!(self, |pool| {
            self.query("SELECT id, title, link, error_count FROM sources WHERE id = ?")
                .bind(id)
                .try_map(|row| DbRow::from(row).source())
                .fetch_optional(pool)
                .await?
        }))
    }

    pub async fn sources(&self) -> Result<Vec<Source>> {
        Ok(with_pool!(self, |pool| {
            self.query("SELECT id, title, link, error_count FROM sources ORDER BY id")
                .try_map(|row| DbRow::from(row).source())
                .fetch_all(pool)
                .await?
        }))
    }

    pub async fn add_source(&self, title: &str, link: &str, error_count: i64) -> Result<Source> {
        let id = match &self.backend {
            Backend::Sqlite(pool) => self
                .query(INSERT_SOURCE)
                .bind(title)
                .bind(link)
                .bind(error_count)
                .execute(pool)
                .await?
                .last_insert_rowid(),
            Backend::Mysql(pool) => self
                .query(INSERT_SOURCE)
                .bind(title)
                .bind(link)
                .bind(error_count)
                .execute(pool)
                .await?
                .last_insert_id()
                .try_into()?,
        };
        Ok(Source {
            id,
            title: title.to_owned(),
            link: link.to_owned(),
            error_count,
        })
    }

    pub async fn subscriptions(&self, user: i64) -> Result<Vec<(Source, Subscription)>> {
        Ok(with_pool!(self, |pool| {
            self.query(
                "SELECT s.id, s.title, s.link, s.error_count, b.user_id, b.source_id,
                        b.enable_notification, b.`interval`, b.wait_time, b.tag
                 FROM subscribes AS b INNER JOIN sources AS s ON b.source_id = s.id
                 WHERE b.user_id = ? ORDER BY b.created_at DESC, b.id DESC",
            )
            .bind(user)
            .try_map(|row| {
                let row = DbRow::from(row);
                Ok((row.source()?, row.subscription(4)?))
            })
            .fetch_all(pool)
            .await?
        }))
    }

    pub async fn subscribers(&self, source: i64) -> Result<Vec<Subscription>> {
        Ok(with_pool!(self, |pool| {
            self.query(
                "SELECT user_id, source_id, enable_notification, `interval`, wait_time, tag
                 FROM subscribes WHERE source_id = ? ORDER BY created_at DESC, id DESC",
            )
            .bind(source)
            .try_map(|row| DbRow::from(row).subscription(0))
            .fetch_all(pool)
            .await?
        }))
    }

    pub async fn subscription(&self, user: i64, source: i64) -> Result<Option<Subscription>> {
        Ok(with_pool!(self, |pool| {
            self.query(
                "SELECT user_id, source_id, enable_notification, `interval`, wait_time, tag
                 FROM subscribes WHERE user_id = ? AND source_id = ? ORDER BY id LIMIT 1",
            )
            .bind(user)
            .bind(source)
            .try_map(|row| DbRow::from(row).subscription(0))
            .fetch_optional(pool)
            .await?
        }))
    }

    pub async fn add_subscription(&self, user: i64, source: i64, interval: i64) -> Result<bool> {
        with_pool!(self, |pool| {
            // Keep the only pooled connection until the check and insert are both done.
            let mut conn = pool.acquire().await?;
            if self
                .query(FIND_SUBSCRIPTION)
                .bind(user)
                .bind(source)
                .fetch_optional(&mut *conn)
                .await?
                .is_some()
            {
                return Ok(false);
            }
            self.query(INSERT_SUBSCRIPTION)
                .bind(user)
                .bind(source)
                .bind(interval)
                .bind(interval)
                .execute(&mut *conn)
                .await?;
        });
        Ok(true)
    }

    pub async fn remove_subscription(&self, user: i64, source: i64) -> Result<bool> {
        Ok(with_pool!(self, |pool| {
            let mut tx = pool.begin().await?;
            let removed = self
                .query(DELETE_SUBSCRIPTION)
                .bind(user)
                .bind(source)
                .execute(&mut *tx)
                .await?
                .rows_affected()
                != 0;
            if removed
                && self
                    .query(FIND_SUBSCRIBER)
                    .bind(source)
                    .fetch_optional(&mut *tx)
                    .await?
                    .is_none()
            {
                // MySQL enforces the legacy contents FK, so delete children first.
                self.query(DELETE_CONTENTS)
                    .bind(source)
                    .execute(&mut *tx)
                    .await?;
                self.query(DELETE_SOURCE)
                    .bind(source)
                    .execute(&mut *tx)
                    .await?;
            }
            tx.commit().await?;
            removed
        }))
    }

    pub async fn save_subscription(&self, subscription: &Subscription) -> Result<()> {
        with_pool!(self, |pool| {
            self.query(
                "UPDATE subscribes SET enable_notification = ?, `interval` = ?, wait_time = ?,
                     tag = ?, updated_at = CURRENT_TIMESTAMP
                 WHERE user_id = ? AND source_id = ?",
            )
            .bind(i64::from(subscription.enable_notification))
            .bind(subscription.interval)
            .bind(subscription.wait_time)
            .bind(&subscription.tag)
            .bind(subscription.user_id)
            .bind(subscription.source_id)
            .execute(pool)
            .await?;
        });
        Ok(())
    }

    pub async fn set_error_count(&self, source: i64, count: i64) -> Result<()> {
        with_pool!(self, |pool| {
            self.query(
                "UPDATE sources SET error_count = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ?",
            )
            .bind(count)
            .bind(source)
            .execute(pool)
            .await?;
        });
        Ok(())
    }

    pub async fn contains_content(&self, hash: &str) -> Result<bool> {
        Ok(with_pool!(self, |pool| {
            self.query("SELECT 1 FROM contents WHERE hash_id = ? LIMIT 1")
                .bind(hash)
                .fetch_optional(pool)
                .await?
                .is_some()
        }))
    }

    pub async fn add_content(&self, content: &Content) -> Result<()> {
        with_pool!(self, |pool| {
            self.query(
                "INSERT INTO contents (source_id, hash_id, raw_id, raw_link, title,
                                       created_at, updated_at)
                 VALUES (?, ?, ?, ?, ?, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
            )
            .bind(content.source_id)
            .bind(&content.hash_id)
            .bind(&content.raw_id)
            .bind(&content.raw_link)
            .bind(&content.title)
            .execute(pool)
            .await?;
        });
        Ok(())
    }

    fn query<'q, DB: Database>(&self, sql: &'q str) -> Query<'q, DB, DB::Arguments<'q>> {
        if self.db_log {
            tracing::info!(target: "flowerss_bot::db", statement = sql, "SQL");
        }
        sqlx::query(sql)
    }
}

enum DbRow {
    Sqlite(SqliteRow),
    Mysql(MySqlRow),
}

impl From<SqliteRow> for DbRow {
    fn from(row: SqliteRow) -> Self {
        Self::Sqlite(row)
    }
}

impl From<MySqlRow> for DbRow {
    fn from(row: MySqlRow) -> Self {
        Self::Mysql(row)
    }
}

impl DbRow {
    fn int(&self, index: usize) -> sqlx::Result<i64> {
        Ok(match self {
            Self::Sqlite(row) => row.try_get::<Option<i64>, _>(index)?,
            Self::Mysql(row) => row.try_get::<Option<i64>, _>(index)?,
        }
        .unwrap_or_default())
    }

    fn uint(&self, index: usize) -> sqlx::Result<i64> {
        match self {
            Self::Sqlite(_) => self.int(index),
            Self::Mysql(row) => row
                .try_get::<Option<u64>, _>(index)?
                .unwrap_or_default()
                .try_into()
                .map_err(|err| sqlx::Error::Decode(Box::new(err))),
        }
    }

    fn text(&self, index: usize) -> sqlx::Result<String> {
        Ok(match self {
            Self::Sqlite(row) => row.try_get::<Option<String>, _>(index)?,
            Self::Mysql(row) => row.try_get::<Option<String>, _>(index)?,
        }
        .unwrap_or_default())
    }

    fn source(&self) -> sqlx::Result<Source> {
        Ok(Source {
            id: self.uint(0)?,
            title: self.text(1)?,
            link: self.text(2)?,
            error_count: self.uint(3)?,
        })
    }

    fn subscription(&self, offset: usize) -> sqlx::Result<Subscription> {
        Ok(Subscription {
            user_id: self.int(offset)?,
            source_id: self.uint(offset + 1)?,
            enable_notification: self.int(offset + 2)? == 1,
            interval: self.int(offset + 3)?,
            wait_time: self.int(offset + 4)?,
            tag: self.text(offset + 5)?,
        })
    }
}

#[cfg(test)]
mod tests;
