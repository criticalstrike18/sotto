// Copyright (c) 2026 Sotto
// SQLite Provider Store: per-provider encrypted API keys, selected models & cached model lists.
// Keys live here (%APPDATA%\Sotto DB, local-only, never committed) instead of the
// JSON config so multiple providers can each hold their own credentials encrypted at rest.

use std::collections::HashMap;

use rusqlite::{params, Result as SqlResult};

use super::history::HistoryDb;

const DPAPI_PREFIX: &str = "dpapi:v1:";

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

fn decode_hex(hex: &str) -> Option<Vec<u8>> {
    let bytes = hex.as_bytes();
    if bytes.len() % 2 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks_exact(2) {
        let hi = match pair[0] {
            b'0'..=b'9' => pair[0] - b'0',
            b'a'..=b'f' => pair[0] - b'a' + 10,
            b'A'..=b'F' => pair[0] - b'A' + 10,
            _ => return None,
        };
        let lo = match pair[1] {
            b'0'..=b'9' => pair[1] - b'0',
            b'a'..=b'f' => pair[1] - b'a' + 10,
            b'A'..=b'F' => pair[1] - b'A' + 10,
            _ => return None,
        };
        out.push((hi << 4) | lo);
    }
    Some(out)
}

#[cfg(target_os = "windows")]
mod crypto {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    /// Encrypts a plaintext API key using Windows DPAPI scoped to the current OS user.
    pub fn protect(plaintext: &str) -> Option<String> {
        let bytes = plaintext.as_bytes();
        let in_blob = CRYPT_INTEGER_BLOB {
            cbData: bytes.len() as u32,
            pbData: bytes.as_ptr() as *mut u8,
        };
        let mut out_blob = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        let ok = unsafe {
            CryptProtectData(
                &in_blob,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out_blob,
            )
        };
        if ok == 0 || out_blob.pbData.is_null() {
            return None;
        }
        let protected = unsafe {
            std::slice::from_raw_parts(out_blob.pbData, out_blob.cbData as usize)
        };
        let encoded = format!("{}{}", super::DPAPI_PREFIX, super::encode_hex(protected));
        unsafe {
            LocalFree(out_blob.pbData as _);
        }
        Some(encoded)
    }

    /// Decrypts a hex-encoded Windows DPAPI payload back to UTF-8 plaintext.
    pub fn unprotect(hex_payload: &str) -> Option<String> {
        let mut cipher = super::decode_hex(hex_payload)?;
        let in_blob = CRYPT_INTEGER_BLOB {
            cbData: cipher.len() as u32,
            pbData: cipher.as_mut_ptr(),
        };
        let mut out_blob = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        let ok = unsafe {
            CryptUnprotectData(
                &in_blob,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out_blob,
            )
        };
        if ok == 0 || out_blob.pbData.is_null() {
            return None;
        }
        let plain_bytes = unsafe {
            std::slice::from_raw_parts(out_blob.pbData, out_blob.cbData as usize).to_vec()
        };
        unsafe {
            LocalFree(out_blob.pbData as _);
        }
        String::from_utf8(plain_bytes).ok()
    }
}

/// Returns `true` when `stored` carries the OS-encrypted DPAPI envelope.
pub fn is_encrypted_key(stored: &str) -> bool {
    stored.starts_with(DPAPI_PREFIX)
}

fn encrypt_api_key(plaintext: &str) -> SqlResult<String> {
    if plaintext.is_empty() || is_encrypted_key(plaintext) {
        return Ok(plaintext.to_string());
    }
    #[cfg(target_os = "windows")]
    {
        crypto::protect(plaintext).ok_or_else(|| {
            rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::new(
                std::io::ErrorKind::Other,
                "Windows DPAPI CryptProtectData failed",
            )))
        })
    }
    #[cfg(not(target_os = "windows"))]
    {
        Ok(plaintext.to_string())
    }
}

