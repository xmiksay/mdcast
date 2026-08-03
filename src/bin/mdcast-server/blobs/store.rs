//! Content-addressed blob cache, held entirely in memory.
//!
//! A blob's name *is* the sha256 of its bytes, which is what makes the whole
//! upload negotiation work: a cached blob can never be stale, so there is no
//! invalidation protocol, and eviction is invisible — a caller that gets a
//! `409` for something it uploaded last week simply uploads it again.
//!
//! Three properties the rest of the server depends on:
//!
//! - **Nothing is trusted.** Every uploaded byte stream is hashed as it
//!   arrives and compared against the digest it was claimed under; a mismatch
//!   never becomes addressable.
//! - **Eviction is second-chance, not evict-oldest.** Each entry carries a
//!   `lives` counter, incremented (capped) on every hit and decremented when
//!   it's touched as an eviction candidate; it's only actually dropped once
//!   drained to zero. Budget can transiently overshoot `max_bytes` while
//!   heavily-reused entries keep surviving — this self-corrects as later
//!   inserts trigger further sweeps and drain reluctant entries by one life
//!   each time. Pinned entries are never touched by eviction at all, so a
//!   permanent overshoot from `pinned_bytes` alone exceeding `max_bytes`
//!   never self-corrects (a `pin()` call warns loudly if this happens).
//! - **A blob evicted mid-render is not a hazard.** `Bytes` is `Arc`-backed:
//!   once `get()` hands back a clone, that clone owns its own reference and
//!   stays valid regardless of what the cache does to its own entry
//!   afterward. The one place eviction *could* be a hazard — a subprocess
//!   backend (pandoc) holding a real filesystem path into this store — is
//!   handled by `mirror_path()` handing out a **hard link**, not a path into
//!   the cache's own bookkeeping: once linked, the caller's copy is safe no
//!   matter what happens to the mirror's own directory entry afterward.

use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::{Context, Result};
use bytes::Bytes;
use lru::LruCache;
use mdcast::mdcast_api::Digest;
use sha2::{Digest as _, Sha256};
use tempfile::TempDir;

struct Entry {
    bytes: Bytes,
    /// Remaining eviction-survival chances. Incremented (capped at
    /// `max_lives`) on a hit, decremented when touched as an eviction
    /// candidate, dropped at zero.
    lives: u32,
    /// Whether a hard-link source file for this digest exists under
    /// `mirror_root` yet. Populated lazily, on the first `mirror_path()`
    /// call for this digest.
    mirrored: bool,
    /// Permanently exempt from eviction until `unpin()` or process exit.
    pinned: bool,
}

struct Inner {
    // Capacity is effectively unbounded by count — eviction is driven
    // entirely by `total_bytes` vs `max_bytes`, not by the crate's own
    // count-based capacity.
    cache: LruCache<Digest, Entry>,
    total_bytes: u64,
    pinned_bytes: u64,
}

pub struct BlobStore {
    inner: Mutex<Inner>,
    max_bytes: u64,
    start_lives: u32,
    max_lives: u32,
    /// Scratch directory holding one file per digest actually materialised
    /// via `mirror_path()` — not a copy of the whole cache, only what a
    /// subprocess backend has actually asked to hard-link.
    mirror_root: TempDir,
}

impl BlobStore {
    pub fn new(max_bytes: u64, start_lives: u32, max_lives: u32) -> Result<Self> {
        let mirror_root = TempDir::new().context("create blob mirror scratch dir")?;
        Ok(Self {
            inner: Mutex::new(Inner {
                cache: LruCache::unbounded(),
                total_bytes: 0,
                pinned_bytes: 0,
            }),
            max_bytes,
            start_lives,
            max_lives,
            mirror_root,
        })
    }

    /// `root/<first two hex chars>/<digest>` — sharded so a large mirror does
    /// not put a hundred thousand entries in one directory.
    fn mirror_path_for(&self, digest: &Digest) -> PathBuf {
        let hex = digest.as_str();
        self.mirror_root.path().join(&hex[..2]).join(hex)
    }

    /// Pure existence check — does not affect recency or lives, matching
    /// `/v1/blobs/check` semantics (checking isn't using).
    pub fn contains(&self, digest: &Digest) -> bool {
        self.inner.lock().unwrap().cache.peek(digest).is_some()
    }

    /// Read a blob, refreshing its LRU position and growing its eviction
    /// resistance. `Bytes` is refcounted, so this is a cheap clone, not a
    /// copy — and the returned handle stays valid even if this entry is
    /// evicted from the cache afterward.
    pub fn get(&self, digest: &Digest) -> Option<Bytes> {
        let mut inner = self.inner.lock().unwrap();
        let max_lives = self.max_lives;
        let entry = inner.cache.get_mut(digest)?;
        entry.lives = (entry.lives + 1).min(max_lives);
        Some(entry.bytes.clone())
    }

