//! Frecency: things you launch often and recently float to the top.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Default, Serialize, Deserialize, Clone, Copy)]
struct Entry {
    count: u32,
    last: u64,
}

#[derive(Default)]
pub struct History {
    entries: HashMap<String, Entry>,
    now: u64,
    path: Option<std::path::PathBuf>,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl History {
    pub fn load(namespace: &str) -> History {
        let path = crate::config::state_dir().join(format!("{namespace}.json"));
        let entries = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        History { entries, now: now(), path: Some(path) }
    }

    pub fn disabled() -> History {
        History::default()
    }

    /// Bonus in "match score" units. 0 for unknown items.
    pub fn score(&self, key: &str) -> f32 {
        let Some(e) = self.entries.get(key) else { return 0.0 };
        let age_h = self.now.saturating_sub(e.last) as f32 / 3600.0;
        let recency = if age_h < 4.0 {
            4.0
        } else if age_h < 24.0 {
            2.5
        } else if age_h < 24.0 * 7.0 {
            1.5
        } else if age_h < 24.0 * 30.0 {
            1.0
        } else {
            0.5
        };
        (1.0 + e.count as f32).ln() * recency * 12.0
    }

    pub fn bump(&mut self, key: &str) {
        let Some(path) = &self.path else { return };
        let e = self.entries.entry(key.to_string()).or_default();
        e.count = e.count.saturating_add(1);
        e.last = self.now;
        // Prune ancient, rarely used entries so the file stays tiny.
        if self.entries.len() > 2000 {
            let cutoff = self.now.saturating_sub(90 * 24 * 3600);
            self.entries.retain(|_, e| e.last > cutoff || e.count > 5);
        }
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(s) = serde_json::to_vec(&self.entries) {
            let tmp = path.with_extension("json.tmp");
            if std::fs::write(&tmp, s).is_ok() {
                let _ = std::fs::rename(tmp, path);
            }
        }
    }
}