fn try_decrypt_api_key(stored: &str) -> Option<String> {
    if stored.is_empty() {
        return Some(String::new());
    }
    if let Some(hex_payload) = stored.strip_prefix(DPAPI_PREFIX) {
        #[cfg(target_os = "windows")]
        {
            let plain = crypto::unprotect(hex_payload);
            if plain.is_none() {
                eprintln!("[Provider Store Warning] Failed to decrypt DPAPI-protected API key.");
            }
            return plain;
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = hex_payload;
            eprintln!("[Provider Store Warning] DPAPI-encrypted key cannot be decrypted on non-Windows.");
            return None;
        }
    }
    // Legacy plaintext row compatibility
    Some(stored.to_string())
}

fn decode_stored_key(stored: &str) -> String {
    try_decrypt_api_key(stored).unwrap_or_default()
}

/// Stored credentials + model choice for one transcription provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderKey {
    pub provider_id: String,
    pub api_key: String,
    pub selected_model: String,
    pub updated_at: i64,
}

/// Creates the provider tables and migrates legacy plaintext keys to OS-encrypted storage.
/// Called from HistoryDb setup; safe to run twice.
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
        "#,
    )?;

    // 1. Encrypt any legacy plaintext entries in `provider_keys` and collect rows for pool sync.
    let pk_rows: Vec<(String, String, i64)> = {
        let mut stmt = conn.prepare("SELECT provider_id, api_key, updated_at FROM provider_keys;")?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
        rows.collect::<SqlResult<Vec<_>>>()?
    };
    for (provider_id, raw_key, _) in &pk_rows {
        let trimmed = raw_key.trim();
        if !trimmed.is_empty() && !is_encrypted_key(trimmed) {
            let enc = encrypt_api_key(trimmed)?;
            if enc != *raw_key {
                conn.execute(
                    "UPDATE provider_keys SET api_key = ?1 WHERE provider_id = ?2;",
                    params![enc, provider_id],
                )?;
            }
        }
    }

    // 2. Encrypt any legacy plaintext entries in `provider_api_keys` and deduplicate plaintexts.
    let pool_rows: Vec<(i64, String, String)> = {
        let mut stmt = conn.prepare("SELECT id, provider_id, api_key FROM provider_api_keys ORDER BY id ASC;")?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
        rows.collect::<SqlResult<Vec<_>>>()?
    };
    let mut seen_by_provider: HashMap<String, Vec<String>> = HashMap::new();
    for (id, provider_id, raw_key) in pool_rows {
        let Some(plain) = try_decrypt_api_key(&raw_key) else {
            continue;
        };
        let plain_trimmed = plain.trim().to_string();
        let seen = seen_by_provider.entry(provider_id).or_default();
        if plain_trimmed.is_empty() || seen.contains(&plain_trimmed) {
            conn.execute("DELETE FROM provider_api_keys WHERE id = ?1;", params![id])?;
            continue;
        }
        seen.push(plain_trimmed.clone());
        if !is_encrypted_key(&raw_key) {
            let enc = encrypt_api_key(&plain_trimmed)?;
            if enc != raw_key {
                conn.execute(
                    "UPDATE provider_api_keys SET api_key = ?1 WHERE id = ?2;",
                    params![enc, id],
                )?;
            }
        }
    }

    // 3. Ensure every primary `provider_keys` key is represented (encrypted) in `provider_api_keys`.
    for (provider_id, raw_key, updated_at) in pk_rows {
        let Some(plain) = try_decrypt_api_key(&raw_key) else {
            continue;
        };
        let plain_trimmed = plain.trim().to_string();
        if plain_trimmed.is_empty() {
            continue;
        }
        let seen = seen_by_provider.entry(provider_id.clone()).or_default();
        if !seen.contains(&plain_trimmed) {
            let enc = encrypt_api_key(&plain_trimmed)?;
            conn.execute(
                "INSERT INTO provider_api_keys (provider_id, api_key, created_at) VALUES (?1, ?2, ?3);",
                params![provider_id, enc, updated_at],
            )?;
            seen.push(plain_trimmed);
        }
    }

    Ok(())
}

