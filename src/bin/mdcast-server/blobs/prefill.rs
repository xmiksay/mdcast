//! Startup blob-store warmup from a predefined asset directory
//! (`MDCAST_PREFILL_DIR`, issue #72).
//!
//! Issue #71 already built every primitive this needs — [`BlobStore::pin`],
//! [`BlobStore::mirror_path`], and the digest-verifying
//! `begin_write`/`write`/`finish` path a real upload goes through. This
//! module is just the ingestion loop that calls them: walk the directory
//! recursively, hash every regular file, ingest it through the same path an
//! upload would, then pin and eagerly mirror it — a prefilled asset is known
//! in advance to be pandoc-reused and exempt from eviction, so there's no
//! reason to wait for a first render to pay either cost.
//!
//! Idempotent by construction: a digest already in the store is skipped
//! (still pinned/mirrored, in case a previous run crashed between insert and
//! pin), so a restart against the same directory re-ingests nothing.
//!
//! A missing/unreadable directory or file is fatal, not a warning — an
//! operator who set `MDCAST_PREFILL_DIR` expects every file under it to
//! become a pinned, mirrored blob; silently starting with a partially warm
//! (or stone-cold) cache is a broken deployment, not a transient condition
//! worth degrading past.

use std::collections::VecDeque;
use std::path::Path;

use anyhow::{Context, Result, bail};
use mdcast::mdcast_api::Digest;
use sha2::{Digest as _, Sha256};

use super::store::BlobStore;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct PrefillSummary {
    pub ingested: usize,
    pub already_present: usize,
    pub bytes: u64,
}

/// Walk `dir` recursively, ingesting every regular file into `store`.
pub async fn prefill(store: &BlobStore, dir: &Path) -> Result<PrefillSummary> {
    let mut summary = PrefillSummary::default();
    let mut dirs = VecDeque::from([dir.to_path_buf()]);

    while let Some(current) = dirs.pop_front() {
        let mut entries = tokio::fs::read_dir(&current)
            .await
            .with_context(|| format!("read prefill directory {}", current.display()))?;
        while let Some(entry) = entries
            .next_entry()
            .await
            .with_context(|| format!("walk prefill directory {}", current.display()))?
        {
            let path = entry.path();
            let file_type = entry
                .file_type()
                .await
                .with_context(|| format!("stat prefill entry {}", path.display()))?;
            if file_type.is_dir() {
                dirs.push_back(path);
            } else {
                ingest_file(store, &path, &mut summary).await?;
            }
        }
    }

    Ok(summary)
}

async fn ingest_file(store: &BlobStore, path: &Path, summary: &mut PrefillSummary) -> Result<()> {
    let bytes = tokio::fs::read(path)
        .await
        .with_context(|| format!("read prefill file {}", path.display()))?;
    let digest = Digest::from_sha256(Sha256::digest(&bytes).into());

    if store.contains(&digest) {
        summary.already_present += 1;
    } else {
        let mut writer = store.begin_write();
        writer.write(&bytes);
        // `digest` is computed from these exact `bytes`, so verification
        // can never fail — `finish` is still the right call, not a bypass,
        // because it's the one path that makes a blob addressable at all.
        if writer.finish(&digest).is_err() {
            bail!(
                "prefill file {} failed digest verification (this should never happen)",
                path.display()
            );
        }
        summary.ingested += 1;
        summary.bytes += bytes.len() as u64;
    }

    if !store.pin(&digest) {
        bail!(
            "prefill file {} vanished from the store immediately after ingestion",
            path.display()
        );
    }
    store
        .mirror_path(&digest)
        .await
        .with_context(|| format!("mirror prefill file {}", path.display()))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn store() -> BlobStore {
        BlobStore::new(1024 * 1024, 3, 32).unwrap()
    }

    async fn write(dir: &Path, rel: &str, contents: &[u8]) {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.unwrap();
        }
        tokio::fs::write(path, contents).await.unwrap();
    }

    #[tokio::test]
    async fn ingests_every_file_recursively_pinned_and_mirrored() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "font.ttf", b"font bytes").await;
        write(dir.path(), "brand/logo.svg", b"<svg/>").await;
        let store = store();

        let summary = prefill(&store, dir.path()).await.unwrap();

        assert_eq!(
            summary,
            PrefillSummary {
                ingested: 2,
                already_present: 0,
                bytes: (b"font bytes".len() + b"<svg/>".len()) as u64,
            }
        );

        let font_digest = Digest::from_sha256(Sha256::digest(b"font bytes").into());
        let logo_digest = Digest::from_sha256(Sha256::digest(b"<svg/>").into());
        assert!(store.contains(&font_digest));
        assert!(store.contains(&logo_digest));

        let pinned_bytes_before = store.pinned_bytes();
        assert_eq!(
            pinned_bytes_before,
            b"font bytes".len() as u64 + b"<svg/>".len() as u64,
            "every prefilled blob must be pinned"
        );

        assert!(
            store.mirror_path(&font_digest).await.unwrap().is_some(),
            "prefilled blobs must already be mirrored"
        );
    }

    #[tokio::test]
    async fn restarting_against_the_same_directory_ingests_nothing_new() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "font.ttf", b"font bytes").await;
        let store = store();

        prefill(&store, dir.path()).await.unwrap();
        let second = prefill(&store, dir.path()).await.unwrap();

        assert_eq!(
            second,
            PrefillSummary {
                ingested: 0,
                already_present: 1,
                bytes: 0,
            }
        );
    }

    #[tokio::test]
    async fn a_missing_directory_fails_startup_naming_the_path() {
        let store = store();
        let missing = PathBuf::from("/does/not/exist/mdcast-prefill");

        let err = prefill(&store, &missing).await.unwrap_err();

        assert!(err.to_string().contains("/does/not/exist/mdcast-prefill"));
    }

    #[tokio::test]
    async fn an_unreadable_file_fails_startup_naming_the_path() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "secret.bin", b"shh").await;
        let path = dir.path().join("secret.bin");
        tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000))
            .await
            .unwrap();
        let store = store();

        let err = prefill(&store, dir.path()).await.unwrap_err();

        // Restore permissions so the tempdir can clean itself up.
        tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .await
            .unwrap();

        assert!(err.to_string().contains("secret.bin"), "{err}");
    }

    #[tokio::test]
    async fn an_empty_directory_ingests_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = store();

        let summary = prefill(&store, dir.path()).await.unwrap();

        assert_eq!(summary, PrefillSummary::default());
    }
}
