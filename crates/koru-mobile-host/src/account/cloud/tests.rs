// SPDX-License-Identifier: MPL-2.0

//! The phone's half of *live in the account, in the Trash here*
//! (trash-recovery PLAN.final P2): there is no control socket on a phone, so
//! the coordinator's question is answered by this device's own Store, in
//! process, through the same call the desktop gateway makes.

use std::collections::BTreeSet;

use koru_store_core::ProfileAccountBinding;
use koru_store_core::text::Locale;

use super::*;

fn at() -> Timestamp {
    Timestamp::from_millis(1_770_000_000_000)
}

#[test]
fn the_account_live_question_is_answered_by_this_devices_store_in_process() {
    let root = tempfile::tempdir().expect("a private root");
    let host = MobileHost::open(root.path().to_path_buf(), [91; 64], "Test", false)
        .expect("a fresh host opens");
    let account: KoruIdentityId = "30000000-0000-4000-8000-000000000001".parse().unwrap();
    let account_uuid = account.as_uuid();

    // A workspace trashed under a Profile nobody opened to sync — its
    // deletion cannot leave this device — and a Profile of the account.
    let (workspace, kept) = host
        .store
        .with_store(move |store| {
            let quiet = store
                .create_profile("Quiet", None, None, Locale::default(), at())
                .unwrap();
            let workspace = store.create_workspace(quiet.id, "Stuck", at()).unwrap();
            store.delete_workspace(workspace.id, at()).unwrap();
            let kept = store
                .create_profile("Kept", None, None, Locale::default(), at())
                .unwrap();
            let binding =
                ProfileAccountBinding::parse_identity_id(&account_uuid.to_string()).unwrap();
            store.bind_profile_account(kept.id, None, binding).unwrap();
            (workspace.id, kept.id)
        })
        .expect("the Store worker answers");

    let cloud = MobileCloudHost::new(
        Arc::downgrade(&host),
        Region::Eu,
        "https://issuer.invalid".into(),
        "https://api.invalid".into(),
    );

    let trashed = Host::reconcile_account_live(&cloud, account, &[(workspace, kept)])
        .expect("the Store says what is in its Trash");
    assert_eq!(trashed, BTreeSet::from([workspace]), "admitted for nothing");
    let listed = host
        .store
        .with_store(|store| store.trashed_in_account().unwrap())
        .unwrap();
    assert_eq!(listed.len(), 1, "and the Store keeps the fact");
    assert_eq!(
        (listed[0].workspace.id, listed[0].profile),
        (workspace, kept)
    );

    // The account stops listing it: nothing to drop, and the fact goes.
    let trashed = Host::reconcile_account_live(&cloud, account, &[]).unwrap();
    assert!(trashed.is_empty());
    assert!(
        host.store
            .with_store(|store| store.trashed_in_account().unwrap())
            .unwrap()
            .is_empty()
    );
}
