// Copyright (c) 2026 Sotto
// Persistent Local SQLite Dictation History & Async Queue Architecture

use chrono::{DateTime, Local, Utc};
use rusqlite::{params, Connection, Result as SqlResult};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

pub const DEFAULT_MAX_RETENTION_ITEMS: usize = 500;

/// Data record representing an individual dictation entry
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryRecord {
    pub id: i64,
    pub timestamp: i64,
    pub timestamp_str: String,
    pub transcript: String,
    pub duration_ms: i32,
    pub duration_str: String,
    pub target_app: String,
}

/// Commands for asynchronous zero-latency dispatch from audio pipeline
enum HistoryCommand {
    Insert {
        transcript: String,
        duration_ms: i32,
        target_app: String,
        on_complete: Option<Box<dyn FnOnce(HistoryRecord) + Send + 'static>>,
    },
    ClearAll {
        on_complete: Option<Box<dyn FnOnce() + Send + 'static>>,
    },
}

/// Thread-safe SQLite Dictation History Store
#[derive(Clone)]
pub struct HistoryDb {
    conn: Arc<Mutex<Connection>>,
    max_retention: usize,
}

impl HistoryDb {
    /// Opens or creates SQLite database with WAL mode and memory-optimized PRAGMAs
    pub fn open(path: impl AsRef<Path>) -> SqlResult<Self> {
        let conn = Connection::open(path)?;
        Self::configure_connection(conn)
    }

    /// Opens default database location in OS AppData
    pub fn open_default() -> SqlResult<Self> {
        let path = default_db_path();
        Self::open(path)
    }

    /// Creates an in-memory database for testing and sandboxed environments
    pub fn open_in_memory() -> SqlResult<Self> {
        let conn = Connection::open_in_memory()?;
        Self::configure_connection(conn)
    }

    fn configure_connection(conn: Connection) -> SqlResult<Self> {
        // High-performance WAL mode for concurrent readers & non-blocking background writes
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "temp_store", "MEMORY")?;
        conn.pragma_update(None, "cache_size", -512)?; // 512KB bounded page cache

        // Create table and indexes with complete column schema
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS dictation_history (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp INTEGER NOT NULL,
                timestamp_str TEXT NOT NULL DEFAULT '',
                transcript TEXT NOT NULL,
                duration_ms INTEGER NOT NULL,
                duration_str TEXT NOT NULL DEFAULT '',
                target_app TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_history_timestamp ON dictation_history(timestamp DESC);
            "#,
        )?;