    /// Start a streaming write. The blob becomes addressable only when
    /// [`BlobWriter::finish`] verifies its digest.
    pub fn begin_write(&self) -> BlobWriter<'_> {
        BlobWriter {
            store: self,
            buf: Vec::new(),
            hasher: Sha256::new(),
        }
    }

    /// Convenience wrapper for callers holding the whole blob already. Test
    /// only — the upload route streams, and nothing else writes blobs.
    #[cfg(test)]
    pub fn put(&self, claimed: &Digest, bytes: &[u8]) -> Result<(), WriteError> {
        let mut writer = self.begin_write();
        writer.write(bytes);
        writer.finish(claimed)
    }

    /// Insert a finished, digest-verified blob, evicting as needed to stay
    /// within `max_bytes` (best-effort — see the module doc comment on soft
    /// overshoot).
    fn insert(&self, digest: Digest, bytes: Bytes) {
        let len = bytes.len() as u64;
        if len > self.max_bytes {
            tracing::warn!(
                %digest,
                len,
                max_bytes = self.max_bytes,
                "blob exceeds the whole cache budget; serving this render without caching it"
            );
            return;
        }

        let mut inner = self.inner.lock().unwrap();
        // Content addressing guarantees identical bytes for the same
        // digest, so a re-upload is normally a same-size no-op — but handle
        // the general case anyway, and preserve `lives`/`mirrored`/`pinned`
        // rather than resetting them: re-inserting an already-pinned
        // predefined asset must not silently un-pin it.
        if let Some(entry) = inner.cache.get_mut(&digest) {
            let old_len = entry.bytes.len() as u64;
            let was_pinned = entry.pinned;
            entry.bytes = bytes;
            inner.total_bytes = inner.total_bytes - old_len + len;
            if was_pinned {
                inner.pinned_bytes = inner.pinned_bytes - old_len + len;
            }
        } else {
            inner.cache.push(
                digest,
                Entry {
                    bytes,
                    lives: self.start_lives,
                    mirrored: false,
                    pinned: false,
                },
            );
            inner.total_bytes += len;
        }
        self.evict_locked(&mut inner);
    }

    /// Second-chance eviction sweep: walk least-recently-used first,
    /// skipping pinned entries outright and decrementing everyone else's
    /// `lives`, only actually removing an entry once its lives reach zero.
    /// Stops as soon as budget is met, or once every candidate has been
    /// touched once (soft overshoot — see the module doc comment).
    fn evict_locked(&self, inner: &mut Inner) {
        if inner.total_bytes <= self.max_bytes {
            return;
        }
        // `lru`'s iteration order is most-recently-used first; walk it
        // reversed to touch the least-recently-used candidate first.
        let candidates: Vec<Digest> = inner.cache.iter().rev().map(|(k, _)| k.clone()).collect();
        let mut freed = 0u64;
        for digest in candidates {
            if inner.total_bytes <= self.max_bytes {
                break;
            }
            let Some(entry) = inner.cache.peek_mut(&digest) else {
                continue; // shouldn't happen mid-sweep, but never panic on it
            };
            if entry.pinned {
                continue;
            }
            entry.lives = entry.lives.saturating_sub(1);
            if entry.lives > 0 {
                continue;
            }
            let Some((_, entry)) = inner.cache.pop_entry(&digest) else {
                continue;
            };
            let len = entry.bytes.len() as u64;
            inner.total_bytes -= len;
            freed += len;
            if entry.mirrored {
                let path = self.mirror_path_for(&digest);
                if let Err(e) = std::fs::remove_file(&path) {
                    tracing::warn!(path = %path.display(), error = %e, "evicting blob mirror failed");
                }
            }
        }
        if freed > 0 {
            tracing::info!(
                freed,
                remaining = inner.total_bytes,
                "evicted least-recently-used blobs"
            );
        }
    }

    /// Mark an already-cached digest permanently exempt from eviction.
    /// Returns `false` if the digest isn't present — a caller must insert
    /// (e.g. via a completed upload) before pinning it.
    ///
    /// No production caller exists yet — this is the primitive a future
    /// prefill routine (issue #72) will pin predefined assets through, kept
    /// deliberately independent of that not-yet-written ingestion loop. See
    /// `store_tests.rs` for coverage in the meantime.
    #[allow(dead_code)]
    pub fn pin(&self, digest: &Digest) -> bool {
        let mut inner = self.inner.lock().unwrap();
        let Some(entry) = inner.cache.peek_mut(digest) else {
            return false;
        };
        if !entry.pinned {
            entry.pinned = true;
            inner.pinned_bytes += entry.bytes.len() as u64;
        }
        if inner.pinned_bytes > self.max_bytes {
            tracing::warn!(
                pinned_bytes = inner.pinned_bytes,
                max_bytes = self.max_bytes,
                "pinned blobs alone exceed the cache budget; raise MDCAST_MAX_CACHE_BYTES or reduce the pinned set"
            );
        }
        true
    }

    /// Release a digest's pin. Returns `false` if the digest isn't present.
    #[allow(dead_code)] // see `pin`'s doc comment
    pub fn unpin(&self, digest: &Digest) -> bool {
        let mut inner = self.inner.lock().unwrap();
        let Some(entry) = inner.cache.peek_mut(digest) else {
            return false;
        };
        if entry.pinned {
            entry.pinned = false;
            inner.pinned_bytes -= entry.bytes.len() as u64;
        }
        true
    }

    #[allow(dead_code)] // see `pin`'s doc comment
    pub fn pinned_bytes(&self) -> u64 {
        self.inner.lock().unwrap().pinned_bytes
    }

    /// A real filesystem path holding this digest's bytes, suitable for
    /// hard-linking into a subprocess backend's own temp dir. Lazily
    /// materialises the mirror file on first call for a digest; returns
    /// `None` if the digest isn't cached — mirrors are entirely derived
    /// from and subordinate to RAM residency, so there's no "mirror and
    /// cache disagree" question to get wrong.
    ///
    /// The actual write happens off the async runtime via `spawn_blocking`
    /// (it's real disk I/O, sized by the whole blob), and `mirrored` is only
    /// set *after* the write is durably in place — not before — so a
    /// concurrent caller can never observe `mirrored` and hard-link a file
    /// that hasn't finished being written yet. Two concurrent first-calls
    /// for the same digest both write (via `tempfile`'s own unique naming,
    /// so their tmp files never collide) and persist to the same final
    /// path; since content addressing guarantees identical bytes, whichever
    /// persist wins is harmless.
    pub async fn mirror_path(&self, digest: &Digest) -> Result<Option<PathBuf>> {
        let bytes = {
            let mut inner = self.inner.lock().unwrap();
            let max_lives = self.max_lives;
            let Some(entry) = inner.cache.get_mut(digest) else {
                return Ok(None);
            };
            if entry.mirrored {
                return Ok(Some(self.mirror_path_for(digest)));
            }
            entry.lives = (entry.lives + 1).min(max_lives);
            entry.bytes.clone()
        };

        let final_path = self.mirror_path_for(digest);
        let dest = final_path.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let parent = dest
                .parent()
                .context("mirror path always has a shard parent")?;
            std::fs::create_dir_all(parent).context("create blob mirror shard dir")?;
            let mut tmp =
                tempfile::NamedTempFile::new_in(parent).context("create blob mirror tmp file")?;
            std::io::Write::write_all(&mut tmp, &bytes).context("write blob mirror tmp file")?;
            tmp.persist(&dest).context("publish blob mirror file")?;
            Ok(())
        })
        .await
        .context("blob mirror write task panicked")??;

        let mut inner = self.inner.lock().unwrap();
        if let Some(entry) = inner.cache.get_mut(digest) {
            entry.mirrored = true;
        }
        // Else: this digest was evicted between the write above and here.
        // The mirror file we just wrote is now orphaned — acceptable
        // (rare, and content-addressed, so it's harmless garbage rather
        // than stale/wrong data); it just won't be cleaned up by eviction
        // since eviction only knows about entries still in the cache.
        Ok(Some(final_path))
    }
}

