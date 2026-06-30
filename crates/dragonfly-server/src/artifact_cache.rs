//! Zero-copy in-memory cache of served artifacts (boot assets, OS/cached images).
//!
//! Under concurrent PXE boot, N machines pull the same large artifact (e.g.
//! modloop, ~295 MiB) at once. Serving each from disk independently re-reads
//! the file N times off one PVE-backed store, collapsing throughput and
//! stalling every consumer. This cache loads each file into memory ONCE into a
//! single shared `Bytes`; every concurrent client then streams zero-copy
//! `Bytes::slice` views of that one allocation — refcounted (CoW-on-write,
//! never written), so N clients pay for one disk read, not N.
//!
//! Population is single-flight: the first caller loads; concurrent callers
//! await a per-path lock, double-check, then clone the shared buffer (no
//! duplicate loads, no torn reads). The cache is size-bounded: when an insert
//! would exceed the cap, least-recently-used entries are evicted. Files larger
//! than the cap are reported as [`GetResult::TooLarge`] so the caller streams
//! them from disk via the existing path rather than caching.

use bytes::Bytes;
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Default cache cap (1 GiB). Fits the boot-asset set (~350 MiB) with headroom
/// on a 2 GiB host; override with `DRAGONFLY_ARTIFACT_CACHE_BYTES`.
pub const DEFAULT_MAX_BYTES: u64 = 1024 * 1024 * 1024;
/// Streaming chunk size for the zero-copy fan-out (matches the prior disk path).
pub const STREAM_CHUNK: usize = 65_536;

/// One cached artifact: its shared bytes + LRU bookkeeping.
struct Entry {
    bytes: Bytes,
    last_used: Instant,
    len: u64,
}

/// Outcome of [`ArtifactCache::get_or_load`].
pub enum GetResult {
    /// The file's bytes, shared (clone the `Bytes` for a zero-copy view).
    Cached(Bytes),
    /// File is larger than the cache cap (carries its length); caller should
    /// stream it from disk instead of caching.
    TooLarge(u64),
}

pub struct ArtifactCache {
    /// Completed loads, keyed by canonical path.
    entries: Mutex<HashMap<PathBuf, Entry>>,
    /// Per-path single-flight locks. A path is present while a load is
    /// rendezvousing; concurrent callers clone the same `Arc<Mutex>` and await.
    /// Cardinality is bounded by the distinct-file set actually served, so
    /// lingering entries (a few dozen boot assets/images) are negligible.
    in_flight: Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>,
    max_bytes: u64,
    current_bytes: AtomicU64,
}

