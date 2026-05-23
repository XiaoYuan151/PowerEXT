use anyhow::Result;
use rusqlite::{Connection, params};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

#[allow(dead_code)]
pub struct BackupRow {
    pub id: i64,
    pub backup_path: String,
    pub original_ext: String,
}

pub struct Db(pub Arc<Mutex<Connection>>);

impl Db {
    pub fn open(data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir.join("backups"))?;
        let conn = Connection::open(data_dir.join("PowerEXT.db"))?;
        conn.execute_batch("
            CREATE TABLE IF NOT EXISTS backups (
                id            INTEGER PRIMARY KEY AUTOINCREMENT,
                original_path TEXT    NOT NULL,
                backup_path   TEXT    NOT NULL,
                original_ext  TEXT    NOT NULL,
                new_ext       TEXT,
                event_time    INTEGER NOT NULL,
                restored      INTEGER NOT NULL DEFAULT 0
            );
            CREATE UNIQUE INDEX IF NOT EXISTS idx_original_path
                ON backups(original_path) WHERE restored = 0;
        ")?;
        Ok(Self(Arc::new(Mutex::new(conn))))
    }

    pub fn find_active_backup(&self, path: &Path) -> Option<BackupRow> {
        let conn = self.0.lock().unwrap();
        conn.query_row(
            "SELECT id, backup_path, original_ext FROM backups WHERE original_path = ?1 AND restored = 0",
            params![path.to_string_lossy().as_ref()],
            |row| Ok(BackupRow { id: row.get(0)?, backup_path: row.get(1)?, original_ext: row.get(2)? }),
        ).ok()
    }

    pub fn insert_backup(&self, original: &Path, backup: &Path, orig_ext: &str, new_ext: Option<&str>) -> Result<()> {
        let ts = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;
        self.0.lock().unwrap().execute(
            "INSERT OR REPLACE INTO backups (original_path, backup_path, original_ext, new_ext, event_time) VALUES (?1,?2,?3,?4,?5)",
            params![original.to_string_lossy().as_ref(), backup.to_string_lossy().as_ref(), orig_ext, new_ext, ts],
        )?;
        Ok(())
    }

    pub fn set_new_ext(&self, original: &Path, new_ext: &str) -> Result<()> {
        self.0.lock().unwrap().execute(
            "UPDATE backups SET new_ext = ?1 WHERE original_path = ?2 AND restored = 0",
            params![new_ext, original.to_string_lossy().as_ref()],
        )?;
        Ok(())
    }

    pub fn mark_restored(&self, id: i64) -> Result<()> {
        self.0.lock().unwrap().execute(
            "UPDATE backups SET restored = 1 WHERE id = ?1",
            params![id],
        )?;
        Ok(())
    }
}