/// A blob being written: bytes are hashed as they arrive and buffered in
/// memory — there is nothing to clean up on drop, since nothing outside the
/// buffer itself was ever touched.
pub struct BlobWriter<'a> {
    store: &'a BlobStore,
    buf: Vec<u8>,
    hasher: Sha256,
}

impl BlobWriter<'_> {
    pub fn write(&mut self, chunk: &[u8]) {
        self.hasher.update(chunk);
        self.buf.extend_from_slice(chunk);
    }

    /// Verify the accumulated hash against `claimed` and, only if they
    /// match, insert the blob into the cache under its digest.
    pub fn finish(self, claimed: &Digest) -> Result<(), WriteError> {
        let actual = Digest::from_sha256(self.hasher.finalize().into());
        if &actual != claimed {
            return Err(WriteError::DigestMismatch {
                claimed: claimed.clone(),
                actual,
            });
        }
        self.store.insert(claimed.clone(), Bytes::from(self.buf));
        Ok(())
    }
}

#[derive(Debug)]
pub enum WriteError {
    /// The bytes do not hash to the digest they were uploaded under. Always
    /// the caller's fault, and safe to report in full.
    DigestMismatch { claimed: Digest, actual: Digest },
}

/// sha256 of a whole slice — the one-shot counterpart of [`BlobWriter`]'s
/// incremental hashing. Test only: production code hashes as it streams.
#[cfg(test)]
pub fn sha256(bytes: &[u8]) -> Digest {
    Digest::from_sha256(Sha256::digest(bytes).into())
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
