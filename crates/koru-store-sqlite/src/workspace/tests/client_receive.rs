// SPDX-License-Identifier: MPL-2.0

//! Actual Cloud carrier -> client receipt -> Store/SQLite, without disk/network.
use super::commit_retry::{MemoryWorkspace, connect};
use super::*;
use koru_device::cloud::{CloudId, CloudReplicaPrincipalV1 as Dataset};
use koru_replication::cloud::{
    Bytes, HandshakeV1, PreparedSession, Refusal, Version1,
    client_store::{self, Failure, Receipt},
    transfer::{self, Completed, MemoryBudget, Record, Scope, Stream, Unit, metadata},
};
use koru_store_core::{
    Locale, ProfileId, WorkspaceId,
    personal_attributes::{EntityRef, PersonalAttributeValue},
    ports::UpdateOrigin,
    profile_units::{IdentityManifest, PersonalManifest, PrivateManifest, ProfileUnitRef},
    service::Store,
};
use loro::{ExportMode, LoroDoc};

mod exports;

struct Fixture {
    store: Store<crate::SqliteCatalog, MemoryWorkspace>,
    control: Connection,
    uri: String,
    profile: ProfileId,
    workspace: WorkspaceId,
    manifest: DocumentId,
    document: DocumentId,
}
impl Fixture {
    fn new() -> Self {
        let uri = format!(
            "file:client-receive-{}?mode=memory&cache=shared",
            DocumentId::new()
        );
        let control = connect(&uri);
        let mut store = Store::open(
            crate::SqliteCatalog::in_memory().unwrap(),
            MemoryWorkspace(uri.clone()),
        )
        .unwrap();
        let profile = store
            .create_profile("Owner", None, None, Locale::default(), at())
            .unwrap()
            .id;
        let workspace = store
            .create_workspace(profile, "Workspace", at())
            .unwrap()
            .id;
        let document = store
            .create_document(workspace, "Document", "page", None, None, at())
            .unwrap()
            .0;
        let manifest = store.manifest_id(workspace).unwrap();
        store.flush(workspace, document, at()).unwrap();
        store.flush(workspace, manifest, at()).unwrap();
        store.take_committed();
        Self {
            store,
            control,
            uri,
            profile,
            workspace,
            manifest,
            document,
        }
    }
    fn scope(&self, profile: bool) -> Scope {
        let profile_id = CloudId::from_bytes(self.profile.as_uuid().into_bytes());
        let principal = if profile {
            Dataset::Profile {
                profile_id,
                lineage_id: CloudId::from_bytes([4; 16]),
                logical_replica_id: CloudId::from_bytes([5; 16]),
            }
        } else {
            Dataset::Workspace {
                workspace_id: CloudId::from_bytes(self.workspace.as_uuid().into_bytes()),
                incarnation_id: CloudId::from_bytes([6; 16]),
                logical_replica_id: CloudId::from_bytes([7; 16]),
            }
        };
        Scope {
            principal,
            eligibility_workspace: self.workspace,
            profile: profile_id,
            manifest: self.manifest,
        }
    }
    fn unit(&self, document: DocumentId) -> Unit {
        Unit::Document {
            workspace_id: CloudId::from_bytes(self.workspace.as_uuid().into_bytes()),
            document_id: CloudId::from_bytes(document.as_uuid().into_bytes()),
        }
    }
    fn payload(&mut self, document: DocumentId, large: bool) -> (Vec<u8>, LoroDoc) {
        let source = LoroDoc::new();
        source
            .import(&self.store.snapshot(self.workspace, document).unwrap())
            .unwrap();
        source.set_peer_id(981).unwrap();
        let version = source.oplog_vv();
        let text = if large {
            let mut state = 0x12345678u32;
            (0..350_000)
                .map(|_| {
                    state ^= state << 13;
                    state ^= state >> 17;
                    state ^= state << 5;
                    char::from(32 + (state % 95) as u8)
                })
                .collect::<String>()
        } else {
            "Cloud receipt".to_owned()
        };
        source.get_text("client-receive").insert(0, &text).unwrap();
        source.commit();
        let bytes = source.export(ExportMode::updates(&version)).unwrap();
        if large {
            assert!(bytes.len() > transfer::RESTRICTED_LIMIT);
        }
        (bytes, source)
    }
    fn deny_commit(&self) {
        self.control.execute_batch(
            "CREATE TRIGGER deny_client_commit BEFORE UPDATE OF version_vector ON loro_documents
             BEGIN SELECT RAISE(ABORT, 'injected client commit failure'); END;"
        ).unwrap();
    }
}

