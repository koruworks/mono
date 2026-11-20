// SPDX-License-Identifier: MPL-2.0
//! What may be seen where: redaction, the credential slot's isolation, and
//! the fences that keep a test of this crate away from a real keychain.
use super::*;
use koru_oauth_credentials::{CredentialStore, SecretString};

const PRIVATE: [&str; 3] = ["SECRET", "MARK", "consent?state"];

fn clean(printed: &str) {
    for word in PRIVATE {
        assert!(!printed.contains(word), "{word} in {printed}");
    }
}

#[test]
fn m5_redaction_debug_of_every_type_that_can_hold_a_secret_or_content() {
    let secret = || SecretString::new("SECRET-value").unwrap();
    let access = AccessToken {
        secret: secret(),
        expires_at: Instant::now(),
    };
    let reference = VendorItemRef::new("MARK-REF").unwrap();
    let picked = PickedItem {
        reference: reference.clone(),
        identity: ItemIdentityDigest([7; 32]),
        title: "MARK-TITLE".into(),
    };
    let consent = AuthorizationRequest::new("https://vendor.example/consent?state=SECRET-state");
    let content = || ItemContent {
        text: "MARK-CONTENT".into(),
        truncated: false,
        total_bytes: 12,
    };
    let printed = [
        format!("{:?}", secret()),
        format!("{access:?}"),
        format!("{reference:?}"),
        format!("{picked:?}"),
        format!("{consent:?}"),
        format!(
            "{:?}",
            AuthorizationCallback {
                state: "SECRET-state".into(),
                code: Some(secret()),
            }
        ),
        format!(
            "{:?}",
            AuthorizedAccount {
                subject: "MARK-SUBJECT".into(),
                label: "MARK-LABEL".into(),
                scopes: vec!["MARK-SCOPE".into()],
                refresh: secret(),
                access: access.clone(),
                items: vec![picked.clone()],
            }
        ),
        format!(
            "{:?}",
            RefreshedAccess {
                access: access.clone(),
                rotated_refresh: Some(secret()),
            }
        ),
        format!("{:?}", content()),
        format!("{:?}", PrivateReadContent::new(content())),
        format!(
            "{:?}",
            ReadOutcome::Delivered(PrivateReadContent::new(content()))
        ),
        format!(
            "{:#?}",
            TripStart::Consent {
                request: BotRequestId::mint(),
                consent: consent.clone(),
            }
        ),
        format!("{:?}", ItemIdentityDigest([7; 32])),
    ]
    .join("\n");
    clean(&printed);
    // The sizes a caller needs are there; the text is not.
    assert!(printed.contains("bytes: 12"));
    // The credential scope prints nothing of the root, device or origin.
    let h = Harness::new();
    let connection = h.connect("Work", SUBJECT_A, &["doc-1"]);
    let effect = h.open(connection.id, "doc-1");
    let outcome = h.read(&effect);
    let printed = format!(
        "{:?} {:?} {:?} {outcome:?} {:?} {:?} {:?}",
        h.service,
        h.context,
        h.slot(connection.id),
        h.connection(connection.id),
        h.items(connection.id),
        h.changes.all(),
    );
    clean(&printed);
    assert!(!printed.contains("isolated"), "{printed}");
    assert!(!printed.contains("vendor.example"), "{printed}");
    // Every vendor failure is one fixed word with no text from a vendor.
    for error in [
        VendorError::NotConfigured,
        VendorError::ConsentDenied,
        VendorError::InvalidGrant,
        VendorError::Unauthorized,
        VendorError::Forbidden,
        VendorError::NotFound,
        VendorError::IdentityChanged,
        VendorError::RateLimited,
        VendorError::Rejected,
        VendorError::Unavailable,
        VendorError::Unconfirmed,
    ] {
        let text = error.to_string();
        assert!(
            text.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'),
            "{text}"
        );
    }
}

