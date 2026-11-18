// Copyright (c) 2026 Sotto
// Thread-safe API Key Pool & Intelligent Auto-Rotation Router.
// Supports multi-key load balancing, instant 429 rate limit failover,
// and health tracking per provider key.

use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

/// Status snapshot of a single API key for UI and health monitoring.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyStatus {
    pub key: String,
    pub masked: String,
    pub is_rate_limited: bool,
    pub cooldown_remaining_secs: u64,
    pub total_requests: u64,
}

impl KeyStatus {
    pub fn status_description(&self) -> String {
        if self.is_rate_limited {
            format!("({}s)", self.cooldown_remaining_secs)
        } else {
            "Ready".to_string()
        }
    }
}

#[derive(Clone, Debug)]
struct KeyEntry {
    key: String,
    cooldown_until: Option<Instant>,
    total_requests: u64,
}

/// Masks an API key for safe UI rendering.
/// E.g., "gsk_1234567890abcdef" -> "gsk_...cdef"
pub fn mask_key(key: &str) -> String {
    let trimmed = key.trim();
    if trimmed.is_empty() {
        return "".to_string();
    }
    if trimmed.len() <= 8 {
        return "***".to_string();
    }
    let prefix_len = if trimmed.starts_with("gsk_") {
        4
    } else if trimmed.starts_with("AIza") {
        4
    } else {
        3
    };
    let suffix_len = 4;
    let prefix = &trimmed[..prefix_len];
    let suffix = &trimmed[trimmed.len() - suffix_len..];
    format!("{}...{}", prefix, suffix)
}

/// Thread-safe multi-key pool with automated load-balancing and 429 rotation.
#[derive(Clone, Debug)]
pub struct KeyPool {
    entries: Arc<RwLock<Vec<KeyEntry>>>,
}

impl Default for KeyPool {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

impl KeyPool {
    /// Creates a new KeyPool from an initial list of keys.
    pub fn new(keys: Vec<String>) -> Self {
        let mut entries = Vec::new();
        for k in keys {
            let trimmed = k.trim().to_string();
            if !trimmed.is_empty() && !entries.iter().any(|e: &KeyEntry| e.key == trimmed) {
                entries.push(KeyEntry {
                    key: trimmed,
                    cooldown_until: None,
                    total_requests: 0,
                });
            }
        }
        Self {
            entries: Arc::new(RwLock::new(entries)),
        }
    }

    /// Adds a key to the pool if it does not already exist.
    pub fn add_key(&self, key: &str) -> bool {
        let trimmed = key.trim().to_string();
        if trimmed.is_empty() {
            return false;
        }
        let mut lock = self.entries.write().unwrap();
        if !lock.iter().any(|e| e.key == trimmed) {
            lock.push(KeyEntry {
                key: trimmed,
                cooldown_until: None,
                total_requests: 0,
            });
            true
        } else {
            false
        }
    }

    /// Removes a key from the pool.
    pub fn remove_key(&self, key: &str) -> bool {
        let trimmed = key.trim();
        let mut lock = self.entries.write().unwrap();
        let initial_len = lock.len();
        lock.retain(|e| e.key != trimmed);
        lock.len() < initial_len
    }

    /// Replaces all keys in the pool while preserving request counts and cooldowns for existing keys.
    pub fn set_keys(&self, keys: Vec<String>) {
        let mut new_entries = Vec::new();
        let old_lock = self.entries.read().unwrap();
        for k in keys {
            let trimmed = k.trim().to_string();
            if trimmed.is_empty() {
                continue;
            }
            if new_entries.iter().any(|e: &KeyEntry| e.key == trimmed) {
                continue;
            }
            if let Some(old) = old_lock.iter().find(|e| e.key == trimmed) {
                new_entries.push(old.clone());
            } else {
                new_entries.push(KeyEntry {
                    key: trimmed,
                    cooldown_until: None,
                    total_requests: 0,
                });
            }
        }
        drop(old_lock);
        *self.entries.write().unwrap() = new_entries;
    }