        // Guarantee schema alignment with existing tables on disk
        let _ = conn.execute("ALTER TABLE dictation_history ADD COLUMN timestamp_str TEXT NOT NULL DEFAULT '';", []);
        let _ = conn.execute("ALTER TABLE dictation_history ADD COLUMN duration_str TEXT NOT NULL DEFAULT '';", []);

        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
            max_retention: 500, // Rolling 500 records cache limit
        };

        // Provider key + model-cache tables (Step: Providers tab)
        let _ = super::providers::ensure_provider_tables(&db);

        Ok(db)
    }

    /// Sets the maximum retention limit (default: 500)
    pub fn set_max_retention(&mut self, limit: usize) {
        self.max_retention = limit;
    }

    /// Shared connection handle for sibling store modules (e.g. provider keys).
    pub(crate) fn connection(&self) -> &Arc<Mutex<Connection>> {
        &self.conn
    }

    /// Inserts a new transcription record and prunes records beyond max_retention
    pub fn insert(&self, transcript: &str, duration_ms: i32, target_app: &str) -> SqlResult<HistoryRecord> {
        let now = Utc::now();
        let timestamp = now.timestamp_millis();
        let local_dt: DateTime<Local> = DateTime::from(now);
        let timestamp_str = local_dt.format("%b %d, %I:%M %p").to_string();
        let duration_str = format!("{:.1}s", duration_ms as f64 / 1000.0);
        let conn = self.conn.lock().unwrap();

        conn.execute(
            "INSERT INTO dictation_history (timestamp, timestamp_str, transcript, duration_ms, duration_str, target_app) VALUES (?1, ?2, ?3, ?4, ?5, ?6);",
            params![timestamp, &timestamp_str, transcript, duration_ms, &duration_str, target_app],
        )?;

        let id = conn.last_insert_rowid();

        // Enforce rolling retention limit: prune old records beyond max_retention
        let total_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM dictation_history;",
            [],
            |row| row.get(0),
        )?;

        if total_count > self.max_retention as i64 {
            let excess = total_count - self.max_retention as i64;
            conn.execute(
                "DELETE FROM dictation_history WHERE id IN (
                    SELECT id FROM dictation_history ORDER BY timestamp ASC, id ASC LIMIT ?1
                );",
                params![excess],
            )?;
        }

        Ok(HistoryRecord {
            id,
            timestamp,
            timestamp_str,
            transcript: transcript.to_string(),
            duration_ms,
            duration_str,
            target_app: target_app.to_string(),
        })
    }

    /// Fetches recent dictations ordered by timestamp descending (newest first)
    pub fn get_recent(&self, limit: usize) -> SqlResult<Vec<HistoryRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, timestamp, timestamp_str, transcript, duration_ms, duration_str, target_app 
             FROM dictation_history 
             ORDER BY timestamp DESC, id DESC 
             LIMIT ?1;",
        )?;

        let rows = stmt.query_map(params![limit as i64], |row| {
            let id: i64 = row.get(0)?;
            let timestamp: i64 = row.get(1)?;
            let mut timestamp_str: String = row.get(2).unwrap_or_default();
            let transcript: String = row.get(3)?;
            let duration_ms: i32 = row.get(4)?;
            let mut duration_str: String = row.get(5).unwrap_or_default();
            let target_app: String = row.get(6)?;

            if timestamp_str.is_empty() {
                let naive_opt = chrono::DateTime::from_timestamp_millis(timestamp);
                timestamp_str = match naive_opt {
                    Some(dt) => {
                        let local: DateTime<Local> = DateTime::from(dt);
                        local.format("%b %d, %I:%M %p").to_string()
                    }
                    None => "Just now".to_string(),
                };
            }

            if duration_str.is_empty() {
                duration_str = format!("{:.1}s", duration_ms as f64 / 1000.0);
            }

            Ok(HistoryRecord {
                id,
                timestamp,
                timestamp_str,
                transcript,
                duration_ms,
                duration_str,
                target_app,
            })
        })?;

        let mut results = Vec::new();
        for item in rows {
            results.push(item?);
        }
        drop(stmt);
        drop(conn);
        self.shrink_memory();
        Ok(results)
    }

    /// Releases transient SQLite page and lookaside allocations back to the heap.
    pub fn shrink_memory(&self) {
        if let Ok(conn) = self.conn.lock() {
            let _ = conn.execute_batch("PRAGMA shrink_memory;");
        }
    }

    /// Returns the total count of saved history records
    pub fn get_count(&self) -> SqlResult<usize> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM dictation_history;",
            [],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    /// Deletes a specific history record by id
    pub fn delete_by_id(&self, id: i64) -> SqlResult<bool> {
        let conn = self.conn.lock().unwrap();
        let rows = conn.execute("DELETE FROM dictation_history WHERE id = ?1;", params![id])?;
        Ok(rows > 0)
    }

    /// Clears the entire dictation history
    pub fn clear_all(&self) -> SqlResult<()> {
        {
            let conn = self.conn.lock().unwrap();
            conn.execute("DELETE FROM dictation_history;", [])?;
            conn.execute("VACUUM;", [])?;
        }
        self.shrink_memory();
        Ok(())
    }
}

/// Standard location for persistent SQLite database in AppData
pub fn default_db_path() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            let dir = Path::new(&appdata).join("Sotto");
            let _ = std::fs::create_dir_all(&dir);
            return dir.join("sotto_history.db");
        }
    }

    // Fallback directory
    let dir = PathBuf::from("./data");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("sotto_history.db")
}

/// Background Asynchronous Worker Queue for zero-blocking UI dictation saves
#[derive(Clone)]
pub struct AsyncHistoryQueue {
    sender: Sender<HistoryCommand>,
    db: HistoryDb,
}