fn completed(scope: Scope, unit: Unit, bytes: Vec<u8>, budget: &MemoryBudget) -> Completed {
    let accepted = PreparedSession {
        principal: scope.principal,
        generation: 1,
        expires_at_seconds: 600,
        accepted: HandshakeV1::SessionAccepted {
            version: Version1,
            session_id: Bytes([1; 32]),
            generation: 1,
            expires_at_seconds: 600,
        },
        claim_references: koru_replication::cloud::transfer::ClaimReferences::None,
    };
    let mut sender = Stream::new(
        &accepted,
        scope,
        0,
        0,
        transfer::MAX_PAYLOAD,
        MemoryBudget::new(64 * 1024 * 1024),
    )
    .unwrap();
    let mut receiver = Stream::new(
        &accepted,
        scope,
        0,
        0,
        transfer::MAX_PAYLOAD,
        budget.clone(),
    )
    .unwrap();
    let sp = sender.initial_policy(0).unwrap();
    let rp = receiver.initial_policy(0).unwrap();
    sender.receive_control(rp, 0, 0, |_| Ok(())).unwrap();
    receiver.receive_control(sp, 0, 0, |_| Ok(())).unwrap();
    let (start, _) = sender
        .begin_outgoing(unit, [11; 8], bytes, 1, 1, |_| Ok(()))
        .unwrap();
    receiver.receive_control(start, 1, 1, |_| Ok(())).unwrap();
    while let Some(record) = sender.next_outgoing(2, |_| Ok(())).unwrap() {
        let encoded = transfer::encode_record(record.record()).unwrap();
        let Record::Peer(frame) = transfer::decode_record(encoded[4], &encoded[5..]).unwrap()
        else {
            panic!()
        };
        receiver.receive_peer(frame, 2, |_| Ok(())).unwrap();
    }
    receiver.handoff(3, |_| Ok(()), |done| done).unwrap()
}

fn assert_ack(scope: Scope, unit: Unit, receipt: &Receipt) {
    assert_eq!(receipt.unit(), unit);
    let Record::Peer(frame) = receipt.acknowledgement() else {
        panic!()
    };
    let values = metadata::decode(scope, frame.clone(), 1).unwrap();
    assert!(
        matches!(values.as_slice(), [metadata::Metadata::Ack { unit: actual, batch, ok: true }]
        if *actual == unit && *batch == [11; 8])
    );
}