    /// Returns the number of configured keys.
    pub fn len(&self) -> usize {
        self.entries.read().unwrap().len()
    }

    /// Checks if the pool is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.read().unwrap().is_empty()
    }

    /// Returns the primary (first configured) key, if any.
    pub fn primary_key(&self) -> Option<String> {
        self.entries.read().unwrap().first().map(|e| e.key.clone())
    }

    /// Selects the optimal key for the next request.
    /// Prefers keys not on cooldown, picking the least utilized.
    /// If all keys are in cooldown, picks the key whose cooldown expires soonest.
    pub fn get_next_key(&self) -> Option<String> {
        let now = Instant::now();
        let lock = self.entries.read().unwrap();
        if lock.is_empty() {
            return None;
        }

        // 1. Ready keys (cooldown expired or none)
        let ready_key = lock
            .iter()
            .filter(|e| e.cooldown_until.map_or(true, |t| now >= t))
            .min_by_key(|e| e.total_requests);

        if let Some(entry) = ready_key {
            return Some(entry.key.clone());
        }

        // 2. All keys on cooldown -> select the one expiring soonest
        let soonest = lock.iter().min_by_key(|e| e.cooldown_until.unwrap_or(now));
        soonest.map(|e| e.key.clone())
    }

    /// Marks a key as rate-limited (HTTP 429), putting it on temporary cooldown.
    pub fn mark_rate_limited(&self, key: &str, cooldown: Duration) {
        let mut lock = self.entries.write().unwrap();
        if let Some(entry) = lock.iter_mut().find(|e| e.key == key) {
            entry.cooldown_until = Some(Instant::now() + cooldown);
        }
    }

    /// Marks a successful request, incrementing usage and clearing cooldown.
    pub fn mark_success(&self, key: &str) {
        let mut lock = self.entries.write().unwrap();
        if let Some(entry) = lock.iter_mut().find(|e| e.key == key) {
            entry.total_requests = entry.total_requests.saturating_add(1);
            entry.cooldown_until = None;
        }
    }

    /// Generates snapshots of all keys with masked display and cooldown status for UI rendering.
    pub fn get_statuses(&self) -> Vec<KeyStatus> {
        let now = Instant::now();
        let lock = self.entries.read().unwrap();
        lock.iter()
            .map(|e| {
                let (is_rate_limited, cooldown_remaining_secs) = match e.cooldown_until {
                    Some(until) if until > now => (true, (until - now).as_secs() + 1),
                    _ => (false, 0),
                };
                KeyStatus {
                    key: e.key.clone(),
                    masked: mask_key(&e.key),
                    is_rate_limited,
                    cooldown_remaining_secs,
                    total_requests: e.total_requests,
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mask_key() {
        assert_eq!(mask_key("gsk_1234567890abcdef"), "gsk_...cdef");
        assert_eq!(mask_key("AIzaSyD1234567890XYZ"), "AIza...0XYZ");
        assert_eq!(mask_key("short"), "***");
        assert_eq!(mask_key(""), "");
    }

    #[test]
    fn test_key_pool_rotation_and_cooldown() {
        let pool = KeyPool::new(vec!["key_1".into(), "key_2".into()]);
        assert_eq!(pool.len(), 2);

        // First pick: key_1
        let k1 = pool.get_next_key().unwrap();
        assert_eq!(k1, "key_1");
        pool.mark_success("key_1");

        // Second pick should be key_2 (least requests)
        let k2 = pool.get_next_key().unwrap();
        assert_eq!(k2, "key_2");
        pool.mark_success("key_2");

        // Mark key_1 rate limited
        pool.mark_rate_limited("key_1", Duration::from_secs(60));
        // Next pick MUST be key_2 even though requests are equal
        assert_eq!(pool.get_next_key().unwrap(), "key_2");

        // Verify status
        let statuses = pool.get_statuses();
        assert_eq!(statuses.len(), 2);
        assert!(statuses.iter().find(|s| s.key == "key_1").unwrap().is_rate_limited);
        assert!(!statuses.iter().find(|s| s.key == "key_2").unwrap().is_rate_limited);
    }
}