impl HistoryDb {
    /// Inserts or replaces the stored encrypted key + model for a provider.
    pub fn upsert_provider_key(&self, provider_id: &str, api_key: &str, selected_model: &str) -> SqlResult<()> {
        let now = chrono::Utc::now().timestamp_millis();
        let trimmed = api_key.trim();
        let encrypted = encrypt_api_key(trimmed)?;
        let conn = self.connection().lock().unwrap();
        conn.execute(
            "INSERT INTO provider_keys (provider_id, api_key, selected_model, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(provider_id) DO UPDATE SET
               api_key = excluded.api_key,
               selected_model = excluded.selected_model,
               updated_at = excluded.updated_at;",
            params![provider_id, encrypted, selected_model, now],
        )?;
        Ok(())
    }

    /// Updates ONLY the selected model for a provider without altering stored API keys.
    pub fn set_provider_model(&self, provider_id: &str, selected_model: &str) -> SqlResult<()> {
        let now = chrono::Utc::now().timestamp_millis();
        let conn = self.connection().lock().unwrap();
        conn.execute(
            "INSERT INTO provider_keys (provider_id, api_key, selected_model, updated_at)
             VALUES (?1, '', ?2, ?3)
             ON CONFLICT(provider_id) DO UPDATE SET
               selected_model = excluded.selected_model,
               updated_at = excluded.updated_at;",
            params![provider_id, selected_model, now],
        )?;
        Ok(())
    }