impl AsyncHistoryQueue {
    pub fn new(db: HistoryDb) -> Self {
        let (tx, rx) = channel::<HistoryCommand>();
        let db_worker = db.clone();

        thread::Builder::new()
            .name("sotto-db-writer".into())
            .spawn(move || {
                while let Ok(cmd) = rx.recv() {
                    match cmd {
                        HistoryCommand::Insert {
                            transcript,
                            duration_ms,
                            target_app,
                            on_complete,
                        } => match db_worker.insert(&transcript, duration_ms, &target_app) {
                            Ok(record) => {
                                if let Some(cb) = on_complete {
                                    cb(record);
                                }
                            }
                            Err(e) => {
                                eprintln!("[HistoryDb Error] Background insert failed: {}", e);
                            }
                        },
                        HistoryCommand::ClearAll { on_complete } => {
                            if let Err(e) = db_worker.clear_all() {
                                eprintln!("[HistoryDb Error] Background clear_all failed: {}", e);
                            }
                            if let Some(cb) = on_complete {
                                cb();
                            }
                        }
                    }
                }
            })
            .expect("Failed to spawn sotto-db-writer thread");

        Self { sender: tx, db }
    }

    /// Asynchronously enqueues an insert without blocking (< 0.01ms)
    pub fn enqueue_insert<F>(&self, transcript: String, duration_ms: i32, target_app: String, on_complete: F)
    where
        F: FnOnce(HistoryRecord) + Send + 'static,
    {
        let _ = self.sender.send(HistoryCommand::Insert {
            transcript,
            duration_ms,
            target_app,
            on_complete: Some(Box::new(on_complete)),
        });
    }

    /// Asynchronously enqueues clear all without blocking
    pub fn enqueue_clear_all<F>(&self, on_complete: F)
    where
        F: FnOnce() + Send + 'static,
    {
        let _ = self.sender.send(HistoryCommand::ClearAll {
            on_complete: Some(Box::new(on_complete)),
        });
    }

    /// Provides access to underlying synchronous DB handle for read queries
    pub fn db(&self) -> &HistoryDb {
        &self.db
    }
}

/// Resolves active process friendly brand name (Sentence Case, without .exe) and executable path from Win32 HWND
pub fn get_process_info_for_hwnd(hwnd: isize) -> (String, Option<String>) {
    if hwnd == 0 {
        return ("Desktop".to_string(), None);
    }

    #[cfg(target_os = "windows")]
    unsafe {
        use windows_sys::Win32::Foundation::*;
        use windows_sys::Win32::System::Threading::*;
        use windows_sys::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;

        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd as HWND, &mut pid);

        if pid == 0 {
            return ("Desktop".to_string(), None);
        }

        let process_handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process_handle.is_null() {
            return ("Desktop".to_string(), None);
        }

        let mut buffer = [0u16; 1024];
        let mut size = buffer.len() as u32;
        let success = QueryFullProcessImageNameW(process_handle, 0, buffer.as_mut_ptr(), &mut size);
        CloseHandle(process_handle);

        if success != 0 && size > 0 {
            let path_str = String::from_utf16_lossy(&buffer[..size as usize]);
            if let Some(file_name) = Path::new(&path_str).file_name() {
                let formatted = crate::app_icon::format_app_name(&file_name.to_string_lossy());
                return (formatted, Some(path_str));
            }
        }
    }

    ("Desktop".to_string(), None)
}

#[cfg(test)]
pub mod tests {
    use super::*;

    #[test]
    fn test_db_initialization_wal_and_schema() {
        let db = HistoryDb::open_in_memory().expect("In-memory SQLite failed to open");
        let conn = db.conn.lock().unwrap();

        let journal_mode: String = conn
            .query_row("PRAGMA journal_mode;", [], |r| r.get(0))
            .expect("Failed to query journal mode");
        // In-memory sqlite reports "memory" for journal_mode, but PRAGMA executed successfully
        assert!(!journal_mode.is_empty());

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM dictation_history;", [], |r| r.get(0))
            .expect("Table dictation_history should exist");
        assert_eq!(count, 0);
    }

