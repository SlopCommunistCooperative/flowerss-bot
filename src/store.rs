use std::{path::Path, sync::Arc};

use anyhow::{Context, Result};
use rusqlite::{Connection as SqliteConnection, params_from_iter, types::Value};
use sqlx::{
    ConnectOptions, MySql, MySqlConnection, MySqlPool, Row as _,
    mysql::{MySqlArguments, MySqlConnectOptions, MySqlPoolOptions, MySqlRow, MySqlSslMode},
    query::Query,
};
use tokio::sync::Mutex;

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
    pub enable_telegraph: bool,
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
    pub telegraph_url: String,
}

#[derive(Clone)]
pub struct Store {
    backend: Backend,
    db_log: bool,
}

#[derive(Clone)]
enum Backend {
    Sqlite(Arc<Mutex<SqliteConnection>>),
    Mysql(MySqlPool),
}

// These schemas mirror the GORM migrations. `contents.description` was never a
// column and there is no deletion marker on any of the four models.
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
        title TEXT, telegraph_url TEXT, created_at DATETIME, updated_at DATETIME,
        PRIMARY KEY (hash_id),
        CONSTRAINT fk_sources_content FOREIGN KEY (source_id) REFERENCES sources (id)
    )",
    "CREATE TABLE IF NOT EXISTS subscribes (
        id INTEGER, user_id INTEGER, source_id INTEGER, enable_notification INTEGER,
        enable_telegraph INTEGER, tag TEXT, `interval` INTEGER, wait_time INTEGER,
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
        raw_link LONGTEXT, title LONGTEXT, telegraph_url LONGTEXT,
        created_at DATETIME(3), updated_at DATETIME(3),
        CONSTRAINT fk_sources_content FOREIGN KEY (source_id) REFERENCES sources (id)
    )",
    "CREATE TABLE IF NOT EXISTS subscribes (
        id BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY, user_id BIGINT,
        source_id BIGINT UNSIGNED, enable_notification BIGINT, enable_telegraph BIGINT,
        tag LONGTEXT, `interval` BIGINT, wait_time BIGINT,
        created_at DATETIME(3), updated_at DATETIME(3)
    )",
];

const INSERT_SOURCE: &str = "INSERT INTO sources (title, link, error_count, created_at, updated_at)
                             VALUES (?, ?, ?, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)";
const FIND_SUBSCRIPTION: &str =
    "SELECT 1 FROM subscribes WHERE user_id = ? AND source_id = ? LIMIT 1";
