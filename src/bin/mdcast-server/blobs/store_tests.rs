//! Unit tests for [`super::BlobStore`], split out of `store.rs` to keep both
//! files under the size cap (same convention as `backends/*_tests.rs`).

use super::*;

/// Ample budget/lives for tests that aren't specifically exercising eviction.
fn store() -> BlobStore {
    BlobStore::new(1024 * 1024, 3, 32).unwrap()
}

#[test]
fn a_stored_blob_reads_back_under_its_digest() {
    let store = store();
    let digest = sha256(b"hello");

    assert!(!store.contains(&digest));
    store.put(&digest, b"hello").unwrap();

    assert!(store.contains(&digest));
    assert_eq!(store.get(&digest).unwrap(), &b"hello"[..]);
}

#[test]
fn an_unknown_digest_reads_back_as_none() {
    let store = store();

    assert_eq!(store.get(&sha256(b"never uploaded")), None);
}

#[test]
fn bytes_that_do_not_match_the_claimed_digest_are_rejected() {
    let store = store();
    let claimed = sha256(b"the bytes I promised");

    let err = store.put(&claimed, b"the bytes I sent").unwrap_err();

    let WriteError::DigestMismatch { claimed: c, actual } = err;
    assert_eq!(c, claimed);
    assert_eq!(actual, sha256(b"the bytes I sent"));
    assert!(
        !store.contains(&claimed),
        "rejected bytes must never become addressable under the claimed digest"
    );
    assert!(
        !store.contains(&sha256(b"the bytes I sent")),
        "rejected bytes must not become addressable under their real digest either"
    );
}

/// The write is atomic in the sense that matters: nothing is addressable
/// under the digest until `finish` has verified the whole thing.
#[test]
fn a_half_written_blob_is_not_addressable() {
    let store = store();
    let digest = sha256(b"first-half second-half");

    let mut writer = store.begin_write();
    writer.write(b"first-half ");
    assert!(!store.contains(&digest), "visible before it was complete");

    writer.write(b"second-half");
    writer.finish(&digest).unwrap();

    assert!(store.contains(&digest));
}

#[test]
fn re_uploading_the_same_blob_is_idempotent() {
    let store = store();
    let digest = sha256(b"same bytes");

    for _ in 0..3 {
        store.put(&digest, b"same bytes").unwrap();
    }

    assert_eq!(store.get(&digest).unwrap(), &b"same bytes"[..]);
}

/// A blob touched once by an eviction sweep (its life decremented but not
/// drained to zero) survives; it takes exactly `lives` sweeps' worth of
/// pressure — each one triggered by inserting another oversized blob that
/// forces a sweep — to actually evict it.
#[test]
fn second_chance_survival_takes_exactly_lives_sweeps() {
    // Budget fits exactly one 10-byte blob; every subsequent insert forces
    // a sweep against `target`.
    let store = BlobStore::new(10, 2, 32).unwrap();
    let target = sha256(b"0123456789");
    store.put(&target, b"0123456789").unwrap();

    // First sweep: lives 2 -> 1, survives.
    store.put(&sha256(b"aaaaaaaaaa"), b"aaaaaaaaaa").unwrap();
    assert!(store.contains(&target), "should survive its first sweep");

    // Second sweep: lives 1 -> 0, evicted.
    store.put(&sha256(b"bbbbbbbbbb"), b"bbbbbbbbbb").unwrap();
    assert!(
        !store.contains(&target),
        "should be evicted on its second sweep"
    );
}

/// A hit increments (capped) an entry's lives rather than resetting it, so
/// a frequently-read blob outlasts one read only once.
#[test]
fn a_hit_grows_lives_up_to_the_cap() {
    let store = BlobStore::new(10, 1, 2).unwrap();
    let target = sha256(b"0123456789");
    store.put(&target, b"0123456789").unwrap();

    // A hit grows lives from 1 towards the cap of 2.
    store.get(&target).unwrap();

    // First sweep after the hit: lives 2 -> 1, survives.
    store.put(&sha256(b"aaaaaaaaaa"), b"aaaaaaaaaa").unwrap();
    assert!(store.contains(&target));

    // Second sweep: lives 1 -> 0, evicted (the cap stopped further growth
    // from more than one hit, so this isn't survivable indefinitely).
    store.put(&sha256(b"bbbbbbbbbb"), b"bbbbbbbbbb").unwrap();
    assert!(!store.contains(&target));
}

#[test]
fn eviction_is_a_no_op_under_the_cap() {
    let store = store();
    let digest = sha256(b"small");
    store.put(&digest, b"small").unwrap();

    // Nothing else is inserted, so nothing should ever be evicted.
    assert!(store.contains(&digest));
}

/// A blob larger than the whole budget is served for the render that
/// uploaded it, but bypasses the cache — and does not evict anything else
/// to make room for itself.
#[test]
fn a_blob_larger_than_the_budget_bypasses_the_cache() {
    let store = BlobStore::new(10, 3, 32).unwrap();
    let small = sha256(b"small");
    store.put(&small, b"small").unwrap();

    let huge = vec![0u8; 100];
    store.put(&sha256(&huge), &huge).unwrap();

    assert!(store.contains(&small), "unrelated entry must be untouched");
    assert!(
        !store.contains(&sha256(&huge)),
        "oversized blob is never cached"
    );
}

