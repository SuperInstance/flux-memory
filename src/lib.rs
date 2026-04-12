use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

fn now_epoch() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

#[derive(Clone, Debug)]
pub struct MemEntry {
    pub key: String,
    pub value: String,
    pub version: u32,
    pub read_only: bool,
}

#[derive(Clone, Debug)]
struct InternalEntry {
    entry: MemEntry,
    created_at: u64,
    ttl_secs: u64,
}

impl InternalEntry {
    fn is_expired(&self) -> bool {
        if self.ttl_secs == 0 {
            return false;
        }
        now_epoch() > self.created_at + self.ttl_secs
    }
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub entries: Vec<(String, MemEntry)>,
    pub label: String,
}

pub struct Store {
    entries: HashMap<String, InternalEntry>,
}

impl Store {
    pub fn new() -> Self {
        Store {
            entries: HashMap::new(),
        }
    }

    pub fn put(&mut self, key: &str, value: &str, ttl_secs: u64, read_only: bool) {
        let existing = self.entries.get(key).map(|ie| ie.entry.version).unwrap_or(0);
        let entry = MemEntry {
            key: key.to_string(),
            value: value.to_string(),
            version: existing + 1,
            read_only,
        };
        self.entries.insert(
            key.to_string(),
            InternalEntry {
                entry,
                created_at: now_epoch(),
                ttl_secs,
            },
        );
    }

    pub fn get(&self, key: &str) -> Option<String> {
        self.entries.get(key).and_then(|ie| {
            if ie.is_expired() {
                None
            } else {
                Some(ie.entry.value.clone())
            }
        })
    }

    pub fn delete(&mut self, key: &str) -> bool {
        self.entries.remove(key).is_some()
    }

    pub fn exists(&self, key: &str) -> bool {
        self.entries.get(key).map_or(false, |ie| !ie.is_expired())
    }

    pub fn update(&mut self, key: &str, value: &str) -> bool {
        let ie = match self.entries.get_mut(key) {
            Some(ie) if !ie.is_expired() => ie,
            _ => return false,
        };
        if ie.entry.read_only {
            return false;
        }
        ie.entry.value = value.to_string();
        ie.entry.version += 1;
        true
    }

    pub fn search(&self, prefix: &str) -> Vec<&MemEntry> {
        self.entries
            .iter()
            .filter(|(k, ie)| k.starts_with(prefix) && !ie.is_expired())
            .map(|(_, ie)| &ie.entry)
            .collect()
    }

    pub fn count(&self) -> usize {
        self.entries.values().filter(|ie| !ie.is_expired()).count()
    }

    pub fn gc(&mut self) -> usize {
        let before = self.entries.len();
        self.entries.retain(|_, ie| !ie.is_expired());
        before - self.entries.len()
    }

    pub fn snapshot(&self, label: &str) -> Snapshot {
        let entries: Vec<(String, MemEntry)> = self
            .entries
            .iter()
            .filter(|(_, ie)| !ie.is_expired())
            .map(|(k, ie)| (k.clone(), ie.entry.clone()))
            .collect();
        Snapshot {
            entries,
            label: label.to_string(),
        }
    }

    pub fn restore(&mut self, snap: &Snapshot) {
        self.entries.clear();
        for (k, entry) in &snap.entries {
            self.entries.insert(
                k.clone(),
                InternalEntry {
                    entry: entry.clone(),
                    created_at: now_epoch(),
                    ttl_secs: 0,
                },
            );
        }
    }

