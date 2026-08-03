//! Unit tests for [`super::BlobStore`], split out of `store.rs` to keep both
//! files under the size cap (same convention as `backends/*_tests.rs`).

use super::*;

const CAP: u64 = 1024 * 1024;

async fn store(dir: &tempfile::TempDir, max_bytes: u64) -> BlobStore {
    BlobStore::open(dir.path().to_path_buf(), max_bytes)
        .await
        .unwrap()
}

fn tmp_entries(store: &BlobStore) -> Vec<PathBuf> {
    std::fs::read_dir(store.root.join(TMP_DIR))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect()
}

#[tokio::test]
async fn a_stored_blob_reads_back_under_its_digest() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(&dir, CAP).await;
    let digest = sha256(b"hello");

    assert!(!store.contains(&digest).await);
    store.put(&digest, b"hello").await.unwrap();

    assert!(store.contains(&digest).await);
    assert_eq!(store.get(&digest).await.unwrap().unwrap(), &b"hello"[..]);
}

#[tokio::test]
async fn an_unknown_digest_reads_back_as_none() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(&dir, CAP).await;

    assert_eq!(store.get(&sha256(b"never uploaded")).await.unwrap(), None);
}

#[tokio::test]
async fn bytes_that_do_not_match_the_claimed_digest_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(&dir, CAP).await;
    let claimed = sha256(b"the bytes I promised");

    let err = store.put(&claimed, b"the bytes I sent").await.unwrap_err();

    match err {
        WriteError::DigestMismatch { claimed: c, actual } => {
            assert_eq!(c, claimed);
            assert_eq!(actual, sha256(b"the bytes I sent"));
        }
        WriteError::Io(e) => panic!("expected a digest mismatch, got {e:#}"),
    }
    assert!(
        !store.contains(&claimed).await,
        "rejected bytes must never become addressable under the claimed digest"
    );
    assert!(
        !store.contains(&sha256(b"the bytes I sent")).await,
        "rejected bytes must not become addressable under their real digest either"
    );
    assert!(
        tmp_entries(&store).is_empty(),
        "a rejected upload must not leave its temp file behind"
    );
}

/// The write is atomic in the sense that matters: nothing is addressable under
/// the digest until every byte has arrived and hashed correctly. Assert it
/// mid-stream, before `finish`.
#[tokio::test]
async fn a_half_written_blob_is_not_addressable() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(&dir, CAP).await;
    let digest = sha256(b"first-half second-half");

    let mut writer = store.begin_write().await.unwrap();
    writer.write(b"first-half ").await.unwrap();
    assert!(
        !store.contains(&digest).await,
        "visible before it was complete"
    );
    assert_eq!(tmp_entries(&store).len(), 1, "partial bytes live in tmp/");

    writer.write(b"second-half").await.unwrap();
    writer.finish(&digest).await.unwrap();

    assert!(store.contains(&digest).await);
    assert!(
        tmp_entries(&store).is_empty(),
        "tmp file should be renamed away"
    );
}

#[tokio::test]
async fn an_abandoned_upload_cleans_up_after_itself() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(&dir, CAP).await;

    {
        let mut writer = store.begin_write().await.unwrap();
        writer.write(b"interrupted").await.unwrap();
    }

    assert!(tmp_entries(&store).is_empty());
}

#[tokio::test]
async fn opening_the_store_discards_partial_uploads_from_a_previous_run() {
    let dir = tempfile::tempdir().unwrap();
    let leftover = dir.path().join(TMP_DIR).join("crashed");
    std::fs::create_dir_all(dir.path().join(TMP_DIR)).unwrap();
    std::fs::write(&leftover, b"half a blob").unwrap();

    let store = store(&dir, CAP).await;

    assert!(!leftover.exists());
    assert!(tmp_entries(&store).is_empty());
}

#[tokio::test]
async fn re_uploading_the_same_blob_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(&dir, CAP).await;
    let digest = sha256(b"same bytes");

    for _ in 0..3 {
        store.put(&digest, b"same bytes").await.unwrap();
    }

    assert_eq!(
        store.get(&digest).await.unwrap().unwrap(),
        &b"same bytes"[..]
    );
    let shard = std::fs::read_dir(store.path_for(&digest).parent().unwrap()).unwrap();
    assert_eq!(
        shard.count(),
        1,
        "one file per digest, no matter how often it is sent"
    );
}

/// Backdate a blob's mtime so LRU order is deterministic instead of racing the
/// filesystem clock.
fn set_age(store: &BlobStore, digest: &Digest, seconds_ago: u64) {
    let when = SystemTime::now() - std::time::Duration::from_secs(seconds_ago);
    std::fs::File::options()
        .write(true)
        .open(store.path_for(digest))
        .unwrap()
        .set_modified(when)
        .unwrap();
}

#[tokio::test]
async fn eviction_drops_the_least_recently_used_blobs_first() {
    let dir = tempfile::tempdir().unwrap();
    // Budget for two of the three 100-byte blobs below.
    let store = store(&dir, 250).await;

    let blobs: Vec<(Digest, Vec<u8>)> = (0u8..3)
        .map(|i| {
            let bytes = vec![i; 100];
            (sha256(&bytes), bytes)
        })
        .collect();
    for (digest, bytes) in &blobs {
        store.put(digest, bytes).await.unwrap();
    }
    set_age(&store, &blobs[0].0, 300); // oldest
    set_age(&store, &blobs[1].0, 200);
    set_age(&store, &blobs[2].0, 100); // newest

    let freed = store.evict().await.unwrap();

    assert_eq!(freed, 100, "only the overflow should go");
    assert!(
        !store.contains(&blobs[0].0).await,
        "oldest blob survived eviction"
    );
    assert!(store.contains(&blobs[1].0).await);
    assert!(store.contains(&blobs[2].0).await);
}

#[tokio::test]
async fn eviction_is_a_no_op_under_the_cap() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(&dir, CAP).await;
    let digest = sha256(b"small");
    store.put(&digest, b"small").await.unwrap();

    assert_eq!(store.evict().await.unwrap(), 0);
    assert!(store.contains(&digest).await);
}

/// Reading a blob refreshes its LRU position, which is the whole reason
/// eviction can use mtime instead of a sidecar index.
#[tokio::test]
async fn reading_a_blob_refreshes_its_recency() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(&dir, 250).await;

    let blobs: Vec<(Digest, Vec<u8>)> = (0u8..3)
        .map(|i| {
            let bytes = vec![i; 100];
            (sha256(&bytes), bytes)
        })
        .collect();
    for (digest, bytes) in &blobs {
        store.put(digest, bytes).await.unwrap();
    }
    set_age(&store, &blobs[0].0, 300);
    set_age(&store, &blobs[1].0, 200);
    set_age(&store, &blobs[2].0, 100);

    // The oldest blob is used by a render — it must now outlive the others.
    store.get(&blobs[0].0).await.unwrap().unwrap();
    store.evict().await.unwrap();

    assert!(
        store.contains(&blobs[0].0).await,
        "a just-read blob was evicted"
    );
    assert!(!store.contains(&blobs[1].0).await);
}
