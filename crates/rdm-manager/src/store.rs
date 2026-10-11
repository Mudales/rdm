//! SQLite persistence.
//!
//! Settings (including categories and queues) are one JSON document; each
//! download is a row. The schema version lives in `PRAGMA user_version` and
//! migrations run in order on open. Calls are synchronous: local SQLite in
//! WAL mode answers in well under a millisecond, so they run inline rather
//! than hopping to a blocking thread.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::error::{Error, Result};
use crate::model::{DownloadRecord, Id, Status};
use crate::settings::Settings;

const MIGRATIONS: &[&str] = &[
    // 1: initial schema
    "CREATE TABLE settings (
         key   TEXT PRIMARY KEY,
         value TEXT NOT NULL
     );
     CREATE TABLE downloads (
         id           INTEGER PRIMARY KEY AUTOINCREMENT,
         url          TEXT    NOT NULL,
         file_name    TEXT    NOT NULL,
         directory    TEXT    NOT NULL,
         category_id  INTEGER,
         queue_id     INTEGER NOT NULL,
         status       TEXT    NOT NULL,
         error        TEXT,
         size         INTEGER,
         downloaded   INTEGER NOT NULL DEFAULT 0,
         resumable    INTEGER,
         referer      TEXT,
         user_agent   TEXT,
         connections  INTEGER,
         speed_limit  INTEGER,
         added_at     INTEGER NOT NULL,
         completed_at INTEGER,
         position     INTEGER NOT NULL
     );
     CREATE INDEX downloads_status ON downloads(status);",
    // 2: stream downloads
    "ALTER TABLE downloads ADD COLUMN stream TEXT;",
];

const COLUMNS: &str = "id, url, file_name, directory, category_id, queue_id, status, error, \
     size, downloaded, resumable, referer, user_agent, connections, speed_limit, added_at, \
     completed_at, position, stream";

#[derive(Debug)]
pub(crate) struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let store = Self {
            conn: Mutex::new(conn),
        };
        store.migrate()?;
        Ok(store)
    }

    fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn migrate(&self) -> Result<()> {
        let mut conn = self.conn();
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        let version = usize::try_from(version).unwrap_or(usize::MAX);
        if version > MIGRATIONS.len() {
            return Err(Error::Corrupt(format!(
                "database schema {version} is newer than this version of rdm supports"
            )));
        }
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(version) {
            let tx = conn.transaction()?;
            tx.execute_batch(sql)?;
            tx.pragma_update(
                None,
                "user_version",
                i64::try_from(i + 1).unwrap_or(i64::MAX),
            )?;
            tx.commit()?;
        }
        Ok(())
    }

    pub fn load_settings(&self) -> Result<Option<Settings>> {
        let json: Option<String> = self
            .conn()
            .query_row("SELECT value FROM settings WHERE key = 'app'", [], |r| {
                r.get(0)
            })
            .optional()?;
        json.map(|j| serde_json::from_str(&j).map_err(|e| Error::Corrupt(e.to_string())))
            .transpose()
    }

    pub fn save_settings(&self, settings: &Settings) -> Result<()> {
        let json = serde_json::to_string(settings).map_err(|e| Error::Corrupt(e.to_string()))?;
        self.conn().execute(
            "INSERT INTO settings (key, value) VALUES ('app', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [json],
        )?;
        Ok(())
    }

    pub fn load_downloads(&self) -> Result<Vec<DownloadRecord>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {COLUMNS} FROM downloads ORDER BY position"
        ))?;
        let rows = stmt.query_map([], from_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row??);
        }
        Ok(out)
    }

    /// Inserts `record` (its `id` is ignored) and returns the new id.
    pub fn insert(&self, r: &DownloadRecord) -> Result<Id> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO downloads (url, file_name, directory, category_id, queue_id, status,
                 error, size, downloaded, resumable, referer, user_agent, connections,
                 speed_limit, added_at, completed_at, position, stream)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
            params![
                r.url,
                r.file_name,
                path_str(&r.directory),
                r.category_id,
                r.queue_id,
                r.status.as_str(),
                r.error,
                r.size.map(to_i64),
                to_i64(r.downloaded),
                r.resumable,
                r.referer,
                r.user_agent,
                r.connections,
                r.speed_limit.map(to_i64),
                r.added_at,
                r.completed_at,
                r.position,
                stream_json(r)?,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn update(&self, r: &DownloadRecord) -> Result<()> {
        self.conn().execute(
            "UPDATE downloads SET url = ?2, file_name = ?3, directory = ?4, category_id = ?5,
                 queue_id = ?6, status = ?7, error = ?8, size = ?9, downloaded = ?10,
                 resumable = ?11, referer = ?12, user_agent = ?13, connections = ?14,
                 speed_limit = ?15, added_at = ?16, completed_at = ?17, position = ?18,
                 stream = ?19
             WHERE id = ?1",
            params![
                r.id,
                r.url,
                r.file_name,
                path_str(&r.directory),
                r.category_id,
                r.queue_id,
                r.status.as_str(),
                r.error,
                r.size.map(to_i64),
                to_i64(r.downloaded),
                r.resumable,
                r.referer,
                r.user_agent,
                r.connections,
                r.speed_limit.map(to_i64),
                r.added_at,
                r.completed_at,
                r.position,
                stream_json(r)?,
            ],
        )?;
        Ok(())
    }

    pub fn delete(&self, id: Id) -> Result<()> {
        self.conn()
            .execute("DELETE FROM downloads WHERE id = ?1", [id])?;
        Ok(())
    }
}

