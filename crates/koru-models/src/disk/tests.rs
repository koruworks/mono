// SPDX-License-Identifier: MPL-2.0

//! The disk store's own suite is small on purpose: the store suite that
//! predates the extraction — atomic publish, the per-digest lock, leases,
//! prune, remove, crash recovery, the trust policy — lives in
//! `koru-voice-models/src/disk/tests.rs` and now runs against this code
//! through voice's typed store. What is here is what is new: **a store opens
//! only for its own feature's consent.**
//!
//! The two refusals touch no disk: consent is the first gate, before the
//! signature and before any directory is read or made, so a store whose root
//! does not exist answers it. The third goes past the gate, so it takes the
//! install lock, and does that in a temporary directory.

use super::*;
use crate::fixtures::{self, Signer};
use koru_voice_consent::{READ, VOICE};
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

/// A source that counts, to prove a refusal fetched nothing.
struct Counting(Arc<AtomicUsize>);

impl ModelSource for Counting {
    fn fetch(
        &self,
        _url: &url::Url,
        _descriptor: &ModelDescriptor,
        _destination: &Path,
        _progress: &mut dyn FnMut(InstallProgress),
        _cancel: &AtomicBool,
    ) -> Result<(), ModelError> {
        self.0.fetch_add(1, Ordering::AcqRel);
        Err(ModelErrorCode::Offline.with("counted"))
    }
}

fn store(consent: ConsentFile, calls: &Arc<AtomicUsize>) -> DiskModelStore {
    DiskModelStore::with_trust(
        ModelPaths::at("/koru-models-test/does-not-exist"),
        Box::new(Counting(Arc::clone(calls))),
        // The arm a nightly uses: if consent were not checked first, this is
        // the policy under which a forged-looking entry would get furthest.
        TrustPolicy::digest_only_on(Some("nightly")).unwrap(),
        consent,
    )
}

/// **A voice grant does not open the reader's store**, even for an entry whose
/// licence and size it would cover, and nothing is fetched.
#[test]
fn a_voice_grant_does_not_open_a_reader_store() {
    let entry = fixtures::entry(&Signer::new(), fixtures::descriptor());
    let voice = VOICE.recorded(entry.terms(), 1);
    assert!(voice.covers(&entry.terms()), "the premise");
    let calls = Arc::new(AtomicUsize::new(0));

    let error = store(READ, &calls)
        .install(
            &entry,
            TrustedKeys::pinned(),
            &voice,
            &mut |_| {},
            &AtomicBool::new(false),
        )
        .unwrap_err();

    assert_eq!(error.code, ModelErrorCode::ConsentRequired);
    assert!(
        error.message.contains("voice-consent.conf"),
        "{}",
        error.message
    );
    assert_eq!(calls.load(Ordering::Acquire), 0, "it fetched anyway");
}

/// The mirror: a reader grant does not open voice's store.
#[test]
fn a_reader_grant_does_not_open_a_voice_store() {
    let entry = fixtures::entry(&Signer::new(), fixtures::descriptor());
    let reader = READ.recorded(entry.terms(), 1);
    let calls = Arc::new(AtomicUsize::new(0));

    let error = store(VOICE, &calls)
        .install(
            &entry,
            TrustedKeys::pinned(),
            &reader,
            &mut |_| {},
            &AtomicBool::new(false),
        )
        .unwrap_err();

    assert_eq!(error.code, ModelErrorCode::ConsentRequired);
    assert_eq!(calls.load(Ordering::Acquire), 0);
}

/// And the store's own record's grant passes the gate — the refusal above is
/// about whose grant it is, not a store that refuses everything. Past consent,
/// the counting source is reached and answers `offline`.
#[test]
fn a_grant_from_the_stores_own_record_passes_the_gate() {
    let entry = fixtures::entry(&Signer::new(), fixtures::descriptor());
    let calls = Arc::new(AtomicUsize::new(0));
    let reader_store = store(READ, &calls);
    assert_eq!(reader_store.consent(), READ);
    let tmp = tempfile::tempdir().unwrap();
    let rooted = DiskModelStore::with_trust(
        ModelPaths::at(tmp.path().join("cache")),
        Box::new(Counting(Arc::clone(&calls))),
        TrustPolicy::digest_only_on(Some("nightly")).unwrap(),
        READ,
    );

    let error = rooted
        .install(
            &entry,
            TrustedKeys::pinned(),
            &READ.recorded(entry.terms(), 1),
            &mut |_| {},
            &AtomicBool::new(false),
        )
        .unwrap_err();

    assert_eq!(error.code, ModelErrorCode::Offline);
    assert_eq!(calls.load(Ordering::Acquire), 1);
}