#[test]
fn client_receive_durably_commits_document_and_manifest_before_returning_ack() {
    for manifest in [false, true] {
        let mut f = Fixture::new();
        let document = if manifest { f.manifest } else { f.document };
        let scope = f.scope(false);
        let unit = f.unit(document);
        let (bytes, source) = f.payload(document, !manifest);
        let budget = MemoryBudget::new(64 * 1024 * 1024);
        let done = completed(scope, unit, bytes.clone(), &budget);
        assert!(budget.used() >= bytes.len());
        assert!(f.store.take_committed().is_empty());
        let receipt =
            client_store::receive(&mut f.store, scope, done, at(), |_, _, _| Ok(())).unwrap();
        assert_ack(scope, unit, &receipt);
        assert!(receipt.durable_sequence().is_some());
        assert_eq!(budget.used(), 0);
        let repository = SqliteWorkspace::from_connection(connect(&f.uri)).unwrap();
        let rows = repository.updates_after(document, None).unwrap();
        assert_eq!(rows.last().unwrap().bytes, bytes);
        assert_eq!(rows.last().unwrap().origin, scope.principal.update_origin());
        assert_eq!(
            loro::VersionVector::decode(
                &repository
                    .document(document)
                    .unwrap()
                    .unwrap()
                    .version_vector
            )
            .unwrap(),
            source.oplog_vv()
        );
        let events = f.store.take_committed();
        assert!(
            events
                .iter()
                .any(|event| event.document == document && event.is_manifest == manifest)
        );
        let done = completed(scope, unit, bytes, &budget);
        let retry =
            client_store::receive(&mut f.store, scope, done, at(), |_, _, _| Ok(())).unwrap();
        assert_ack(scope, unit, &retry);
        assert_eq!(retry.durable_sequence(), None);
        assert!(f.store.take_committed().is_empty());
        assert_eq!(repository.updates_after(document, None).unwrap(), rows);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn client_receive_retains_original_budget_after_failed_store_commit_and_caller_disposal() {
    let mut f = Fixture::new();
    let scope = f.scope(false);
    let unit = f.unit(f.document);
    let (bytes, _) = f.payload(f.document, false);
    let budget = MemoryBudget::new(64 * 1024 * 1024);
    f.deny_commit();
    let done = completed(scope, unit, bytes.clone(), &budget);
    let charged = budget.used();
    assert!(matches!(
        client_store::receive(&mut f.store, scope, done, at(), |_, _, _| Ok(())),
        Err(Failure::Store(Code::StorageFailed))
    ));
    assert_eq!(
        budget.used(),
        charged,
        "the Store still owns the failed update"
    );
    assert!(f.store.take_committed().is_empty());
    let done = completed(scope, unit, bytes.clone(), &budget);
    assert!(budget.used() > charged);
    assert!(matches!(
        client_store::receive(&mut f.store, scope, done, at(), |_, _, _| Ok(())),
        Err(Failure::Store(Code::StorageFailed))
    ));
    assert_eq!(
        budget.used(),
        charged,
        "known retry must not double-retain memory"
    );
    f.control
        .execute_batch("DROP TRIGGER deny_client_commit")
        .unwrap();
    let done = completed(scope, unit, bytes, &budget);
    let receipt = client_store::receive(&mut f.store, scope, done, at(), |_, _, _| Ok(())).unwrap();
    assert_ack(scope, unit, &receipt);
    assert!(receipt.durable_sequence().is_some());
    assert_eq!(budget.used(), 0);

    let (bytes, _) = f.payload(f.document, false);
    f.deny_commit();
    let done = completed(scope, unit, bytes, &budget);
    assert!(client_store::receive(&mut f.store, scope, done, at(), |_, _, _| Ok(())).is_err());
    assert!(budget.used() > 0);
    drop(f.store);
    assert_eq!(
        budget.used(),
        0,
        "disposing the Store releases its retained pending bytes"
    );
}

#[test]
fn client_receive_profile_units_use_existing_atomic_catalog_commits() {
    let mut f = Fixture::new();
    let scope = f.scope(true);
    let profile_id = scope.profile;
    let states = f.store.profile_unit_states(f.profile).unwrap();
    let identity_state = states
        .iter()
        .find(|state| matches!(state.unit, ProfileUnitRef::Identity { .. }))
        .unwrap();
    let identity = IdentityManifest::from_snapshot(&identity_state.snapshot).unwrap();
    identity.set_name("Cloud name").unwrap();
    let private_state = states
        .iter()
        .find(|state| matches!(state.unit, ProfileUnitRef::Private { .. }))
        .unwrap();
    let private = PrivateManifest::from_snapshot(&private_state.snapshot).unwrap();
    private.add_dictionary_word("en", "cloudword").unwrap();
    let personal = PersonalManifest::create(f.profile, f.workspace).unwrap();
    let cases = [
        (
            Unit::Identity { profile_id },
            ProfileUnitRef::Identity { profile: f.profile },
            identity.export_snapshot().unwrap(),
        ),
        (
            Unit::Private { profile_id },
            ProfileUnitRef::Private { profile: f.profile },
            private.export_snapshot().unwrap(),
        ),
    ];
    for (unit, stored_unit, bytes) in cases {
        let budget = MemoryBudget::new(64 * 1024 * 1024);
        let source = LoroDoc::new();
        source.import(&bytes).unwrap();
        let done = completed(scope, unit, bytes, &budget);
        let receipt =
            client_store::receive(&mut f.store, scope, done, at(), |_, _, _| Ok(())).unwrap();
        assert_ack(scope, unit, &receipt);
        assert_eq!(
            receipt.durable_sequence(),
            None,
            "Profile commit does not use workspace sequences"
        );
        assert_eq!(budget.used(), 0);
        let row = f
            .store
            .profile_unit_states(f.profile)
            .unwrap()
            .into_iter()
            .find(|state| state.unit == stored_unit)
            .unwrap();
        let persisted = LoroDoc::new();
        persisted.import(&row.snapshot).unwrap();
        assert_eq!(persisted.oplog_vv(), source.oplog_vv());
        assert_eq!(persisted.get_deep_value(), source.get_deep_value());
    }

    // **The v1 personal address holds no unit since catalog 13** (Plan 1.1,
    // TECHNICAL.final §5.2 step 6). A *complete* v1 snapshot that still
    // arrives is admitted into the Workspace's own v2 unit through the
    // migration's PeerID normalization — so, unlike the two units above, the
    // bytes are not kept as they were and nothing is listed under
    // `ProfileUnitRef::Personal`; what has to survive is the content.
    let entity =
        EntityRef::new("node", f.workspace, f.document, DocumentId::new().as_uuid()).unwrap();
    let note = PersonalAttributeValue::Text {
        text: "legacy".into(),
    };
    personal
        .set_personal_attribute(&entity, "note", &note)
        .unwrap();
    let unit = Unit::Personal {
        profile_id,
        workspace_id: CloudId::from_bytes(f.workspace.as_uuid().into_bytes()),
    };
    let budget = MemoryBudget::new(64 * 1024 * 1024);
    let done = completed(scope, unit, personal.export_snapshot().unwrap(), &budget);
    let receipt = client_store::receive(&mut f.store, scope, done, at(), |_, _, _| Ok(())).unwrap();
    assert_ack(scope, unit, &receipt);
    assert_eq!(
        receipt.durable_sequence(),
        None,
        "Profile commit does not use workspace sequences"
    );
    assert_eq!(budget.used(), 0);
    assert!(
        f.store
            .profile_unit_states(f.profile)
            .unwrap()
            .iter()
            .all(|state| !matches!(state.unit, ProfileUnitRef::Personal { .. })),
        "a personal unit was listed on the retired v1 address"
    );
    assert_eq!(
        f.store
            .personal_attributes(f.profile, &entity)
            .unwrap()
            .get("note"),
        Some(&note),
        "the legacy snapshot was admitted into the Workspace's v2 unit"
    );

    assert_eq!(f.store.profile(f.profile).unwrap().name, "Cloud name");
    assert_eq!(
        f.store.personal_dictionary(f.profile).unwrap()["en"],
        vec!["cloudword"]
    );
}

#[test]
fn client_receive_rejects_invalid_profile_content_without_ack_or_catalog_mutation() {
    let mut f = Fixture::new();
    let scope = f.scope(true);
    let unit = Unit::Identity {
        profile_id: scope.profile,
    };
    let before = f
        .store
        .profile_unit_version(ProfileUnitRef::Identity { profile: f.profile })
        .unwrap();
    let budget = MemoryBudget::new(64 * 1024 * 1024);
    let state = f
        .store
        .profile_unit_states(f.profile)
        .unwrap()
        .into_iter()
        .find(|state| matches!(state.unit, ProfileUnitRef::Identity { .. }))
        .unwrap();
    let source = LoroDoc::new();
    source.import(&state.snapshot).unwrap();
    source.set_peer_id(982).unwrap();
    source
        .get_map("meta")
        .insert("profile_id", ProfileId::new().to_storage())
        .unwrap();
    source.commit();
    for bytes in [
        vec![1, 2, 3],
        source.export(ExportMode::all_updates()).unwrap(),
    ] {
        let done = completed(scope, unit, bytes, &budget);
        assert!(matches!(
            client_store::receive(&mut f.store, scope, done, at(), |_, _, _| Ok(())),
            Err(Failure::Store(_))
        ));
        assert_eq!(
            f.store
                .profile_unit_version(ProfileUnitRef::Identity { profile: f.profile })
                .unwrap(),
            before
        );
        assert_eq!(f.store.profile(f.profile).unwrap().name, "Owner");
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn client_receive_refuses_current_authority_wrong_scope_origin_and_missing_ancestors() {
    let mut f = Fixture::new();
    let scope = f.scope(false);
    let unit = f.unit(f.document);
    let before = f.store.version(f.workspace, f.document).unwrap();
    let (bytes, source) = f.payload(f.document, false);
    let budget = MemoryBudget::new(64 * 1024 * 1024);
    let done = completed(scope, unit, bytes.clone(), &budget);
    let mut called = false;
    let result = client_store::receive(&mut f.store, scope, done, at(), |_, actual, target| {
        called = true;
        assert_eq!(actual.principal, scope.principal);
        assert_eq!(target, unit);
        Err(Refusal::CloudDisabled)
    });
    assert!(called);
    assert!(matches!(
        result,
        Err(Failure::Transfer(transfer::Error::Authority(
            Refusal::CloudDisabled
        )))
    ));
    let mut done = completed(scope, unit, bytes.clone(), &budget);
    done.origin = UpdateOrigin::Store;
    assert!(matches!(
        client_store::receive(&mut f.store, scope, done, at(), |_, _, _| panic!(
            "invalid origin"
        )),
        Err(Failure::Transfer(transfer::Error::Invalid))
    ));
    let done = completed(scope, unit, bytes, &budget);
    let wrong = Scope {
        manifest: DocumentId::new(),
        ..scope
    };
    assert!(matches!(
        client_store::receive(&mut f.store, wrong, done, at(), |_, _, _| Ok(())),
        Err(Failure::Transfer(transfer::Error::Invalid))
    ));

    let ancestor = source.oplog_vv();
    source
        .get_text("client-receive")
        .insert(0, "needs missing ancestor ")
        .unwrap();
    source.commit();
    let child = source.export(ExportMode::updates(&ancestor)).unwrap();
    let done = completed(scope, unit, child, &budget);
    assert!(matches!(
        client_store::receive(&mut f.store, scope, done, at(), |_, _, _| Ok(())),
        Err(Failure::Store(Code::CrdtRejected))
    ));
    assert_eq!(f.store.version(f.workspace, f.document).unwrap(), before);
    assert!(f.store.take_committed().is_empty());
    assert_eq!(budget.used(), 0);
}

/// **The origin-off promise for Profile identity.** A device that has never
/// held the unit — signed in, Profile row provisioned from the account, no
/// identity bytes — must be able to receive one from the Cloud and become a
/// complete Profile. This is the rung a fresh install stands on, and PLAN19
/// round five needed it pinned because three separate hypotheses said it was
/// the broken one. It is not: the `None` branch of `apply_profile_unit_updates`
/// reconstructs the whole document from the worker's snapshot and commits it.
#[test]
fn client_receive_gives_a_device_its_first_profile_identity_unit() {
    let mut f = Fixture::new();
    let scope = f.scope(true);
    let unit = Unit::Identity {
        profile_id: scope.profile,
    };
    // The authority, as another device wrote it and the Cloud retains it.
    f.store
        .rename_profile(f.profile, "Renamed elsewhere")
        .unwrap();
    let authority = f
        .store
        .profile_unit_snapshot(ProfileUnitRef::Identity { profile: f.profile })
        .unwrap();
    let authored = LoroDoc::new();
    authored.import(&authority).unwrap();

    // A second installation of the same account: the Profile row exists as a
    // presence placeholder and nothing has authored its lineage.
    let mut fresh = Store::open(
        crate::SqliteCatalog::in_memory().unwrap(),
        MemoryWorkspace(format!(
            "file:client-receive-fresh-{}?mode=memory&cache=shared",
            DocumentId::new()
        )),
    )
    .unwrap();
    fresh
        .provision_account_profile(
            f.profile,
            koru_store_core::ProfileAccountBinding::parse_identity_id(
                "10000000-0000-4000-8000-000000000001",
            )
            .unwrap(),
            "Placeholder",
            Locale::default(),
            at(),
        )
        .unwrap();
    // What the device advertises with nothing held, and therefore why the
    // worker answers with a snapshot rather than a delta.
    assert!(
        client_store::version(&mut fresh, scope, unit, at(), |_, _, _| Ok(()))
            .unwrap()
            .is_empty()
    );

    let budget = MemoryBudget::new(64 * 1024 * 1024);
    let done = completed(scope, unit, authority.clone(), &budget);
    let receipt = client_store::receive(&mut fresh, scope, done, at(), |_, _, _| Ok(())).unwrap();
    assert_ack(scope, unit, &receipt);
    assert_eq!(budget.used(), 0);

    let held = fresh
        .profile_unit_snapshot(ProfileUnitRef::Identity { profile: f.profile })
        .unwrap();
    let persisted = LoroDoc::new();
    persisted.import(&held).unwrap();
    assert_eq!(persisted.oplog_vv(), authored.oplog_vv());
    // The placeholder's invented name and creation time are replaced by the
    // delivered lineage's, not merged with them.
    assert_eq!(
        IdentityManifest::from_snapshot(&held)
            .unwrap()
            .profile()
            .unwrap(),
        IdentityManifest::from_snapshot(&authority)
            .unwrap()
            .profile()
            .unwrap()
    );
    assert_eq!(fresh.profile(f.profile).unwrap().name, "Renamed elsewhere");
    // Editing now works, which is what "complete Profile" means to a person.
    fresh.rename_profile(f.profile, "Edited here").unwrap();
}

/// **The defect PLAN19 round five found.** `IdentityManifest::create` ran twice
/// for one Profile id, on two installations, and the Cloud now holds both
/// lineages merged. A device holding one of them is offered the other.
///
/// It **merges**, and that is deliberate. Refusing was self-sealing: each side
/// rejected the other in both directions forever, only a device with no copy
/// could escape, and — because one refused unit ends the whole dataset pass —
/// a disagreement about a *name* froze the private and personal units beside
/// it on every pass. Merging costs a label a person can retype, drops nothing
/// (a Profile's Workspace list is not in this document), and converges
/// everywhere it reaches.
///
/// The divergence is still reported, loudly and with both values, because two
/// lineages mean a device minted a manifest for a Profile it did not
/// originate. This import is the only place that can observe that.
#[test]
fn client_receive_merges_a_second_identity_lineage_rather_than_sealing_itself() {
    let mut f = Fixture::new();
    let scope = f.scope(true);
    let unit = Unit::Identity {
        profile_id: scope.profile,
    };
    let held = ProfileUnitRef::Identity { profile: f.profile };
    let before_version = f.store.profile_unit_version(held).unwrap();
    let before_snapshot = f.store.profile_unit_snapshot(held).unwrap();

    // A second installation authored its own lineage for the same Profile id,
    // stamping its own creation time, and it reached the Cloud. Its peer is
    // pinned so the register conflict resolves the same way every run: which
    // side of a live divergence wins is arbitrary, and the device on the
    // losing side is the one that can never accept another edit again.
    let foreign = LoroDoc::new();
    foreign.set_peer_id(u64::MAX - 1).unwrap();
    let meta = foreign.get_map("meta");
    meta.insert("schema_version", 1_i64).unwrap();
    meta.insert("profile_id", f.profile.to_storage()).unwrap();
    meta.insert("name", "Same Profile, other lineage").unwrap();
    meta.insert("locale", "und").unwrap();
    meta.insert("created_at", at().millis() + 1).unwrap();
    meta.insert("kind", "normal").unwrap();
    foreign.commit();
    // It is a perfectly valid identity for this exact Profile on its own —
    // which is why no scope check has anything to say about it.
    let foreign_bytes = foreign.export(ExportMode::snapshot()).unwrap();
    assert_eq!(
        IdentityManifest::from_snapshot(&foreign_bytes)
            .unwrap()
            .unit(),
        held
    );
    let cloud = LoroDoc::new();
    cloud.import(&before_snapshot).unwrap();
    cloud.import(&foreign_bytes).unwrap();
    let created_at = |doc: &LoroDoc| {
        IdentityManifest::from_snapshot(&doc.export(ExportMode::snapshot()).unwrap())
            .unwrap()
            .profile()
            .unwrap()
            .created_at
    };
    let local = LoroDoc::new();
    local.import(&before_snapshot).unwrap();
    assert_ne!(
        created_at(&cloud),
        created_at(&local),
        "the merge must actually move created_at, or this pins nothing"
    );
    // Exactly what the worker exports for a device advertising what it holds:
    // updates since a version the device really has, of a document that really
    // contains that version. Neither end of the transfer is wrong.
    let payload = cloud.export(ExportMode::updates(&before_version)).unwrap();

    let budget = MemoryBudget::new(64 * 1024 * 1024);
    let done = completed(scope, unit, payload, &budget);
    client_store::receive(&mut f.store, scope, done, at(), |_, _, _| Ok(()))
        .expect("a second lineage merges; refusing it strands this device forever");

    // The merge really happened: the held unit advanced rather than being left
    // where it was, which is the difference between accepting and ignoring.
    assert_ne!(
        f.store.profile_unit_version(held).unwrap(),
        before_version,
        "the unit did not move, so nothing was merged"
    );
    // And what is left is a valid identity for *this* Profile. Which side of
    // the divergence won the name is arbitrary and deliberately not asserted:
    // a person retypes it, and pinning it here would pin Loro's tie-break
    // rather than our behaviour.
    let merged = IdentityManifest::from_snapshot(&f.store.profile_unit_snapshot(held).unwrap())
        .expect("the merged identity must still be readable");
    assert_eq!(merged.unit(), held);
    assert_eq!(merged.profile().unwrap().id, f.profile);
    assert_eq!(budget.used(), 0);
}