impl ArtifactCache {
    pub fn new(max_bytes: u64) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            in_flight: Mutex::new(HashMap::new()),
            max_bytes,
            current_bytes: AtomicU64::new(0),
        }
    }

    /// Read the cap from `DRAGONFLY_ARTIFACT_CACHE_BYTES`, else [`DEFAULT_MAX_BYTES`].
    pub fn max_bytes_from_env() -> u64 {
        std::env::var("DRAGONFLY_ARTIFACT_CACHE_BYTES")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|&v| v > 0)
            .unwrap_or(DEFAULT_MAX_BYTES)
    }

    /// Fast path: clone the cached bytes (refcount bump — zero-copy) and bump
    /// last-used. `None` if absent.
    fn get_cached(&self, path: &Path) -> Option<Bytes> {
        let mut entries = self.entries.lock().ok()?;
        let e = entries.get_mut(path)?;
        e.last_used = Instant::now();
        Some(e.bytes.clone())
    }

    /// Get-or-create the per-path single-flight lock.
    fn inflight_for(&self, path: &Path) -> Arc<tokio::sync::Mutex<()>> {
        let mut inflight = self.in_flight.lock().expect("in_flight mutex poisoned");
        inflight
            .entry(path.to_path_buf())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    /// Get the file's bytes, loading it once (single-flight) if absent. Returns
    /// [`GetResult::TooLarge`] (uncached) when the file exceeds the cap.
    pub async fn get_or_load(&self, path: &Path) -> Result<GetResult, io::Error> {
        // Fast path: already cached.
        if let Some(b) = self.get_cached(path) {
            return Ok(GetResult::Cached(b));
        }
        // Single-flight: serialize concurrent first-loaders on this path.
        let lock = self.inflight_for(path);
        let _guard = lock.lock().await;
        // Double-check after acquiring — another waiter may have just loaded it.
        if let Some(b) = self.get_cached(path) {
            return Ok(GetResult::Cached(b));
        }
        // Load once.
        let vec = tokio::fs::read(path).await?;
        let len = vec.len() as u64;
        if len > self.max_bytes {
            return Ok(GetResult::TooLarge(len));
        }
        let bytes = Bytes::from(vec);
        self.insert(path, bytes.clone(), len);
        Ok(GetResult::Cached(bytes))
    }

    /// Insert an entry, evicting least-recently-used entries until it fits.
    fn insert(&self, path: &Path, bytes: Bytes, len: u64) {
        let mut entries = self.entries.lock().expect("entries mutex poisoned");
        // If refreshing an existing path, account for the replaced length.
        if let Some(old) = entries.insert(
            path.to_path_buf(),
            Entry {
                bytes,
                last_used: Instant::now(),
                len,
            },
        ) {
            self.current_bytes.fetch_sub(old.len, Ordering::Relaxed);
        }
        self.current_bytes.fetch_add(len, Ordering::Relaxed);
        // Evict LRU until within cap. Never evict the just-inserted entry.
        while self.current_bytes.load(Ordering::Relaxed) > self.max_bytes {
            let victim = entries
                .iter()
                .filter(|(k, _)| *k != path)
                .min_by_key(|(_, e)| e.last_used)
                .map(|(k, _)| k.clone());
            match victim {
                Some(k) => {
                    if let Some(e) = entries.remove(&k) {
                        self.current_bytes.fetch_sub(e.len, Ordering::Relaxed);
                    }
                }
                None => break, // only the just-inserted entry remains
            }
        }
    }

    /// Current total cached bytes (for tests/observability).
    pub fn current_bytes(&self) -> u64 {
        self.current_bytes.load(Ordering::Relaxed)
    }
}