    /// Returns the stored credentials for a provider (decrypted), if any.
    pub fn get_provider_key(&self, provider_id: &str) -> SqlResult<Option<ProviderKey>> {
        let conn = self.connection().lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT provider_id, api_key, selected_model, updated_at FROM provider_keys WHERE provider_id = ?1;",
        )?;
        let mut rows = stmt.query(params![provider_id])?;
        if let Some(row) = rows.next()? {
            let stored: String = row.get(1)?;
            Ok(Some(ProviderKey {
                provider_id: row.get(0)?,
                api_key: decode_stored_key(&stored),
                selected_model: row.get(2)?,
                updated_at: row.get(3)?,
            }))
        } else {
            Ok(None)
        }
    }

    /// Lists all stored provider credentials (decrypted).
    pub fn list_provider_keys(&self) -> SqlResult<Vec<ProviderKey>> {
        let conn = self.connection().lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT provider_id, api_key, selected_model, updated_at FROM provider_keys ORDER BY provider_id;",
        )?;
        let rows = stmt.query_map([], |row| {
            let stored: String = row.get(1)?;
            Ok(ProviderKey {
                provider_id: row.get(0)?,
                api_key: decode_stored_key(&stored),
                selected_model: row.get(2)?,
                updated_at: row.get(3)?,
            })
        })?;
        rows.collect()
    }

    /// Adds an individual encrypted API key to the provider's key rotation pool,
    /// deduplicating by decrypted plaintext.
    pub fn add_provider_api_key(&self, provider_id: &str, api_key: &str) -> SqlResult<()> {
        let trimmed = api_key.trim();
        if trimmed.is_empty() {
            return Ok(());
        }
        let now = chrono::Utc::now().timestamp_millis();
        let conn = self.connection().lock().unwrap();

        // DPAPI ciphertexts are non-deterministic, so check duplicates by decrypted plaintext.
        {
            let mut stmt = conn.prepare(
                "SELECT api_key FROM provider_api_keys WHERE provider_id = ?1 ORDER BY id ASC;",
            )?;
            let rows = stmt.query_map(params![provider_id], |row| row.get::<usize, String>(0))?;
            for r in rows {
                if let Ok(raw) = r {
                    if let Some(existing) = try_decrypt_api_key(&raw) {
                        if existing.trim() == trimmed {
                            return Ok(());
                        }
                    }
                }
            }
        }

        let encrypted = encrypt_api_key(trimmed)?;
        conn.execute(
            "INSERT INTO provider_api_keys (provider_id, api_key, created_at) VALUES (?1, ?2, ?3);",
            params![provider_id, encrypted, now],
        )?;

        // Ensure `provider_keys` also records a primary encrypted key if missing or empty.
        let mut pk_stmt = conn.prepare("SELECT api_key FROM provider_keys WHERE provider_id = ?1;")?;
        let existing_primary: Option<String> = pk_stmt.query_row(params![provider_id], |row| row.get(0)).ok();
        match existing_primary {
            None => {
                conn.execute(
                    "INSERT INTO provider_keys (provider_id, api_key, selected_model, updated_at)
                     VALUES (?1, ?2, '', ?3);",
                    params![provider_id, encrypted, now],
                )?;
            }
            Some(ref cur) if decode_stored_key(cur).trim().is_empty() => {
                conn.execute(
                    "UPDATE provider_keys SET api_key = ?1, updated_at = ?2 WHERE provider_id = ?3;",
                    params![encrypted, now, provider_id],
                )?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Removes an individual API key from the provider's key rotation pool by matching
    /// decrypted plaintext, and rotates `provider_keys.api_key` if it held that key.
    pub fn remove_provider_api_key(&self, provider_id: &str, api_key: &str) -> SqlResult<()> {
        let target = api_key.trim();
        if target.is_empty() {
            return Ok(());
        }
        let conn = self.connection().lock().unwrap();
        let rows: Vec<(i64, String)> = {
            let mut stmt = conn.prepare(
                "SELECT id, api_key FROM provider_api_keys WHERE provider_id = ?1 ORDER BY id ASC;",
            )?;
            let mapped = stmt.query_map(params![provider_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
            mapped.collect::<SqlResult<Vec<_>>>()?
        };

        let mut remaining_first_stored: Option<String> = None;
        for (id, raw) in rows {
            let matches = try_decrypt_api_key(&raw)
                .map(|plain| plain.trim() == target)
                .unwrap_or(false);
            if matches {
                conn.execute("DELETE FROM provider_api_keys WHERE id = ?1;", params![id])?;
            } else if remaining_first_stored.is_none()
                && !decode_stored_key(&raw).trim().is_empty()
            {
                remaining_first_stored = Some(raw);
            }
        }

        // If the primary `provider_keys` entry held this key, rotate it to another key or empty.
        let mut pk_stmt = conn.prepare("SELECT api_key FROM provider_keys WHERE provider_id = ?1;")?;
        if let Ok(primary_raw) = pk_stmt.query_row(params![provider_id], |row| row.get::<usize, String>(0)) {
            let primary_plain = decode_stored_key(&primary_raw);
            if primary_plain.trim() == target || primary_plain.trim().is_empty() {
                let new_primary = remaining_first_stored.unwrap_or_default();
                conn.execute(
                    "UPDATE provider_keys SET api_key = ?1 WHERE provider_id = ?2;",
                    params![new_primary, provider_id],
                )?;
            }
        }
        Ok(())
    }

    /// Lists all registered API keys for a provider (decrypted, for auto-rotation pool).
    pub fn get_provider_api_keys(&self, provider_id: &str) -> SqlResult<Vec<String>> {
        let conn = self.connection().lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT api_key FROM provider_api_keys WHERE provider_id = ?1 ORDER BY id ASC;",
        )?;
        let rows = stmt.query_map(params![provider_id], |row| row.get::<usize, String>(0))?;
        let mut keys: Vec<String> = Vec::new();
        for r in rows {
            if let Ok(raw) = r {
                if let Some(plain) = try_decrypt_api_key(&raw) {
                    let trimmed = plain.trim().to_string();
                    if !trimmed.is_empty() && !keys.contains(&trimmed) {
                        keys.push(trimmed);
                    }
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
        // Duplicate key addition must be deduplicated even when ciphertext differs.
        db.add_provider_api_key("groq", "gsk_key_1").expect("add duplicate key 1");

        let keys = db.get_provider_api_keys("groq").expect("get keys");
        assert_eq!(keys, vec!["gsk_key_1".to_string(), "gsk_key_2".to_string()]);
        assert_eq!(
            db.get_provider_key("groq").expect("primary").expect("present").api_key,
            "gsk_key_1"
        );

        // Remove key 1 -> primary rotates to key 2.
        db.remove_provider_api_key("groq", "gsk_key_1").expect("remove key 1");
        let remaining = db.get_provider_api_keys("groq").expect("get keys remaining");
        assert_eq!(remaining, vec!["gsk_key_2".to_string()]);
        assert_eq!(
            db.get_provider_key("groq").expect("primary").expect("present").api_key,
            "gsk_key_2"
        );

        // Remove key 2 -> pool and primary are empty.
        db.remove_provider_api_key("groq", "gsk_key_2").expect("remove key 2");
        assert!(db.get_provider_api_keys("groq").expect("get keys").is_empty());
        assert_eq!(
            db.get_provider_key("groq").expect("primary").expect("present").api_key,
            ""
        );
    }

    #[test]
    fn test_set_provider_model_preserves_keys() {
        let db = mem_db();
        db.set_provider_model("groq", "whisper-large-v3").expect("set model before key");
        db.add_provider_api_key("groq", "gsk_keep_me").expect("add key");
        db.set_provider_model("groq", "distil-whisper-large-v3-en").expect("update model");

        let pk = db.get_provider_key("groq").expect("get").expect("present");
        assert_eq!(pk.api_key, "gsk_keep_me");
        assert_eq!(pk.selected_model, "distil-whisper-large-v3-en");
        assert_eq!(db.get_provider_api_keys("groq").expect("pool"), vec!["gsk_keep_me"]);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn test_keys_encrypted_at_rest_and_legacy_plaintext_migrates() {
        let db = mem_db();
        // Seed legacy plaintext rows directly in SQLite before running ensure_provider_tables.
        {
            let conn = db.connection().lock().unwrap();
            conn.execute(
                "INSERT INTO provider_keys (provider_id, api_key, selected_model, updated_at)
                 VALUES ('groq', 'gsk_legacy_plain', 'whisper-large-v3-turbo', 100);",
                [],
            )
            .expect("seed legacy provider_keys");
            conn.execute(
                "INSERT INTO provider_api_keys (provider_id, api_key, created_at)
                 VALUES ('gemini', 'AIza_legacy_plain', 200);",
                [],
            )
            .expect("seed legacy provider_api_keys");
        }

        ensure_provider_tables(&db).expect("migrate legacy keys");
        db.add_provider_api_key("groq", "gsk_new_secret").expect("add new key");

        // On Windows, raw columns in SQLite must hold DPAPI ciphertext, never plaintext.
        #[cfg(target_os = "windows")]
        {
            let conn = db.connection().lock().unwrap();
            let raw_pk: String = conn
                .query_row(
                    "SELECT api_key FROM provider_keys WHERE provider_id = 'groq';",
                    [],
                    |r| r.get(0),
                )
                .expect("raw pk");
            assert!(is_encrypted_key(&raw_pk));
            assert!(!raw_pk.contains("gsk_legacy_plain"));

            let mut stmt = conn
                .prepare("SELECT api_key FROM provider_api_keys ORDER BY id ASC;")
                .expect("prep");
            let raws: Vec<String> = stmt
                .query_map([], |r| r.get(0))
                .expect("map")
                .collect::<SqlResult<Vec<_>>>()
                .expect("collect");
            assert_eq!(raws.len(), 3);
            for raw in &raws {
                assert!(is_encrypted_key(raw), "expected dpapi prefix in row: {}", raw);
                assert!(!raw.contains("plain") && !raw.contains("secret"));
            }
        }

        // Decrypted APIs return original plaintexts intact, and migration is idempotent.
        ensure_provider_tables(&db).expect("re-run migration");
        assert_eq!(
            db.get_provider_key("groq").expect("get").expect("pk").api_key,
            "gsk_legacy_plain"
        );
        assert_eq!(
            db.get_provider_api_keys("groq").expect("groq pool"),
            vec!["gsk_legacy_plain".to_string(), "gsk_new_secret".to_string()]
        );
        assert_eq!(
            db.get_provider_api_keys("gemini").expect("gemini pool"),
            vec!["AIza_legacy_plain".to_string()]
        );
    }
}