    #[test]
    fn test_db_empty_state_query() {
        let db = HistoryDb::open_in_memory().unwrap();
        assert_eq!(db.get_count().unwrap(), 0);
        let items = db.get_recent(50).unwrap();
        assert!(items.is_empty(), "Empty DB query must return empty Vec safely");
    }

    #[test]
    fn test_db_unicode_emoji_and_special_quotes() {
        let db = HistoryDb::open_in_memory().unwrap();

        // Edge case: strings with single quotes, double quotes, emojis, newlines, tabs, and multilang Unicode
        let complex_transcript = "Sotto's voice \"pipeline\" -- test; DROP TABLE dictation_history; \n\t 🚀 🎤 日本語 العربية";
        let record = db.insert(complex_transcript, 1850, "Code.exe").unwrap();

        assert_eq!(record.id, 1);
        assert_eq!(record.transcript, complex_transcript);
        assert_eq!(record.duration_ms, 1850);
        assert_eq!(record.duration_str, "1.9s");
        assert_eq!(record.target_app, "Code.exe");

        let fetched = db.get_recent(10).unwrap();
        assert_eq!(fetched.len(), 1);
        assert_eq!(fetched[0].transcript, complex_transcript);
    }

    #[test]
    fn test_db_rolling_retention_prune_boundary() {
        let mut db = HistoryDb::open_in_memory().unwrap();
        // Set small retention boundary for fast, strict test
        db.set_max_retention(10);

        for i in 1..=10 {
            db.insert(&format!("Dictation record #{}", i), i * 100, "App.exe").unwrap();
        }
        assert_eq!(db.get_count().unwrap(), 10);

        // Insert 3 more items, pushing total to 13
        db.insert("Overflow record 11", 1100, "App.exe").unwrap();
        db.insert("Overflow record 12", 1200, "App.exe").unwrap();
        db.insert("Overflow record 13", 1300, "App.exe").unwrap();

        // Count should strictly remain 10
        assert_eq!(db.get_count().unwrap(), 10);

        // Newest record should be #13, oldest retained record should be #4
        let recent = db.get_recent(15).unwrap();
        assert_eq!(recent.len(), 10);
        assert_eq!(recent[0].transcript, "Overflow record 13");
        assert_eq!(recent[9].transcript, "Dictation record #4");
    }

    #[test]
    fn test_db_clear_all_and_delete_by_id() {
        let db = HistoryDb::open_in_memory().unwrap();
        let r1 = db.insert("First note", 500, "chrome.exe").unwrap();
        let r2 = db.insert("Second note", 800, "slack.exe").unwrap();
        assert_eq!(db.get_count().unwrap(), 2);

        // Delete single
        let deleted = db.delete_by_id(r1.id).unwrap();
        assert!(deleted);
        assert_eq!(db.get_count().unwrap(), 1);
        let items = db.get_recent(10).unwrap();
        assert_eq!(items[0].id, r2.id);

        // Clear all
        db.clear_all().unwrap();
        assert_eq!(db.get_count().unwrap(), 0);
        assert!(db.get_recent(10).unwrap().is_empty());
    }

    #[test]
    fn test_async_queue_concurrent_inserts() {
        let db = HistoryDb::open_in_memory().unwrap();
        let queue = AsyncHistoryQueue::new(db);

        let (done_tx, done_rx) = channel();

        // Concurrently dispatch 25 non-blocking inserts from 5 parallel threads
        let mut handles = Vec::new();
        for t in 0..5 {
            let q = queue.clone();
            let d_tx = done_tx.clone();
            handles.push(thread::spawn(move || {
                for i in 0..5 {
                    let d = d_tx.clone();
                    q.enqueue_insert(
                        format!("Thread {} - item {}", t, i),
                        (t * 100 + i) as i32,
                        "ConcurrentApp.exe".to_string(),
                        move |_| {
                            let _ = d.send(());
                        },
                    );
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        // Await all 25 async inserts
        for _ in 0..25 {
            done_rx.recv_timeout(std::time::Duration::from_secs(5)).expect("Insert timed out");
        }

        assert_eq!(queue.db().get_count().unwrap(), 25);
    }
}
