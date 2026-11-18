// Copyright (c) 2026 Sotto
// Database module for persistence and history

pub mod history;
pub mod providers;

pub use history::{
    default_db_path, get_process_info_for_hwnd, AsyncHistoryQueue, HistoryDb,
    HistoryRecord, DEFAULT_MAX_RETENTION_ITEMS,
};
pub use providers::ProviderKey;