/// When every candidate a sweep touches still has lives remaining, the
/// store accepts the new blob anyway rather than looping forever or
/// rejecting the write — a soft, self-correcting overshoot.
#[test]
fn soft_overshoot_is_accepted_when_nothing_is_evictable_yet() {
    let store = BlobStore::new(10, 5, 32).unwrap();
    let a = sha256(b"0123456789");
    store.put(&a, b"0123456789").unwrap();

    let b = sha256(b"aaaaaaaaaa");
    store.put(&b, b"aaaaaaaaaa").unwrap();

    // Neither insert panicked or errored, and both blobs are still present
    // (lives 5 -> 4 after one sweep touch, nowhere near zero).
    assert!(store.contains(&a));
    assert!(store.contains(&b));
}

#[tokio::test]
async fn mirror_path_is_none_for_an_uncached_digest() {
    let store = store();
    assert_eq!(store.mirror_path(&sha256(b"nope")).await.unwrap(), None);
}

#[tokio::test]
async fn mirror_path_materialises_once_and_is_stable_after() {
    let store = store();
    let digest = sha256(b"mirror me");
    store.put(&digest, b"mirror me").unwrap();

    let first = store.mirror_path(&digest).await.unwrap().unwrap();
    assert_eq!(tokio::fs::read(&first).await.unwrap(), b"mirror me");

    let second = store.mirror_path(&digest).await.unwrap().unwrap();
    assert_eq!(first, second, "repeat calls must return the same path");
}

#[tokio::test]
async fn evicting_a_mirrored_digest_removes_its_mirror_file() {
    let store = BlobStore::new(10, 1, 32).unwrap();
    let target = sha256(b"0123456789");
    store.put(&target, b"0123456789").unwrap();
    let path = store.mirror_path(&target).await.unwrap().unwrap();
    assert!(path.exists());

    // One sweep is enough: `mirror_path`'s own `get`-equivalent bumped
    // lives from 1 to 2, so this needs two sweeps to drain it.
    store.put(&sha256(b"aaaaaaaaaa"), b"aaaaaaaaaa").unwrap();
    store.put(&sha256(b"bbbbbbbbbb"), b"bbbbbbbbbb").unwrap();

    assert!(!store.contains(&target));
    assert!(!path.exists(), "mirror file must be cleaned up on eviction");
}

#[test]
fn a_pinned_entry_survives_indefinitely() {
    let store = BlobStore::new(10, 1, 32).unwrap();
    let target = sha256(b"0123456789");
    store.put(&target, b"0123456789").unwrap();
    assert!(store.pin(&target));

    // Many sweeps, each of which would otherwise drain and evict a
    // non-pinned single-life entry on the very first pass.
    for i in 0u8..10 {
        let bytes = vec![i; 10];
        store.put(&sha256(&bytes), &bytes).unwrap();
    }

    assert!(
        store.contains(&target),
        "a pinned entry must never be evicted"
    );
}

#[test]
fn pin_and_unpin_report_false_for_an_unknown_digest() {
    let store = store();
    let unknown = sha256(b"never uploaded");
    assert!(!store.pin(&unknown));
    assert!(!store.unpin(&unknown));
}

#[test]
fn pinned_bytes_tracks_pin_and_unpin() {
    let store = store();
    let a = sha256(b"aaa");
    let b = sha256(b"bb");
    store.put(&a, b"aaa").unwrap();
    store.put(&b, b"bb").unwrap();

    assert_eq!(store.pinned_bytes(), 0);
    store.pin(&a);
    assert_eq!(store.pinned_bytes(), 3);
    store.pin(&b);
    assert_eq!(store.pinned_bytes(), 5);
    store.unpin(&a);
    assert_eq!(store.pinned_bytes(), 2);
}

#[test]
fn pinned_bytes_is_unaffected_by_eviction_of_other_entries() {
    let store = BlobStore::new(10, 1, 32).unwrap();
    let pinned = sha256(b"0123456789");
    store.put(&pinned, b"0123456789").unwrap();
    store.pin(&pinned);

    let other = sha256(b"aaaaaaaaaa");
    store.put(&other, b"aaaaaaaaaa").unwrap();
    // `other` gets evicted on its own first sweep (single life, and the
    // pinned entry is never touched to give it a second chance).
    store.put(&sha256(b"bbbbbbbbbb"), b"bbbbbbbbbb").unwrap();

    assert_eq!(store.pinned_bytes(), 10);
}

/// Pinning past `max_bytes` logs a warning (not asserted here — no
/// tracing-capture test infrastructure exists elsewhere in this codebase to
/// mirror) but must never panic or reject the pin; `pinned_bytes` correctly
/// reflects the permanent overshoot.
#[test]
fn pinning_past_the_budget_still_pins() {
    let store = BlobStore::new(10, 3, 32).unwrap();
    let a = sha256(b"0123456789"); // 10 bytes — fills the whole budget alone
    store.put(&a, b"0123456789").unwrap();
    assert!(store.pin(&a));
    assert_eq!(store.pinned_bytes(), 10);

    // A second, smaller blob can't fit without evicting `a` — but `a` is
    // pinned, so it never bypasses the sweep to make room. Pin `b` too
    // (via a fresh store sized only for `a` alone, then over-committed by
    // this second pin) to exercise pinned_bytes exceeding max_bytes.
    let b = sha256(b"more");
    store.put(&b, b"more").unwrap();
    assert!(store.pin(&b));

    assert_eq!(store.pinned_bytes(), 14, "10 + 4 bytes, both pinned");
    assert!(
        store.pinned_bytes() > 10,
        "pinned total must be allowed to exceed max_bytes"
    );
}
