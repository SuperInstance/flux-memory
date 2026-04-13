use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
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
        let existing = self
            .entries
            .get(key)
            .map(|ie| ie.entry.version)
            .unwrap_or(0);
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

    // ── Put / Get edge cases ──────────────────────────────────────────

    #[test]
    fn test_put_empty_key() {
        let mut s = Store::new();
        s.put("", "val", 0, false);
        assert_eq!(s.get(""), Some("val".to_string()));
        assert!(s.exists(""));
        assert_eq!(s.count(), 1);
    }

    #[test]
    fn test_put_empty_value() {
        let mut s = Store::new();
        s.put("key", "", 0, false);
        assert_eq!(s.get("key"), Some("".to_string()));
    }

    #[test]
    fn test_put_overwrites_existing_value() {
        let mut s = Store::new();
        s.put("k", "old", 0, false);
        s.put("k", "new", 0, false);
        assert_eq!(s.get("k"), Some("new".to_string()));
        assert_eq!(s.count(), 1); // still only one entry
    }

    #[test]
    fn test_put_flips_read_only_flag() {
        let mut s = Store::new();
        s.put("k", "v1", 0, true);  // read_only
        assert!(!s.update("k", "v2"));
        s.put("k", "v3", 0, false); // now writable
        assert!(s.update("k", "v4"));
        assert_eq!(s.get("k"), Some("v4".to_string()));
    }

    #[test]
    fn test_put_resets_ttl() {
        let mut s = Store::new();
        s.put("k", "v1", 2, false);
        // Re-put before expiry to refresh TTL
        std::thread::sleep(std::time::Duration::from_secs(1));
        s.put("k", "v2", 5, false);
        std::thread::sleep(std::time::Duration::from_secs(2));
        // Original TTL would have expired at ~2s; refreshed TTL keeps it alive at 5s
        assert_eq!(s.get("k"), Some("v2".to_string()));
    }

    // ── Delete edge cases ─────────────────────────────────────────────

    #[test]
    fn test_delete_nonexistent_returns_false() {
        let mut s = Store::new();
        assert!(!s.delete("ghost"));
    }

    #[test]
    fn test_delete_then_count_zero() {
        let mut s = Store::new();
        s.put("a", "1", 0, false);
        s.delete("a");
        assert_eq!(s.count(), 0);
        assert_eq!(s.get("a"), None);
    }

    #[test]
    fn test_exists_after_delete() {
        let mut s = Store::new();
        s.put("a", "1", 0, false);
        s.delete("a");
        assert!(!s.exists("a"));
    }

    // ── Update edge cases ─────────────────────────────────────────────

    #[test]
    fn test_update_increments_version() {
        let mut s = Store::new();
        s.put("k", "v1", 0, false);
        assert_eq!(s.entries.get("k").unwrap().entry.version, 1);
        s.update("k", "v2");
        assert_eq!(s.entries.get("k").unwrap().entry.version, 2);
        s.update("k", "v3");
        assert_eq!(s.entries.get("k").unwrap().entry.version, 3);
    }

    #[test]
    fn test_update_expired_entry_fails() {
        let mut s = Store::new();
        s.put("k", "v", 1, false);
        std::thread::sleep(std::time::Duration::from_secs(2));
        assert!(!s.update("k", "new"));
    }

    // ── GC tests ──────────────────────────────────────────────────────

    #[test]
    fn test_gc_empty_store() {
        let mut s = Store::new();
        assert_eq!(s.gc(), 0);
    }

    #[test]
    fn test_gc_no_expired_entries() {
        let mut s = Store::new();
        s.put("a", "1", 0, false);
        s.put("b", "2", 60, false);
        assert_eq!(s.gc(), 0);
        assert_eq!(s.count(), 2);
    }

    #[test]
    fn test_gc_multiple_expired() {
        let mut s = Store::new();
        s.put("short1", "a", 1, false);
        s.put("short2", "b", 1, false);
        s.put("short3", "c", 1, false);
        s.put("long1", "d", 60, false);
        std::thread::sleep(std::time::Duration::from_secs(2));
        let removed = s.gc();
        assert_eq!(removed, 3);
        assert_eq!(s.count(), 1);
        assert_eq!(s.get("long1"), Some("d".to_string()));
    }

    #[test]
    fn test_gc_then_put_works() {
        let mut s = Store::new();
        s.put("tmp", "x", 1, false);
        std::thread::sleep(std::time::Duration::from_secs(2));
        s.gc();
        s.put("fresh", "y", 0, false);
        assert_eq!(s.get("fresh"), Some("y".to_string()));
        assert_eq!(s.count(), 1);
    }

    // ── Search tests ──────────────────────────────────────────────────

    #[test]
    fn test_search_empty_prefix_matches_all() {
        let mut s = Store::new();
        s.put("a", "1", 0, false);
        s.put("b", "2", 0, false);
        s.put("c", "3", 0, false);
        let results = s.search("");
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn test_search_excludes_expired() {
        let mut s = Store::new();
        s.put("user:1", "alice", 1, false);
        s.put("user:2", "bob", 60, false);
        std::thread::sleep(std::time::Duration::from_secs(2));
        let results = s.search("user:");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].key, "user:2");
    }

    #[test]
    fn test_search_returns_entry_refs() {
        let mut s = Store::new();
        s.put("x:y", "val", 0, true);
        let results = s.search("x:");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].value, "val");
        assert!(results[0].read_only);
        assert_eq!(results[0].version, 1);
    }

    #[test]
    fn test_search_on_empty_store() {
        let s = Store::new();
        assert!(s.search("anything").is_empty());
    }

    // ── Snapshot / Restore / Diff tests ───────────────────────────────

    #[test]
    fn test_snapshot_empty_store() {
        let s = Store::new();
        let snap = s.snapshot("empty");
        assert!(snap.entries.is_empty());
        assert_eq!(snap.label, "empty");
    }

    #[test]
    fn test_snapshot_excludes_expired() {
        let mut s = Store::new();
        s.put("alive", "yes", 60, false);
        s.put("dead", "no", 1, false);
        std::thread::sleep(std::time::Duration::from_secs(2));
        let snap = s.snapshot("after-expiry");
        assert_eq!(snap.entries.len(), 1);
        assert_eq!(snap.entries[0].0, "alive");
    }

    #[test]
    fn test_restore_from_empty_snapshot() {
        let mut s = Store::new();
        s.put("a", "1", 0, false);
        s.put("b", "2", 0, false);
        let empty_snap = Snapshot {
            entries: vec![],
            label: "empty".to_string(),
        };
        s.restore(&empty_snap);
        assert_eq!(s.count(), 0);
    }

    #[test]
    fn test_restore_preserves_entry_metadata() {
        let mut s = Store::new();
        s.put("k", "v", 0, true);
        s.update("k", "v2"); // should fail because read_only
        let snap = s.snapshot("meta-check");
        // Create fresh store and restore
        let mut s2 = Store::new();
        s2.restore(&snap);
        let entry = &s2.entries.get("k").unwrap().entry;
        assert_eq!(entry.key, "k");
        assert_eq!(entry.value, "v");
        assert_eq!(entry.version, 1);
        assert!(entry.read_only);
    }

    #[test]
    fn test_restore_resets_ttl_to_zero() {
        let mut s = Store::new();
        s.put("k", "v", 1, false);
        let snap = s.snapshot("ttl-reset");
        std::thread::sleep(std::time::Duration::from_secs(2));
        let mut s2 = Store::new();
        s2.restore(&snap);
        // Restored entry should have ttl=0, so it should not be expired
        assert_eq!(s2.get("k"), Some("v".to_string()));
    }

    #[test]
    fn test_diff_no_changes() {
        let mut s = Store::new();
        s.put("a", "1", 0, false);
        let snap = s.snapshot("no-change");
        let (added, removed) = s.diff(&snap);
        assert!(added.is_empty());
        assert!(removed.is_empty());
    }

    #[test]
    fn test_diff_empty_store_against_nonempty_snapshot() {
        let mut s = Store::new();
        s.put("a", "1", 0, false);
        s.put("b", "2", 0, false);
        let snap = s.snapshot("full");
        s.delete("a");
        s.delete("b");
        let (added, removed) = s.diff(&snap);
        assert!(added.is_empty());
        assert_eq!(removed.len(), 2);
    }

    // ── Version tracking ──────────────────────────────────────────────

    #[test]
    fn test_version_resets_on_full_removal_and_reput() {
        let mut s = Store::new();
        s.put("k", "v1", 0, false);
        s.put("k", "v2", 0, false);
        s.put("k", "v3", 0, false);
        assert_eq!(s.entries.get("k").unwrap().entry.version, 3);
        s.delete("k");
        s.put("k", "v4", 0, false);
        // After delete, version starts fresh from 1
        assert_eq!(s.entries.get("k").unwrap().entry.version, 1);
    }

    #[test]
    fn test_version_put_then_update() {
        let mut s = Store::new();
        s.put("k", "v1", 0, false); // version 1
        s.update("k", "v2");          // version 2
        s.put("k", "v3", 0, false);  // version 3 (put sees version 2, increments to 3)
        assert_eq!(s.entries.get("k").unwrap().entry.version, 3);
        assert_eq!(s.get("k"), Some("v3".to_string()));
    }

    // ── Stress / bulk operations ─────────────────────────────────────

    #[test]
    fn test_large_batch_insert() {
        let mut s = Store::new();
        for i in 0..1000 {
            s.put(&format!("key:{}", i), &format!("val:{}", i), 0, false);
        }
        assert_eq!(s.count(), 1000);
        assert_eq!(s.get("key:500"), Some("val:500".to_string()));
        assert_eq!(s.get("key:999"), Some("val:999".to_string()));
        assert_eq!(s.search("key:").len(), 1000);
    }

    #[test]
    fn test_large_batch_insert_and_delete() {
        let mut s = Store::new();
        for i in 0..500 {
            s.put(&format!("item:{}", i), "data", 0, false);
        }
        for i in 0..500 {
            assert!(s.delete(&format!("item:{}", i)));
        }
        assert_eq!(s.count(), 0);
        assert_eq!(s.search("item:").len(), 0);
    }

    // ── Clone / Debug trait verification ──────────────────────────────

    #[test]
    fn test_mem_entry_clone_and_debug() {
        let entry = MemEntry {
            key: "k".to_string(),
            value: "v".to_string(),
            version: 1,
            read_only: false,
        };
        let cloned = entry.clone();
        assert_eq!(entry.key, cloned.key);
        assert_eq!(entry.value, cloned.value);
        let debug_str = format!("{:?}", entry);
        assert!(debug_str.contains("k"));
        assert!(debug_str.contains("v"));
    }

    #[test]
    fn test_snapshot_clone() {
        let mut s = Store::new();
        s.put("a", "1", 0, false);
        let snap = s.snapshot("clone-test");
        let snap2 = snap.clone();
        assert_eq!(snap.entries.len(), snap2.entries.len());
        assert_eq!(snap.label, snap2.label);
    }

    #[test]
    fn test_snapshot_debug() {
        let mut s = Store::new();
        s.put("x", "y", 0, false);
        let snap = s.snapshot("debug");
        let debug_str = format!("{:?}", snap);
        assert!(debug_str.contains("debug"));
    }

    // ── Count accuracy ────────────────────────────────────────────────

    #[test]
    fn test_count_excludes_expired_without_gc() {
        let mut s = Store::new();
        s.put("alive", "yes", 0, false);
        s.put("dead", "no", 1, false);
        std::thread::sleep(std::time::Duration::from_secs(2));
        // count() should exclude expired even without gc()
        assert_eq!(s.count(), 1);
        // But raw entries len still includes the expired one
        assert_eq!(s.entries.len(), 2);
    }

    #[test]
    fn test_count_after_restore() {
        let mut s = Store::new();
        s.put("a", "1", 0, false);
        s.put("b", "2", 0, false);
        s.put("c", "3", 0, false);
        let snap = s.snapshot("pre-delete");
        s.delete("b");
        assert_eq!(s.count(), 2);
        s.restore(&snap);
        assert_eq!(s.count(), 3);
    }

    // ── Exists accuracy ───────────────────────────────────────────────

    #[test]
    fn test_exists_excludes_expired() {
        let mut s = Store::new();
        s.put("temp", "x", 1, false);
        std::thread::sleep(std::time::Duration::from_secs(2));
        assert!(!s.exists("temp"));
    }

    #[test]
    fn test_exists_on_new_store() {
        let s = Store::new();
        assert!(!s.exists("anything"));
    }

    // ── Read-only flag ────────────────────────────────────────────────

    #[test]
    fn test_read_only_entry_preserves_value_through_update_attempts() {
        let mut s = Store::new();
        s.put("config", "immutable", 0, true);
        for _ in 0..5 {
            assert!(!s.update("config", "changed"));
        }
        assert_eq!(s.get("config"), Some("immutable".to_string()));
        assert_eq!(s.entries.get("config").unwrap().entry.version, 1);
    }

    #[test]
    fn test_read_only_flag_in_snapshot() {
        let mut s = Store::new();
        s.put("ro", "val", 0, true);
        s.put("rw", "val2", 0, false);
        let snap = s.snapshot("flags");
        let ro_entry = snap.entries.iter().find(|(k, _)| k == "ro").unwrap();
        let rw_entry = snap.entries.iter().find(|(k, _)| k == "rw").unwrap();
        assert!(ro_entry.1.read_only);
        assert!(!rw_entry.1.read_only);
    }
}