fn stream_json(r: &DownloadRecord) -> Result<Option<String>> {
    r.stream
        .as_ref()
        .map(|s| serde_json::to_string(s).map_err(|e| Error::Corrupt(e.to_string())))
        .transpose()
}

fn to_i64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

fn to_u64(v: i64) -> u64 {
    u64::try_from(v).unwrap_or_default()
}

fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// Maps a row; the inner `Result` reports values that are not ours.
fn from_row(row: &Row<'_>) -> rusqlite::Result<Result<DownloadRecord>> {
    let status: String = row.get(6)?;
    let Some(status) = Status::parse(&status) else {
        return Ok(Err(Error::Corrupt(format!("unknown status `{status}`"))));
    };
    let stream = match row.get::<_, Option<String>>(18)? {
        Some(json) => match serde_json::from_str(&json) {
            Ok(s) => Some(s),
            Err(e) => return Ok(Err(Error::Corrupt(format!("bad stream settings: {e}")))),
        },
        None => None,
    };
    Ok(Ok(DownloadRecord {
        id: row.get(0)?,
        url: row.get(1)?,
        file_name: row.get(2)?,
        directory: PathBuf::from(row.get::<_, String>(3)?),
        category_id: row.get(4)?,
        queue_id: row.get(5)?,
        status,
        error: row.get(7)?,
        size: row.get::<_, Option<i64>>(8)?.map(to_u64),
        downloaded: to_u64(row.get(9)?),
        resumable: row.get(10)?,
        referer: row.get(11)?,
        user_agent: row.get(12)?,
        connections: row.get(13)?,
        speed_limit: row.get::<_, Option<i64>>(14)?.map(to_u64),
        added_at: row.get(15)?,
        completed_at: row.get(16)?,
        position: row.get(17)?,
        stream,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> DownloadRecord {
        DownloadRecord {
            id: 0,
            url: "https://example.com/a.zip".into(),
            file_name: "a.zip".into(),
            directory: PathBuf::from("/tmp/dl"),
            category_id: Some(4),
            queue_id: 1,
            status: Status::Queued,
            error: None,
            size: Some(1234),
            downloaded: 12,
            resumable: Some(true),
            referer: Some("https://example.com/".into()),
            user_agent: None,
            connections: Some(4),
            speed_limit: None,
            added_at: 1_700_000_000,
            completed_at: None,
            position: 1,
            stream: Some(crate::model::StreamChoice {
                variant: Some("v2".into()),
                label: Some("720p".into()),
                ..Default::default()
            }),
        }
    }

    #[test]
    fn roundtrip_and_migrations_are_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db.sqlite3");
        let store = Store::open(&path).unwrap();
        assert_eq!(store.load_settings().unwrap(), None);

        let mut r = record();
        r.id = store.insert(&r).unwrap();
        r.status = Status::Completed;
        r.completed_at = Some(1_700_000_100);
        store.update(&r).unwrap();
        let settings = Settings::with_directory(Path::new("/tmp/dl"));
        store.save_settings(&settings).unwrap();
        drop(store);

        let store = Store::open(&path).unwrap();
        assert_eq!(store.load_downloads().unwrap(), vec![r.clone()]);
        assert_eq!(store.load_settings().unwrap(), Some(settings));
        store.delete(r.id).unwrap();
        assert!(store.load_downloads().unwrap().is_empty());
    }

    #[test]
    fn upgrades_a_version_1_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db.sqlite3");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(MIGRATIONS[0]).unwrap();
            conn.pragma_update(None, "user_version", 1).unwrap();
            conn.execute(
                "INSERT INTO downloads (url, file_name, directory, queue_id, status, added_at, position)
                 VALUES ('https://x/a.zip', 'a.zip', '/tmp', 1, 'stopped', 1, 0)",
                [],
            )
            .unwrap();
        }
        let store = Store::open(&path).unwrap();
        let rows = store.load_downloads().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].stream, None);
        let mut r = rows[0].clone();
        r.stream = Some(crate::model::StreamChoice::default());
        store.update(&r).unwrap();
        assert_eq!(store.load_downloads().unwrap()[0].stream, r.stream);
    }

    #[test]
    fn refuses_newer_schema() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db.sqlite3");
        drop(Store::open(&path).unwrap());
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "user_version", 99).unwrap();
        drop(conn);
        assert!(matches!(Store::open(&path), Err(Error::Corrupt(_))));
    }
}