/// Stream `slice` in [`STREAM_CHUNK`]-sized zero-copy chunks through `tx`.
/// Each chunk is a `Bytes::split_to` view of the shared allocation — no copy,
/// no disk. The receiver drains at the client's read rate (channel backpressure).
pub fn spawn_zero_copy_stream(
    slice: Bytes,
    tx: tokio::sync::mpsc::Sender<Result<Bytes, dragonfly_common::Error>>,
) {
    tokio::spawn(async move {
        let mut remaining = slice;
        while !remaining.is_empty() {
            let take = std::cmp::min(remaining.len(), STREAM_CHUNK);
            let chunk = remaining.split_to(take);
            if tx.send(Ok(chunk)).await.is_err() {
                return; // receiver dropped (client disconnected)
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::fs;

    async fn write_tmp(name: &str, contents: &[u8]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "dragonfly-artifact-cache-test-{}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).await.unwrap();
        let p = dir.join(name);
        fs::write(&p, contents).await.unwrap();
        p
    }

    // Two Bytes share one allocation iff their data pointers are equal. A
    // re-read would allocate fresh -> different pointer. So pointer equality
    // is the proof of "served from the cache, not re-read from disk".
    fn share_allocation(a: &Bytes, b: &Bytes) -> bool {
        a.as_ptr() == b.as_ptr()
    }

    #[tokio::test]
    async fn loads_on_miss_then_serves_subsequent_calls_from_memory() {
        let cache = ArtifactCache::new(1024);
        let path = write_tmp("load_once.bin", b"hello artifact cache").await;
        let first = match cache.get_or_load(&path).await.unwrap() {
            GetResult::Cached(b) => b,
            _ => panic!("expected Cached"),
        };
        let second = match cache.get_or_load(&path).await.unwrap() {
            GetResult::Cached(b) => b,
            _ => panic!("expected Cached"),
        };
        assert!(
            share_allocation(&first, &second),
            "second call re-read from disk"
        );
        let _ = fs::remove_file(&path).await;
    }

    #[tokio::test]
    async fn single_flight_loads_once_for_n_concurrent_callers() {
        let cache = Arc::new(ArtifactCache::new(65_536));
        let path = Arc::new(write_tmp("singleflight.bin", &[7u8; 4096]).await);

        // N concurrent first-loaders on the same cold path.
        let results = futures::future::join_all((0..16).map(|_| {
            let cache = Arc::clone(&cache);
            let path = Arc::clone(&path);
            async move { cache.get_or_load(&path).await.unwrap() }
        }))
        .await;

        // Every caller must have received the SAME allocation — one load, not 16.
        let first_ptr = match &results[0] {
            GetResult::Cached(b) => b.as_ptr(),
            _ => panic!("expected Cached"),
        };
        for r in &results {
            match r {
                GetResult::Cached(b) => assert_eq!(
                    b.as_ptr(),
                    first_ptr,
                    "concurrent caller got an independent allocation (no single-flight)"
                ),
                _ => panic!("expected Cached"),
            }
        }
        let _ = fs::remove_file(&*path).await;
    }

    #[tokio::test]
    async fn slice_is_zero_copy() {
        let cache = ArtifactCache::new(1_048_576);
        let path = write_tmp("slice.bin", &[9u8; 100_000]).await;
        let full = match cache.get_or_load(&path).await.unwrap() {
            GetResult::Cached(b) => b,
            _ => panic!("expected Cached"),
        };
        // A slice shares the underlying allocation (offset pointer from base).
        let mid = full.slice(40_000..60_000);
        assert_eq!(mid.as_ptr() as usize, full.as_ptr() as usize + 40_000);
        assert_eq!(&mid[..3], &full[40_000..40_003], "slice content differs");
        let _ = fs::remove_file(&path).await;
    }

    #[tokio::test]
    async fn evicts_lru_when_cap_exceeded() {
        // Cap holds exactly one file of this size.
        const SIZE: usize = 1024;
        let cache = ArtifactCache::new(SIZE as u64);
        let a = write_tmp("a.bin", &[1u8; SIZE]).await;
        let b = write_tmp("b.bin", &[2u8; SIZE]).await;

        let first_a = match cache.get_or_load(&a).await.unwrap() {
            GetResult::Cached(b) => b,
            _ => panic!("expected Cached"),
        };
        // Loading B (same size) exceeds the cap -> evicts A (the LRU).
        let _ = match cache.get_or_load(&b).await.unwrap() {
            GetResult::Cached(b) => b,
            _ => panic!("expected Cached"),
        };
        // A was evicted, so re-loading it must allocate fresh (new pointer).
        let second_a = match cache.get_or_load(&a).await.unwrap() {
            GetResult::Cached(b) => b,
            _ => panic!("expected Cached"),
        };
        assert!(
            !share_allocation(&first_a, &second_a),
            "A was not evicted (re-load returned the cached allocation)"
        );
        let _ = fs::remove_file(&a).await;
        let _ = fs::remove_file(&b).await;
    }

    #[tokio::test]
    async fn oversized_file_is_not_cached() {
        let cache = ArtifactCache::new(100);
        let path = write_tmp("oversize.bin", &[0u8; 200]).await;
        match cache.get_or_load(&path).await.unwrap() {
            GetResult::TooLarge(len) => assert_eq!(len, 200),
            GetResult::Cached(_) => panic!("oversized file was cached"),
        }
        assert_eq!(
            cache.current_bytes(),
            0,
            "oversized file counted toward cap"
        );
        let _ = fs::remove_file(&path).await;
    }
}
