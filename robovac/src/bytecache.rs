//! A small byte cache for proxied robot responses.
//!
//! Only obstacle photos need this today, and they need it because Valetudo
//! rate-limits them tightly: three per second, ten per five seconds, thirty per
//! twenty. A page that eagerly loaded every obstacle photo would therefore get
//! throttled by the very endpoint it is trying to use.
//!
//! Deliberately bounded by total bytes rather than entry count, since a single
//! camera frame is far larger than a map segment name and an entry-count limit
//! would be trivially defeated by many large entries.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

struct Entry {
    key: String,
    bytes: Vec<u8>,
    content_type: String,
    stored: Instant,
}

pub struct ByteCache {
    max_bytes: usize,
    ttl: Duration,
    entries: Mutex<VecDeque<Entry>>,
}

impl ByteCache {
    pub fn new(max_bytes: usize, ttl: Duration) -> Self {
        Self {
            max_bytes,
            ttl,
            entries: Mutex::new(VecDeque::new()),
        }
    }

    pub fn get(&self, key: &str) -> Option<(Vec<u8>, String)> {
        let mut entries = self.lock();

        // Drop expired entries as we go, so a key that is stale but not at the
        // front does not leak indefinitely.
        let now = Instant::now();
        entries.retain(|e| now.duration_since(e.stored) < self.ttl);

        let position = entries.iter().position(|e| e.key == key)?;
        let entry = entries.remove(position)?;
        let value = (entry.bytes.clone(), entry.content_type.clone());
        // Re-insert at the back: this is least-recently-used ordering.
        entries.push_back(entry);
        Some(value)
    }

    pub fn put(&self, key: &str, bytes: Vec<u8>, content_type: &str) {
        if bytes.len() > self.max_bytes {
            // A single oversized entry would evict everything and still not fit.
            return;
        }

        let mut entries = self.lock();
        entries.retain(|e| e.key != key);

        while entries.iter().map(|e| e.bytes.len()).sum::<usize>() + bytes.len() > self.max_bytes {
            match entries.pop_front() {
                Some(_) => continue,
                None => break,
            }
        }

        entries.push_back(Entry {
            key: key.to_string(),
            bytes,
            content_type: content_type.to_string(),
            stored: Instant::now(),
        });
    }

    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Guard against a panic while holding the lock poisoning the cache forever;
    /// a broken cache should degrade to a miss, not take the process down.
    fn lock(&self) -> std::sync::MutexGuard<'_, VecDeque<Entry>> {
        self.entries.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache() -> ByteCache {
        ByteCache::new(100, Duration::from_secs(60))
    }

    #[test]
    fn stores_and_returns() {
        let c = cache();
        c.put("a", vec![1, 2, 3], "image/jpeg");
        assert_eq!(c.get("a"), Some((vec![1, 2, 3], "image/jpeg".to_string())));
    }

    #[test]
    fn missing_key_is_a_miss() {
        assert_eq!(cache().get("nope"), None);
    }

    #[test]
    fn evicts_least_recently_used_when_full() {
        // Room for exactly two 6-byte entries, so the third forces an eviction.
        let c = ByteCache::new(12, Duration::from_secs(60));
        c.put("a", vec![0; 6], "image/jpeg");
        c.put("b", vec![0; 6], "image/jpeg");
        assert_eq!(c.len(), 2);

        // Touch "a" so "b" becomes the least recently used.
        assert!(c.get("a").is_some());
        c.put("c", vec![0; 6], "image/jpeg");

        assert!(c.get("a").is_some(), "recently used should survive");
        assert_eq!(c.get("b"), None, "least recently used should be evicted");
        assert!(c.get("c").is_some());
    }

    #[test]
    fn replacing_a_key_does_not_double_count() {
        let c = ByteCache::new(10, Duration::from_secs(60));
        c.put("a", vec![0; 6], "image/jpeg");
        c.put("a", vec![0; 6], "image/jpeg");
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn oversized_entries_are_refused() {
        let c = ByteCache::new(8, Duration::from_secs(60));
        c.put("big", vec![0; 100], "image/jpeg");
        assert!(
            c.is_empty(),
            "an entry larger than the cache must not evict everything"
        );
        assert_eq!(c.get("big"), None);
    }

    #[test]
    fn expired_entries_are_dropped() {
        let c = ByteCache::new(100, Duration::from_millis(1));
        c.put("a", vec![1], "image/jpeg");
        std::thread::sleep(Duration::from_millis(5));
        assert_eq!(c.get("a"), None, "stale entries must not be served");
    }

    #[test]
    fn concurrent_access_is_safe() {
        let c = std::sync::Arc::new(ByteCache::new(1000, Duration::from_secs(60)));
        let mut handles = Vec::new();
        for i in 0..8 {
            let c = c.clone();
            handles.push(std::thread::spawn(move || {
                for n in 0..50 {
                    let key = format!("k{}", (i * 50 + n) % 20);
                    c.put(&key, vec![0; 8], "image/jpeg");
                    let _ = c.get(&key);
                }
            }));
        }
        for h in handles {
            h.join().expect("no thread should panic");
        }
        assert!(c.len() <= 20);
    }
}
