// Copyright (c) 2026 Sotto
// SQLite Provider Store: per-provider API keys, selected models & cached model lists.
// Keys live here (%APPDATA%\Sotto DB, local-only, never committed) instead of the
// JSON config so multiple providers can each hold their own credentials.

use rusqlite::{params, Result as SqlResult};

use super::history::HistoryDb;

/// Stored credentials + model choice for one transcription provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderKey {
    pub provider_id: String,
    pub api_key: String,
    pub selected_model: String,
    pub updated_at: i64,
}

/// Creates the provider tables. Called from HistoryDb setup; safe to run twice.
pub fn ensure_provider_tables(db: &HistoryDb) -> SqlResult<()> {
    let conn = db.connection().lock().unwrap();
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS provider_keys (
            provider_id TEXT PRIMARY KEY,
            api_key TEXT NOT NULL DEFAULT '',
            selected_model TEXT NOT NULL DEFAULT '',
            updated_at INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS provider_models (
            provider_id TEXT PRIMARY KEY,
            models_json TEXT NOT NULL DEFAULT '[]',
            fetched_at INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS provider_api_keys (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            provider_id TEXT NOT NULL,
            api_key TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            UNIQUE(provider_id, api_key)
        );
        INSERT OR IGNORE INTO provider_api_keys (provider_id, api_key, created_at)
        SELECT provider_id, api_key, updated_at FROM provider_keys WHERE api_key != '';
        "#,
    )?;
    Ok(())
}

impl HistoryDb {
    /// Inserts or replaces the stored key + model for a provider.
    pub fn upsert_provider_key(&self, provider_id: &str, api_key: &str, selected_model: &str) -> SqlResult<()> {
        let now = chrono::Utc::now().timestamp_millis();
        let conn = self.connection().lock().unwrap();
        conn.execute(
            "INSERT INTO provider_keys (provider_id, api_key, selected_model, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(provider_id) DO UPDATE SET
               api_key = excluded.api_key,
               selected_model = excluded.selected_model,
               updated_at = excluded.updated_at;",
            params![provider_id, api_key, selected_model, now],
        )?;
        Ok(())
    }

    /// Returns the stored credentials for a provider, if any.
    pub fn get_provider_key(&self, provider_id: &str) -> SqlResult<Option<ProviderKey>> {
        let conn = self.connection().lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT provider_id, api_key, selected_model, updated_at FROM provider_keys WHERE provider_id = ?1;",
        )?;
        let mut rows = stmt.query(params![provider_id])?;
        if let Some(row) = rows.next()? {
            Ok(Some(ProviderKey {
                provider_id: row.get(0)?,
                api_key: row.get(1)?,
                selected_model: row.get(2)?,
                updated_at: row.get(3)?,
            }))
        } else {
            Ok(None)
        }
    }

    /// Lists all stored provider credentials.
    pub fn list_provider_keys(&self) -> SqlResult<Vec<ProviderKey>> {
        let conn = self.connection().lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT provider_id, api_key, selected_model, updated_at FROM provider_keys ORDER BY provider_id;",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(ProviderKey {
                provider_id: row.get(0)?,
                api_key: row.get(1)?,
                selected_model: row.get(2)?,
                updated_at: row.get(3)?,
            })
        })?;
        rows.collect()
    }

    /// Adds an individual API key to the provider's key rotation pool.
    pub fn add_provider_api_key(&self, provider_id: &str, api_key: &str) -> SqlResult<()> {
        let trimmed = api_key.trim();
        if trimmed.is_empty() {
            return Ok(());
        }
        let now = chrono::Utc::now().timestamp_millis();
        let conn = self.connection().lock().unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO provider_api_keys (provider_id, api_key, created_at) VALUES (?1, ?2, ?3);",
            params![provider_id, trimmed, now],
        )?;
        // If provider_keys is empty or unset, set this as primary key
        conn.execute(
            "UPDATE provider_keys SET api_key = ?1 WHERE provider_id = ?2 AND (api_key IS NULL OR api_key = '');",
            params![trimmed, provider_id],
        )?;
        Ok(())
    }

    /// Removes an individual API key from the provider's key rotation pool.
    pub fn remove_provider_api_key(&self, provider_id: &str, api_key: &str) -> SqlResult<()> {
        let conn = self.connection().lock().unwrap();
        conn.execute(
            "DELETE FROM provider_api_keys WHERE provider_id = ?1 AND api_key = ?2;",
            params![provider_id, api_key.trim()],
        )?;
        // If the primary provider_keys entry held this key, rotate it to another key or empty
        let mut stmt = conn.prepare("SELECT api_key FROM provider_api_keys WHERE provider_id = ?1 LIMIT 1;")?;
        let next_key: Option<String> = stmt.query_row(params![provider_id], |row| row.get(0)).ok();
        let new_primary = next_key.unwrap_or_default();
        conn.execute(
            "UPDATE provider_keys SET api_key = ?1 WHERE provider_id = ?2;",
            params![new_primary, provider_id],
        )?;
        Ok(())
    }

    /// Lists all registered API keys for a provider (for auto-rotation pool).
    pub fn get_provider_api_keys(&self, provider_id: &str) -> SqlResult<Vec<String>> {
        let conn = self.connection().lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT api_key FROM provider_api_keys WHERE provider_id = ?1 ORDER BY id ASC;",
        )?;
        let rows = stmt.query_map(params![provider_id], |row| row.get::<usize, String>(0))?;
        let mut keys: Vec<String> = Vec::new();
        for r in rows {
            if let Ok(k) = r {
                let trimmed: String = k.trim().to_string();
                if !trimmed.is_empty() && !keys.contains(&trimmed) {
                    keys.push(trimmed);
                }
            }
        }
        Ok(keys)
    }

    /// Removes stored credentials for a provider.
    pub fn delete_provider_key(&self, provider_id: &str) -> SqlResult<()> {
        let conn = self.connection().lock().unwrap();
        conn.execute("DELETE FROM provider_keys WHERE provider_id = ?1;", params![provider_id])?;
        Ok(())
    }

    /// Caches a freshly fetched model list (JSON array of model id strings).
    pub fn cache_provider_models(&self, provider_id: &str, models_json: &str) -> SqlResult<()> {
        let now = chrono::Utc::now().timestamp_millis();
        let conn = self.connection().lock().unwrap();
        conn.execute(
            "INSERT INTO provider_models (provider_id, models_json, fetched_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(provider_id) DO UPDATE SET
               models_json = excluded.models_json,
               fetched_at = excluded.fetched_at;",
            params![provider_id, models_json, now],
        )?;
        Ok(())
    }

    /// Returns the cached model list + fetch timestamp, if any.
    pub fn get_cached_provider_models(&self, provider_id: &str) -> SqlResult<Option<(String, i64)>> {
        let conn = self.connection().lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT models_json, fetched_at FROM provider_models WHERE provider_id = ?1;",
        )?;
        let mut rows = stmt.query(params![provider_id])?;
        if let Some(row) = rows.next()? {
            Ok(Some((row.get(0)?, row.get(1)?)))
        } else {
            Ok(None)
        }
    }

    /// Cached model list for UI dropdowns, falling back to baked-in defaults
    /// when offline or never fetched.
    pub fn cached_model_list(&self, provider_id: &str, fallback: &[&str]) -> Vec<String> {
        if let Ok(Some((json, _))) = self.get_cached_provider_models(provider_id) {
            if let Ok(list) = serde_json::from_str::<Vec<String>>(&json) {
                if !list.is_empty() {
                    return list;
                }
            }
        }
        fallback.iter().map(|s| s.to_string()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem_db() -> HistoryDb {
        let db = HistoryDb::open_in_memory().expect("in-memory db");
        ensure_provider_tables(&db).expect("provider tables");
        db
    }

    #[test]
    fn test_provider_key_roundtrip() {
        let db = mem_db();
        assert!(db.get_provider_key("groq").expect("get").is_none());
        db.upsert_provider_key("groq", "gsk_test", "whisper-large-v3-turbo")
            .expect("upsert");
        let k = db.get_provider_key("groq").expect("get").expect("present");
        assert_eq!(k.api_key, "gsk_test");
        assert_eq!(k.selected_model, "whisper-large-v3-turbo");
        db.upsert_provider_key("groq", "gsk_new", "whisper-large-v3").expect("re-upsert");
        let k2 = db.get_provider_key("groq").expect("get").expect("present");
        assert_eq!(k2.api_key, "gsk_new");
        assert_eq!(k2.selected_model, "whisper-large-v3");
        db.delete_provider_key("groq").expect("delete");
        assert!(db.get_provider_key("groq").expect("get").is_none());
    }

    #[test]
    fn test_provider_models_cache_roundtrip() {
        let db = mem_db();
        assert!(db.get_cached_provider_models("gemini").expect("get").is_none());
        db.cache_provider_models("gemini", r#"["gemini-2.5-flash"]"#).expect("cache");
        let (json, _) = db.get_cached_provider_models("gemini").expect("get").expect("present");
        assert_eq!(json, r#"["gemini-2.5-flash"]"#);
    }

    #[test]
    fn test_list_provider_keys() {
        let db = mem_db();
        db.upsert_provider_key("groq", "a", "m1").expect("upsert");
        db.upsert_provider_key("gemini", "b", "m2").expect("upsert");
        let list = db.list_provider_keys().expect("list");
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn test_multi_key_pool_storage() {
        let db = mem_db();
        assert!(db.get_provider_api_keys("groq").expect("get").is_empty());
        db.add_provider_api_key("groq", "gsk_key_1").expect("add key 1");
        db.add_provider_api_key("groq", "gsk_key_2").expect("add key 2");
        // Duplicate key addition should be idempotent (INSERT OR IGNORE)
        db.add_provider_api_key("groq", "gsk_key_1").expect("add duplicate key 1");

        let keys = db.get_provider_api_keys("groq").expect("get keys");
        assert_eq!(keys.len(), 2);
        assert!(keys.contains(&"gsk_key_1".to_string()));
        assert!(keys.contains(&"gsk_key_2".to_string()));

        // Remove key 1
        db.remove_provider_api_key("groq", "gsk_key_1").expect("remove key 1");
        let remaining = db.get_provider_api_keys("groq").expect("get keys remaining");
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0], "gsk_key_2");
    }
}
