//! Content-addressed blob cache on local disk.
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
//! - **Writes are atomic.** Bytes land in `tmp/`, and only a fully written,
//!   digest-verified file is `rename`d into place. A crashed upload can leave
//!   a temp file (wiped at startup), never a corrupt blob under a real digest.
//! - **Recency lives in the filesystem.** `get` touches the file's mtime and
//!   eviction sorts by it, so the LRU order needs no sidecar index and
//!   survives a restart.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use bytes::Bytes;
use mdcast::mdcast_api::Digest;
use sha2::{Digest as _, Sha256};
use tokio::fs;
use tokio::io::AsyncWriteExt;

/// Sub-directory holding partial uploads. Three characters, so it can never
/// collide with a two-hex-character shard directory.
const TMP_DIR: &str = "tmp";

pub struct BlobStore {
    root: PathBuf,
    max_bytes: u64,
}

impl BlobStore {
    /// Open (creating if needed) the store under `root`. Partial uploads left
    /// by a previous run are discarded: they are unreferenced by definition,
    /// since a temp file is never named by digest.
    pub async fn open(root: PathBuf, max_bytes: u64) -> Result<Self> {
        let tmp = root.join(TMP_DIR);
        fs::create_dir_all(&root)
            .await
            .with_context(|| format!("create blob dir {}", root.display()))?;
        if fs::metadata(&tmp).await.is_ok() {
            fs::remove_dir_all(&tmp)
                .await
                .with_context(|| format!("clear stale uploads in {}", tmp.display()))?;
        }
        fs::create_dir_all(&tmp)
            .await
            .with_context(|| format!("create upload dir {}", tmp.display()))?;
        Ok(Self { root, max_bytes })
    }

    /// `root/<first two hex chars>/<digest>` — sharded so a large cache does
    /// not put a hundred thousand entries in one directory.
    pub fn path_for(&self, digest: &Digest) -> PathBuf {
        let hex = digest.as_str();
        self.root.join(&hex[..2]).join(hex)
    }

    pub async fn contains(&self, digest: &Digest) -> bool {
        fs::metadata(self.path_for(digest)).await.is_ok()
    }

    /// Read a blob, refreshing its LRU position. A blob evicted between a
    /// manifest check and this call reads back as `None` rather than an error
    /// — the render then treats it as an unresolved asset, exactly as it would
    /// a key nobody ever uploaded.
    pub async fn get(&self, digest: &Digest) -> Result<Option<Bytes>> {
        let path = self.path_for(digest);
        match fs::read(&path).await {
            Ok(bytes) => {
                touch(&path).await;
                Ok(Some(Bytes::from(bytes)))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("read blob {digest}")),
        }
    }

    /// Start a streaming write. The blob becomes addressable only when
    /// [`BlobWriter::finish`] verifies its digest.
    pub async fn begin_write(&self) -> Result<BlobWriter<'_>> {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let name = format!(
            "{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let path = self.root.join(TMP_DIR).join(name);
        let file = fs::File::create(&path)
            .await
            .with_context(|| format!("create upload file {}", path.display()))?;
        Ok(BlobWriter {
            store: self,
            path,
            file,
            hasher: Sha256::new(),
            published: false,
        })
    }

    /// Convenience wrapper for callers holding the whole blob in memory. Test
    /// only — the upload route streams, and nothing else writes blobs.
    #[cfg(test)]
    pub async fn put(&self, claimed: &Digest, bytes: &[u8]) -> Result<(), WriteError> {
        let mut writer = self.begin_write().await.map_err(WriteError::Io)?;
        writer.write(bytes).await.map_err(WriteError::Io)?;
        writer.finish(claimed).await
    }

    /// Delete least-recently-used blobs until the store fits in its budget.
    /// Called after an upload; returns the number of bytes freed.
    ///
    /// The whole store is walked each time. That is deliberate: a node-local
    /// cache holds thousands of files, not millions, and an index would be one
    /// more thing to keep consistent with the filesystem across crashes.
    pub async fn evict(&self) -> Result<u64> {
        let (mut total, mut entries) = self.walk().await?;
        if total <= self.max_bytes {
            return Ok(0);
        }

        entries.sort_by_key(|(modified, _, _)| *modified);
        let mut freed = 0;
        for (_, len, path) in entries {
            if total <= self.max_bytes {
                break;
            }
            match fs::remove_file(&path).await {
                Ok(()) => {
                    total -= len;
                    freed += len;
                }
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "evicting blob failed")
                }
            }
        }
        tracing::info!(
            freed,
            remaining = total,
            "evicted least-recently-used blobs"
        );
        Ok(freed)
    }

    /// Every stored blob as `(mtime, size, path)`, plus the total size.
    async fn walk(&self) -> Result<(u64, Vec<(SystemTime, u64, PathBuf)>)> {
        let mut total = 0;
        let mut entries = Vec::new();
        let mut shards = fs::read_dir(&self.root)
            .await
            .with_context(|| format!("scan blob dir {}", self.root.display()))?;
        while let Some(shard) = shards.next_entry().await? {
            if shard.file_name() == TMP_DIR || !shard.file_type().await?.is_dir() {
                continue;
            }
            let mut files = fs::read_dir(shard.path()).await?;
            while let Some(file) = files.next_entry().await? {
                let Ok(meta) = file.metadata().await else {
                    continue; // evicted by a concurrent request mid-walk
                };
                if !meta.is_file() {
                    continue;
                }
                total += meta.len();
                entries.push((
                    meta.modified().unwrap_or(UNIX_EPOCH),
                    meta.len(),
                    file.path(),
                ));
            }
        }
        Ok((total, entries))
    }
}