    pub fn diff(&self, snap: &Snapshot) -> (Vec<String>, Vec<String>) {
        let snap_keys: std::collections::HashSet<&str> =
            snap.entries.iter().map(|(k, _)| k.as_str()).collect();
        let cur_keys: std::collections::HashSet<&str> = self
            .entries
            .iter()
            .filter(|(_, ie)| !ie.is_expired())
            .map(|(k, _)| k.as_str())
            .collect();

        let added: Vec<String> = cur_keys
            .difference(&snap_keys)
            .map(|s| s.to_string())
            .collect();
        let removed: Vec<String> = snap_keys
            .difference(&cur_keys)
            .map(|s| s.to_string())
            .collect();
        (added, removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_put_and_get() {
        let mut s = Store::new();
        s.put("a", "1", 0, false);
        assert_eq!(s.get("a"), Some("1".to_string()));
    }

    #[test]
    fn test_get_missing() {
        let s = Store::new();
        assert_eq!(s.get("nope"), None);
    }

    #[test]
    fn test_exists() {
        let mut s = Store::new();
        s.put("x", "v", 0, false);
        assert!(s.exists("x"));
        assert!(!s.exists("y"));
    }

    #[test]
    fn test_delete() {
        let mut s = Store::new();
        s.put("a", "1", 0, false);
        assert!(s.delete("a"));
        assert!(!s.delete("a"));
        assert_eq!(s.get("a"), None);
    }

    #[test]
    fn test_update() {
        let mut s = Store::new();
        s.put("a", "1", 0, false);
        assert!(s.update("a", "2"));
        assert_eq!(s.get("a"), Some("2".to_string()));
        assert_eq!(s.entries.get("a").unwrap().entry.version, 2);
    }

    #[test]
    fn test_update_readonly_fails() {
        let mut s = Store::new();
        s.put("a", "1", 0, true);
        assert!(!s.update("a", "2"));
        assert_eq!(s.get("a"), Some("1".to_string()));
    }

    #[test]
    fn test_update_missing() {
        let mut s = Store::new();
        assert!(!s.update("a", "1"));
    }

    #[test]
    fn test_version_increments_on_put() {
        let mut s = Store::new();
        s.put("a", "1", 0, false);
        assert_eq!(s.entries.get("a").unwrap().entry.version, 1);
        s.put("a", "2", 0, false);
        assert_eq!(s.entries.get("a").unwrap().entry.version, 2);
    }

    #[test]
    fn test_search_prefix() {
        let mut s = Store::new();
        s.put("user:1", "alice", 0, false);
        s.put("user:2", "bob", 0, false);
        s.put("post:1", "hello", 0, false);
        let results = s.search("user:");
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_search_empty() {
        let mut s = Store::new();
        s.put("a", "1", 0, false);
        assert!(s.search("z").is_empty());
    }

    #[test]
    fn test_count() {
        let mut s = Store::new();
        assert_eq!(s.count(), 0);
        s.put("a", "1", 0, false);
        s.put("b", "2", 0, false);
        assert_eq!(s.count(), 2);
        s.delete("a");
        assert_eq!(s.count(), 1);
    }

    #[test]
    fn test_ttl_expired() {
        let mut s = Store::new();
        s.put("temp", "data", 1, false);
        thread::sleep(Duration::from_secs(2));
        assert_eq!(s.get("temp"), None);
        assert!(!s.exists("temp"));
    }

    #[test]
    fn test_ttl_still_alive() {
        let mut s = Store::new();
        s.put("temp", "data", 10, false);
        assert_eq!(s.get("temp"), Some("data".to_string()));
    }

    #[test]
    fn test_ttl_zero_never_expires() {
        let mut s = Store::new();
        s.put("perm", "val", 0, false);
        thread::sleep(Duration::from_millis(100));
        assert!(s.exists("perm"));
    }

    #[test]
    fn test_gc() {
        let mut s = Store::new();
        s.put("short", "1", 1, false);
        s.put("long", "2", 60, false);
        thread::sleep(Duration::from_secs(2));
        let removed = s.gc();
        assert_eq!(removed, 1);
        assert_eq!(s.count(), 1);
        assert_eq!(s.get("long"), Some("2".to_string()));
    }

    #[test]
    fn test_snapshot_and_restore() {
        let mut s = Store::new();
        s.put("a", "1", 0, false);
        s.put("b", "2", 0, false);
        let snap = s.snapshot("v1");
        s.put("c", "3", 0, false);
        s.delete("a");
        assert_eq!(s.count(), 2);
        s.restore(&snap);
        assert_eq!(s.count(), 2);
        assert_eq!(s.get("a"), Some("1".to_string()));
        assert_eq!(s.get("b"), Some("2".to_string()));
        assert_eq!(snap.label, "v1");
    }

    #[test]
    fn test_diff_added_removed() {
        let mut s = Store::new();
        s.put("a", "1", 0, false);
        s.put("b", "2", 0, false);
        let snap = s.snapshot("s1");
        s.delete("a");
        s.put("c", "3", 0, false);
        let (added, removed) = s.diff(&snap);
        assert!(added.contains(&"c".to_string()));
        assert!(removed.contains(&"a".to_string()));
    }
}