/// A journey through every flow, checking the observations the service
/// exposes to its ports. Persistence belongs to a future runtime adapter.
#[test]
fn m5_redaction_a_whole_journey_exposes_no_secret_or_content() {
    let recorded = {
        let h = Harness::new();
        h.vendor.set_rotation(true);
        let work = h.connect("Work", SUBJECT_A, &["doc-1", "doc-2"]);
        // A granted read, a refresh with rotation, a refused read.
        let effect = h.open(work.id, "doc-1");
        assert_eq!(Harness::text(h.read(&effect)), CONTENT_1);
        h.expire_access();
        let effect = h.open(work.id, "doc-2");
        assert_eq!(Harness::text(h.read(&effect)), "MARK-CONTENT two");
        h.vendor.transfer("doc-1", SUBJECT_B);
        let effect = h.open(work.id, "doc-1");
        assert!(matches!(h.read(&effect), ReadOutcome::Refused(_)));
        // A wrong account, a duplicate, a canceled connect.
        let (request, consent) = h.begin("Second");
        h.service
            .complete_trip(h.owner, request, h.vendor.approve(&consent, SUBJECT_A, &[]))
            .unwrap();
        let (request, _) = h.begin("Third");
        h.service.cancel_trip(h.owner, request).unwrap();
        // The credential lapses; the person signs in again; then disconnects.
        h.vendor.invalidate(SUBJECT_A);
        h.expire_access();
        let effect = h.open(work.id, "doc-2");
        assert!(matches!(h.read(&effect), ReadOutcome::Refused(_)));
        let lapsed = h.connection(work.id);
        for subject in [SUBJECT_B, SUBJECT_A] {
            let request = BotRequestId::mint();
            let revision = h.connection(work.id).revision;
            let TripStart::Consent { consent, .. } = h
                .service
                .begin_sign_in_again(h.owner, request, work.id, revision, REDIRECT)
                .unwrap()
            else {
                panic!("the sign-in did not begin")
            };
            h.service
                .complete_trip(h.owner, request, h.vendor.approve(&consent, subject, &[]))
                .unwrap();
        }
        assert_eq!(h.connection(work.id).revision, lapsed.revision + 1);
        let revision = h.connection(work.id).revision;
        let done = h
            .service
            .disconnect(h.owner, BotRequestId::mint(), work.id, revision)
            .unwrap();
        assert_eq!(done.connection.state, ConnectionState::Disconnected);
        // The vendor issued secrets; all of them are marked.
        let secrets = h.vendor.secrets();
        assert!(secrets.len() >= 6);
        assert!(secrets.iter().all(|s| s.starts_with("SECRET-")));
        // Nothing is left in any credential slot after the Disconnect.
        assert_eq!(h.stored(work.id), None);
        format!(
            "{:?}\n{:?}\n{:?}\n{:?}\n{:?}",
            h.journal.entries(),
            h.changes.all(),
            h.ledger.advances(),
            h.credentials.touched(),
            done,
        )
    };
    // What the fakes saw of the service: calls, codes, ids, sizes.
    for word in [
        "SECRET",
        "MARK-CONTENT",
        "MARK-TITLE",
        "MARK-LABEL",
        "doc-1",
    ] {
        assert!(!recorded.contains(word), "{word} in {recorded}");
    }
}