const INSERT_SUBSCRIPTION: &str =
    "INSERT INTO subscribes (user_id, source_id, enable_notification, enable_telegraph, tag,
                             `interval`, wait_time, created_at, updated_at)
     VALUES (?, ?, 1, 1, '', ?, ?, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)";
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
        let mut connection = pool.acquire().await?;
        let log = config.log.db_log;
        for sql in MYSQL_SCHEMA {
            mysql_execute(&mut connection, log, sql, &[]).await?;
        }
        drop(connection);
        Ok(Self {
            backend: Backend::Mysql(pool),
            db_log: config.log.db_log,
        })
    }

    async fn open_sqlite(path: &Path, db_log: bool) -> Result<Self> {
        let path = path.to_owned();
        let connection = tokio::task::spawn_blocking(move || -> Result<_> {
            if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                std::fs::create_dir_all(parent)?;
            }
            let connection = SqliteConnection::open(&path)
                .with_context(|| format!("open SQLite database {}", path.display()))?;
            // Match legacy SQLite behavior even when the bundled library enables
            // foreign-key checks by default.
            connection.pragma_update(None, "foreign_keys", false)?;
            for sql in SQLITE_SCHEMA {
                sqlite_execute(&connection, db_log, sql, [])?;
            }
            Ok(connection)
        })
        .await??;
        Ok(Self {
            backend: Backend::Sqlite(Arc::new(Mutex::new(connection))),
            db_log,
        })
    }

    pub async fn find_source(&self, link: &str) -> Result<Option<Source>> {
        self.one(
            "SELECT id, title, link, error_count FROM sources WHERE link = ? ORDER BY id LIMIT 1",
            [Text(link)],
            |row| row.source(),
        )
        .await
    }

    pub async fn source(&self, id: i64) -> Result<Option<Source>> {
        self.one(
            "SELECT id, title, link, error_count FROM sources WHERE id = ?",
            [Int(id)],
            |row| row.source(),
        )
        .await
    }

    pub async fn sources(&self) -> Result<Vec<Source>> {
        self.many(
            "SELECT id, title, link, error_count FROM sources ORDER BY id",
            [],
            |row| row.source(),
        )
        .await
    }

    pub async fn add_source(&self, title: &str, link: &str, error_count: i64) -> Result<Source> {
        log_sql(self.db_log, INSERT_SOURCE);
        let id = match &self.backend {
            Backend::Sqlite(connection) => {
                let title = title.to_owned();
                let link = link.to_owned();
                return sqlite(connection, move |connection| {
                    let id = connection.prepare_cached(INSERT_SOURCE)?.insert((
                        &title,
                        &link,
                        error_count,
                    ))?;
                    Ok(Source {
                        id,
                        title,
                        link,
                        error_count,
                    })
                })
                .await;
            }
            Backend::Mysql(pool) => {
                mysql_query(INSERT_SOURCE, &[Text(title), Text(link), Int(error_count)])
                    .execute(pool)
                    .await?
                    .last_insert_id()
                    .try_into()?
            }
        };
        Ok(Source {
            id,
            title: title.to_owned(),
            link: link.to_owned(),
            error_count,
        })
    }

    pub async fn subscriptions(&self, user: i64) -> Result<Vec<(Source, Subscription)>> {
        self.many(
            "SELECT s.id, s.title, s.link, s.error_count, b.user_id, b.source_id,
                    b.enable_notification, b.enable_telegraph, b.`interval`, b.wait_time, b.tag
             FROM subscribes AS b INNER JOIN sources AS s ON b.source_id = s.id
             WHERE b.user_id = ? ORDER BY b.created_at DESC, b.id DESC",
            [Int(user)],
            |row| Ok((row.source()?, row.subscription(4)?)),
        )
        .await
    }

    pub async fn subscribers(&self, source: i64) -> Result<Vec<Subscription>> {
        self.many(
            "SELECT user_id, source_id, enable_notification, enable_telegraph, `interval`, wait_time, tag
             FROM subscribes WHERE source_id = ? ORDER BY created_at DESC, id DESC",
            [Int(source)],
            |row| row.subscription(0),
        )
        .await
    }

    pub async fn subscription(&self, user: i64, source: i64) -> Result<Option<Subscription>> {
        self.one(
            "SELECT user_id, source_id, enable_notification, enable_telegraph, `interval`, wait_time, tag
             FROM subscribes WHERE user_id = ? AND source_id = ? ORDER BY id LIMIT 1",
            [Int(user), Int(source)],
            |row| row.subscription(0),
        )
        .await
    }

    pub async fn add_subscription(&self, user: i64, source: i64, interval: i64) -> Result<bool> {
        let log = self.db_log;
        match &self.backend {
            Backend::Sqlite(connection) => {
                sqlite(connection, move |connection| {
                    if sqlite_exists(connection, log, FIND_SUBSCRIPTION, [user, source])? {
                        return Ok(false);
                    }
                    sqlite_execute(
                        connection,
                        log,
                        INSERT_SUBSCRIPTION,
                        [user, source, interval, interval],
                    )?;
                    Ok(true)
                })
                .await
            }
            Backend::Mysql(pool) => {
                let mut connection = pool.acquire().await?;
                if mysql_exists(
                    &mut connection,
                    log,
                    FIND_SUBSCRIPTION,
                    &[Int(user), Int(source)],
                )
                .await?
                {
                    return Ok(false);
                }
                mysql_execute(
                    &mut connection,
                    log,
                    INSERT_SUBSCRIPTION,
                    &[Int(user), Int(source), Int(interval), Int(interval)],
                )
                .await?;
                Ok(true)
            }
        }
    }

    pub async fn remove_subscription(&self, user: i64, source: i64) -> Result<bool> {
        let log = self.db_log;
        match &self.backend {
            Backend::Sqlite(connection) => {
                sqlite(connection, move |connection| {
                    let transaction = connection.transaction()?;
                    let removed =
                        sqlite_execute(&transaction, log, DELETE_SUBSCRIPTION, [user, source])?
                            != 0;
                    if removed && !sqlite_exists(&transaction, log, FIND_SUBSCRIBER, [source])? {
                        sqlite_execute(&transaction, log, DELETE_CONTENTS, [source])?;
                        sqlite_execute(&transaction, log, DELETE_SOURCE, [source])?;
                    }
                    transaction.commit()?;
                    Ok(removed)
                })
                .await
            }
            Backend::Mysql(pool) => {
                let mut transaction = pool.begin().await?;
                let removed = mysql_execute(
                    &mut transaction,
                    log,
                    DELETE_SUBSCRIPTION,
                    &[Int(user), Int(source)],
                )
                .await?
                    != 0;
                if removed
                    && !mysql_exists(&mut transaction, log, FIND_SUBSCRIBER, &[Int(source)]).await?
                {
                    // MySQL enforces the legacy contents FK, so delete children first.
                    mysql_execute(&mut transaction, log, DELETE_CONTENTS, &[Int(source)]).await?;
                    mysql_execute(&mut transaction, log, DELETE_SOURCE, &[Int(source)]).await?;
                }
                transaction.commit().await?;
                Ok(removed)
            }
        }
    }

    pub async fn save_subscription(&self, subscription: &Subscription) -> Result<()> {
        self.execute(
            "UPDATE subscribes SET enable_notification = ?, enable_telegraph = ?,
                 `interval` = ?, wait_time = ?, tag = ?, updated_at = CURRENT_TIMESTAMP
             WHERE user_id = ? AND source_id = ?",
            [
                Int(i64::from(subscription.enable_notification)),
                Int(i64::from(subscription.enable_telegraph)),
                Int(subscription.interval),
                Int(subscription.wait_time),
                Text(&subscription.tag),
                Int(subscription.user_id),
                Int(subscription.source_id),
            ],
        )
        .await
    }

    pub async fn set_error_count(&self, source: i64, count: i64) -> Result<()> {
        self.execute(
            "UPDATE sources SET error_count = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ?",
            [Int(count), Int(source)],
        )
        .await
    }

    pub async fn contains_content(&self, hash: &str) -> Result<bool> {
        Ok(self
            .one(
                "SELECT 1 FROM contents WHERE hash_id = ? LIMIT 1",
                [Text(hash)],
                |_| Ok(()),
            )
            .await?
            .is_some())
    }

    pub async fn add_content(&self, content: &Content) -> Result<()> {
        self.execute(
            "INSERT INTO contents (source_id, hash_id, raw_id, raw_link, title, telegraph_url,
                                   created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
            [
                Int(content.source_id),
                Text(&content.hash_id),
                Text(&content.raw_id),
                Text(&content.raw_link),
                Text(&content.title),
                Text(&content.telegraph_url),
            ],
        )
        .await
    }

    async fn execute<const N: usize>(
        &self,
        sql: &'static str,
        params: [Param<'_>; N],
    ) -> Result<()> {
        log_sql(self.db_log, sql);
        match &self.backend {
            Backend::Sqlite(connection) => {
                let values = params.map(Param::sqlite_value);
                sqlite(connection, move |connection| {
                    connection
                        .prepare_cached(sql)?
                        .execute(params_from_iter(values.iter()))?;
                    Ok(())
                })
                .await
            }
            Backend::Mysql(pool) => {
                mysql_query(sql, &params).execute(pool).await?;
                Ok(())
            }
        }
    }

    async fn one<T: Send + 'static, const N: usize>(
        &self,
        sql: &'static str,
        params: [Param<'_>; N],
        decode: impl FnOnce(DbRow<'_>) -> Result<T> + Send + 'static,
    ) -> Result<Option<T>> {
        log_sql(self.db_log, sql);
        match &self.backend {
            Backend::Sqlite(connection) => {
                let values = params.map(Param::sqlite_value);
                sqlite(connection, move |connection| {
                    let mut statement = connection.prepare_cached(sql)?;
                    let mut rows = statement.query(params_from_iter(values.iter()))?;
                    rows.next()?
                        .map(|row| decode(DbRow::Sqlite(row)))
                        .transpose()
                })
                .await
            }
            Backend::Mysql(pool) => mysql_query(sql, &params)
                .fetch_optional(pool)
                .await?
                .map(|row| decode(DbRow::Mysql(row)))
                .transpose(),
        }
    }

    async fn many<T: Send + Unpin + 'static, const N: usize>(
        &self,
        sql: &'static str,
        params: [Param<'_>; N],
        decode: impl Fn(DbRow<'_>) -> Result<T> + Send + 'static,
    ) -> Result<Vec<T>> {
        log_sql(self.db_log, sql);
        match &self.backend {
            Backend::Sqlite(connection) => {
                let values = params.map(Param::sqlite_value);
                sqlite(connection, move |connection| {
                    let mut statement = connection.prepare_cached(sql)?;
                    let mut rows = statement.query(params_from_iter(values.iter()))?;
                    let mut records = Vec::new();
                    while let Some(row) = rows.next()? {
                        records.push(decode(DbRow::Sqlite(row))?);
                    }
                    Ok(records)
                })
                .await
            }
            Backend::Mysql(pool) => Ok(mysql_query(sql, &params)
                .try_map(move |row| {
                    decode(DbRow::Mysql(row)).map_err(|err| sqlx::Error::Decode(err.into()))
                })
                .fetch_all(pool)
                .await?),
        }
    }
}

async fn sqlite<T: Send + 'static>(
    connection: &Arc<Mutex<SqliteConnection>>,
    operation: impl FnOnce(&mut SqliteConnection) -> Result<T> + Send + 'static,
) -> Result<T> {
    // Queue for the connection before dispatching so waiters don't occupy
    // blocking threads. The guard also keeps transactions serialized if the
    // caller is cancelled after the operation starts.
    let mut connection = Arc::clone(connection).lock_owned().await;
    tokio::task::spawn_blocking(move || operation(&mut connection)).await?
}

fn sqlite_execute(
    connection: &SqliteConnection,
    log: bool,
    sql: &str,
    params: impl rusqlite::Params,
) -> Result<usize> {
    log_sql(log, sql);
    Ok(connection.prepare_cached(sql)?.execute(params)?)
}

fn sqlite_exists(
    connection: &SqliteConnection,
    log: bool,
    sql: &str,
    params: impl rusqlite::Params,
) -> Result<bool> {
    log_sql(log, sql);
    Ok(connection.prepare_cached(sql)?.exists(params)?)
}

#[derive(Clone, Copy)]
enum Param<'a> {
    Int(i64),
    Text(&'a str),
}

use Param::{Int, Text};

impl Param<'_> {
    fn sqlite_value(self) -> Value {
        match self {
            Int(number) => Value::Integer(number),
            Text(text) => Value::Text(text.to_owned()),
        }
    }
}