/// A blob being written: bytes go to a temp file and through a hasher at the
/// same time, so verification costs no extra pass over the data.
pub struct BlobWriter<'a> {
    store: &'a BlobStore,
    path: PathBuf,
    file: fs::File,
    hasher: Sha256,
    published: bool,
}

impl BlobWriter<'_> {
    pub async fn write(&mut self, chunk: &[u8]) -> Result<()> {
        self.hasher.update(chunk);
        self.file.write_all(chunk).await.context("write blob chunk")
    }

    /// Verify the accumulated hash against `claimed` and, only if they match,
    /// publish the blob under its digest with an atomic rename.
    pub async fn finish(mut self, claimed: &Digest) -> Result<(), WriteError> {
        self.file.flush().await.map_err(io("flush blob"))?;
        // Cloned rather than consumed: the `Drop` impl below forbids moving a
        // field out of `self`.
        let actual = Digest::from_sha256(self.hasher.clone().finalize().into());
        if &actual != claimed {
            return Err(WriteError::DigestMismatch {
                claimed: claimed.clone(),
                actual,
            });
        }

        let final_path = self.store.path_for(claimed);
        if let Some(parent) = final_path.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(io("create blob shard dir"))?;
        }
        // Re-uploading an existing blob overwrites it with byte-identical
        // content, which is what makes the route idempotent.
        fs::rename(&self.path, &final_path)
            .await
            .map_err(io("publish blob"))?;
        self.published = true;
        Ok(())
    }
}

impl Drop for BlobWriter<'_> {
    /// A rejected or abandoned upload must not leave its temp file behind. The
    /// unlink is synchronous because `Drop` cannot await; it is a single
    /// syscall on a file this task just wrote, so it will not stall the
    /// runtime.
    fn drop(&mut self) {
        if !self.published {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[derive(Debug)]
pub enum WriteError {
    /// The bytes do not hash to the digest they were uploaded under. Always
    /// the caller's fault, and safe to report in full.
    DigestMismatch {
        claimed: Digest,
        actual: Digest,
    },
    Io(anyhow::Error),
}

fn io(what: &'static str) -> impl Fn(std::io::Error) -> WriteError {
    move |e| WriteError::Io(anyhow::Error::new(e).context(what))
}

/// Refresh a blob's LRU position. Best-effort: a store on a read-only or
/// `noatime`-ish mount still serves renders, it just evicts in a worse order.
async fn touch(path: &Path) {
    let owned = path.to_path_buf();
    let touched = tokio::task::spawn_blocking(move || {
        std::fs::File::options()
            .write(true)
            .open(&owned)?
            .set_modified(SystemTime::now())
    })
    .await;
    match touched {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            tracing::debug!(path = %path.display(), error = %e, "touching blob mtime failed; LRU order may drift")
        }
        Err(e) => tracing::debug!(error = %e, "blob mtime touch task failed"),
    }
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