#[test]
fn m5_credential_slot_is_per_connection_and_never_the_slot_of_another_owner() {
    let h = Harness::new();
    // The slot a document importer signed in to the same vendor origin
    // uses: the bare entry for that origin. It holds its own token.
    let importer = h.context.entry(FAKE_ORIGIN).unwrap();
    h.credentials
        .store(&importer, SecretString::new("SECRET-importer").unwrap())
        .unwrap();
    let touched_before = h.credentials.touched().len();

    let first = h.connect("Work", SUBJECT_A, &["doc-1"]);
    let second = h.connect("Other", SUBJECT_B, &["doc-b"]);
    // One slot per connection, none of them the importer's.
    let slots = [h.slot(first.id), h.slot(second.id)];
    assert_ne!(slots[0], slots[1]);
    assert_ne!(slots[0], importer);
    assert_ne!(slots[1], importer);
    assert!(slots[0].account().starts_with("provider-v1:"));
    for slot in &slots {
        // The slot's name carries no connection id, origin or account.
        for private in [
            first.id.to_storage(),
            second.id.to_storage(),
            "vendor.example".to_owned(),
            SUBJECT_A.to_owned(),
            "koru-bots".to_owned(),
        ] {
            assert!(
                !slot.account().contains(&private),
                "{private} in a slot name"
            );
        }
    }
    // The same connection id under another root, channel or device is
    // another slot.
    assert_ne!(
        connection_slot(
            &context("/isolated/second-root", h.owner),
            FAKE_ORIGIN,
            first.id
        )
        .unwrap(),
        slots[0]
    );
    assert_ne!(
        connection_slot(&h.context, "https://other.example", first.id).unwrap(),
        slots[0]
    );

    // Every flow that touches a credential: a refresh with rotation, a
    // sign-in again as the wrong and as the right account, a disconnect
    // with a revoke.
    h.vendor.set_rotation(true);
    h.expire_access();
    let effect = h.open(first.id, "doc-1");
    assert_eq!(Harness::text(h.read(&effect)), CONTENT_1);
    h.vendor.invalidate(SUBJECT_A);
    h.expire_access();
    let effect = h.open(first.id, "doc-1");
    assert!(matches!(h.read(&effect), ReadOutcome::Refused(_)));
    for subject in [SUBJECT_B, SUBJECT_A] {
        let request = BotRequestId::mint();
        let revision = h.connection(first.id).revision;
        let TripStart::Consent { consent, .. } = h
            .service
            .begin_sign_in_again(h.owner, request, first.id, revision, REDIRECT)
            .unwrap()
        else {
            panic!("the sign-in did not begin")
        };
        h.service
            .complete_trip(h.owner, request, h.vendor.approve(&consent, subject, &[]))
            .unwrap();
    }
    for id in [first.id, second.id] {
        let revision = h.connection(id).revision;
        h.service
            .disconnect(h.owner, BotRequestId::mint(), id, revision)
            .unwrap();
    }
    assert_eq!(h.service.recover().unwrap(), Recovered::default());

    // Through all of it the importer's slot was never read, written or
    // cleared, and it still holds exactly what it held.
    let touched = h.credentials.touched();
    assert!(touched.len() > touched_before + 6, "{}", touched.len());
    for (operation, account) in &touched[touched_before..] {
        assert_ne!(
            account,
            importer.account(),
            "{operation} on the importer's slot"
        );
        assert!(
            slots.iter().any(|slot| slot.account() == account),
            "{operation} on a slot no connection owns"
        );
    }
    assert_eq!(
        h.credentials.peek(&importer).unwrap().expose(),
        "SECRET-importer"
    );
    // And both connections' own slots are empty after their Disconnect.
    assert_eq!((h.stored(first.id), h.stored(second.id)), (None, None));
}

/// The files of this crate, read as text: the manifest and every source.
const MANIFEST: &str = include_str!("../../Cargo.toml");
const SOURCES: [(&str, &str); 6] = [
    ("lib.rs", include_str!("../lib.rs")),
    ("identity.rs", include_str!("../identity.rs")),
    ("ports.rs", include_str!("../ports.rs")),
    ("service.rs", include_str!("../service.rs")),
    ("vendor.rs", include_str!("../vendor.rs")),
    ("support.rs", include_str!("../support.rs")),
];

#[test]
fn m5_isolation_no_test_can_reach_an_os_keychain_and_no_source_names_a_vendor() {
    // A dependency is never compiled with `cfg(test)`, so the fence is the
    // manifest: this crate depends on the credential port without the
    // feature that links an OS keychain, and names no keychain crate.
    let code: Vec<&str> = MANIFEST
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#') && !line.is_empty())
        .collect();
    assert!(code.contains(&r#"koru-oauth-credentials = { path = "../koru-oauth-credentials" }"#));
    for line in &code {
        assert!(!line.contains("desktop"), "{line}");
        assert!(!line.contains("keyring"), "{line}");
        assert!(
            !line.contains("reqwest") && !line.contains("oauth2"),
            "{line}"
        );
    }
    for (name, source) in SOURCES {
        // No source constructs or names the OS store, opens a socket, or
        // runs a process.
        for forbidden in [
            "OsCredentialStore",
            "keyring",
            "TcpListener",
            "TcpStream",
            "UdpSocket",
            "reqwest",
            "std::process",
            "std::fs",
        ] {
            assert!(!source.contains(forbidden), "{forbidden} in {name}");
        }
        // No vendor or product is named: the provider token is data, kept
        // in one list outside this crate.
        let lower = source.to_lowercase();
        for vendor in [
            "google",
            "gdrive",
            "github",
            "stripe",
            "dropbox",
            "notion",
            "microsoft",
        ] {
            assert!(!lower.contains(vendor), "{vendor} in {name}");
        }
    }
    // The stores this crate's tests use are in memory.
    let h = Harness::new();
    let connection = h.connect("Work", SUBJECT_A, &["doc-1"]);
    assert!(h.stored(connection.id).is_some());
    let empty = LockableCredentials::default();
    assert!(empty.peek(&h.slot(connection.id)).is_none());
}