fn mysql_query<'a>(sql: &'a str, values: &[Param<'a>]) -> Query<'a, MySql, MySqlArguments> {
    let mut query = sqlx::query(sql);
    for &value in values {
        query = match value {
            Int(number) => query.bind(number),
            Text(text) => query.bind(text),
        };
    }
    query
}

fn log_sql(enabled: bool, sql: &str) {
    if enabled {
        tracing::info!(target: "flowerss_bot::db", statement = sql, "SQL");
    }
}

async fn mysql_execute(
    connection: &mut MySqlConnection,
    log: bool,
    sql: &str,
    params: &[Param<'_>],
) -> Result<u64> {
    log_sql(log, sql);
    Ok(mysql_query(sql, params)
        .execute(connection)
        .await?
        .rows_affected())
}

async fn mysql_exists(
    connection: &mut MySqlConnection,
    log: bool,
    sql: &str,
    params: &[Param<'_>],
) -> Result<bool> {
    log_sql(log, sql);
    Ok(mysql_query(sql, params)
        .fetch_optional(connection)
        .await?
        .is_some())
}

enum DbRow<'a> {
    Sqlite(&'a rusqlite::Row<'a>),
    Mysql(MySqlRow),
}

impl DbRow<'_> {
    fn int(&self, index: usize) -> Result<i64> {
        Ok(match self {
            Self::Sqlite(row) => row.get::<_, Option<i64>>(index)?,
            Self::Mysql(row) => row.try_get::<Option<i64>, _>(index)?,
        }
        .unwrap_or_default())
    }

    fn uint(&self, index: usize) -> Result<i64> {
        match self {
            Self::Sqlite(_) => self.int(index),
            Self::Mysql(row) => Ok(row
                .try_get::<Option<u64>, _>(index)?
                .unwrap_or_default()
                .try_into()?),
        }
    }

    fn text(&self, index: usize) -> Result<String> {
        Ok(match self {
            Self::Sqlite(row) => row.get::<_, Option<String>>(index)?,
            Self::Mysql(row) => row.try_get::<Option<String>, _>(index)?,
        }
        .unwrap_or_default())
    }

    fn source(&self) -> Result<Source> {
        Ok(Source {
            id: self.uint(0)?,
            title: self.text(1)?,
            link: self.text(2)?,
            error_count: self.uint(3)?,
        })
    }

    fn subscription(&self, offset: usize) -> Result<Subscription> {
        Ok(Subscription {
            user_id: self.int(offset)?,
            source_id: self.uint(offset + 1)?,
            enable_notification: self.int(offset + 2)? == 1,
            enable_telegraph: self.int(offset + 3)? == 1,
            interval: self.int(offset + 4)?,
            wait_time: self.int(offset + 5)?,
            tag: self.text(offset + 6)?,
        })
    }
}

#[cfg(test)]
mod tests;
