// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_ai_core::{AiProviderId, ChatId, DocumentId, ModelSelection, ReasoningChoice, Timestamp};
use koru_ai_sqlite::Repository;
use koru_delegate_core::RoutineDigest32;
use koru_store_server::memory::MemoryStore;
use std::sync::Arc;

pub(crate) struct Fixture {
    pub memory: MemoryStore,
    pub adapter: Arc<BotAuthorityAdapter>,
    pub owner: BotOwner,
    pub profile: ProfileId,
    pub other: ProfileId,
    pub bot: Bot,
    pub creation: BotRoutineCreation,
    pub command: BotRoutineCreate,
}
impl Fixture {
    pub(crate) fn new() -> (Self, Repository) {
        let memory = MemoryStore::open("routine-store-fixture").unwrap();
        let (profile, other, home, role) = memory
            .with_store(|store| {
                let at = Timestamp::from_millis(1);
                let profile = store
                    .create_profile("Owner", None, None, Default::default(), at)
                    .unwrap()
                    .id;
                let other = store
                    .create_profile("Other", None, None, Default::default(), at)
                    .unwrap()
                    .id;
                let home = store.create_workspace(profile, "Home", at).unwrap().id;
                let (role, _) = store
                    .create_document(
                        home,
                        "Role",
                        koru_core::explore::KIND_DOCUMENT,
                        None,
                        None,
                        at,
                    )
                    .unwrap();
                (profile, other, home, role)
            })
            .unwrap();
        let adapter = Arc::new(
            BotAuthorityAdapter::from_device(memory.client(), &uuid::Uuid::new_v4().to_string())
                .unwrap(),
        );
        let owner = adapter.owner().unwrap();
        let bot = Bot {
            id: BotId::mint(),
            home_workspace: home,
            user_scope: owner.user_scope,
            owner_device: owner.device,
            config: BotConfig {
                name: "Fixture".into(),
                role: BotPage {
                    workspace: home,
                    document: role,
                },
                working_folder: if cfg!(windows) {
                    "D:/isolated"
                } else {
                    "/isolated"
                }
                .into(),
                model: ModelSelection {
                    provider: AiProviderId::parse("agent-codex").unwrap(),
                    requested_model: "fixture".into(),
                    default_reasoning: ReasoningChoice::Default,
                },
                mode: None,
                fast: None,
            },
            access: BotAccess::Default,
            selected_pages: vec![],
            revision: 1,
            policy_revision: 1,
            main_chat: ChatId::new(),
            paused: true,
            folder_lock_request: None,
            folder_frozen_at_ms: None,
            created_at_ms: 1,
            updated_at_ms: 1,
            lifecycle: Default::default(),
            open_operation: None,
            held_by: None,
            pending_change: None,
        };
        let mut repository = Repository::in_memory().unwrap();
        repository
            .save_bot(&bot, profile, BotRequestId::mint(), &"a".repeat(64), None)
            .unwrap();
        let command: BotRoutineCreate = serde_json::from_value(serde_json::json!({
            "request":BotRequestId::mint(), "bot":bot.id, "expected_bot_revision":1,
            "name":"Routine", "objective":"Scheduled work", "documents":[],
            "recurrence":{"type":"floating","cron":"0 9 * * *"}
        }))
        .unwrap();
        repository
            .prepare_bot_routine_creation(owner, &command, RoutineDigest32::from_bytes([7; 32]), 2)
            .unwrap();
        let creation = repository
            .bot_routine_creation_by_request(owner, bot.id, command.request)
            .unwrap()
            .unwrap();
        (
            Self {
                memory,
                adapter,
                owner,
                profile,
                other,
                bot,
                creation,
                command,
            },
            repository,
        )
    }
    fn prepare(&self) -> Result<PreparedRoutineDocument, BotError> {
        self.adapter.prepare_routine_document(
            self.owner,
            self.profile,
            &self.creation.identity,
            &Cancellation::default(),
        )
    }
    pub(crate) fn origin(&self) -> CreationOrigin {
        let id = self.creation.identity.clone();
        self.memory
            .with_store(move |store| {
                match store
                    .bounded_creation_origin(id.home, DocumentId::from_uuid(id.schedule.as_uuid()))
                    .unwrap()
                {
                    CreationOriginLookup::Known(origin) => origin,
                    _ => panic!("retained origin required"),
                }
            })
            .unwrap()
    }
}

#[test]
fn routine_current_definition_uses_retained_origin_and_actual_store_location() {
    let (f, _) = Fixture::new();
    f.prepare().unwrap();
    let origin = f.origin();
    let mut definition = f.creation.identity.seed.header.definition.clone();
    definition.revision += 1;
    definition.effective_at_ms += 1;
    definition.lifecycle = serde_json::from_str("\"active\"").unwrap();
    definition.template.objective =
        koru_delegate_core::Objective::new("Edited current task").unwrap();
    let current = definition.clone();
    let id = f.creation.identity.clone();
    f.memory
        .with_store(move |store| {
            let candidate = DocumentId::from_uuid(id.schedule.as_uuid());
            let doc = ScheduleDocument::from_snapshot(&store.snapshot(id.home, candidate).unwrap())
                .unwrap();
            doc.update_definition(id.owner.device, &current).unwrap();
            doc.doc()
                .get_map("schedule")
                .insert("name", "Current title")
                .unwrap();
            doc.doc().commit();
            store
                .apply_update(
                    id.home,
                    candidate,
                    doc.export_updates_since(&loro::VersionVector::default())
                        .unwrap(),
                    Timestamp::from_millis(5),
                    None,
                )
                .unwrap();
            store
                .flush(id.home, candidate, Timestamp::from_millis(5))
                .unwrap();
        })
        .unwrap();
    let verify = |reader,
                  identity: &BotRoutineCreationIdentity,
                  definition: &RoutineScheduleDefinition,
                  cancel: &Cancellation| {
        f.adapter
            .verify_routine_definition(f.owner, reader, identity, definition, None, cancel)
    };
    assert!(
        verify(
            f.profile,
            &f.creation.identity,
            &definition,
            &Cancellation::default()
        )
        .is_ok()
    );
    assert_eq!(f.origin(), origin);
    assert!(
        verify(
            f.other,
            &f.creation.identity,
            &definition,
            &Cancellation::default()
        )
        .is_err()
    );
    assert!(
        verify(
            f.profile,
            &f.creation.identity,
            &f.creation.identity.seed.header.definition,
            &Cancellation::default()
        )
        .is_err()
    );
    let mut wrong = f.creation.identity.clone();
    wrong.schedule = koru_delegate_core::ScheduleId::mint();
    wrong.seed.header.schedule = wrong.schedule;
    wrong.seed_hash = wrong.seed.canonical_digest().unwrap();
    assert!(verify(f.profile, &wrong, &definition, &Cancellation::default()).is_err());
    let cancel = Cancellation::default();
    cancel.cancel();
    assert!(verify(f.profile, &f.creation.identity, &definition, &cancel).is_err());
    let id = f.creation.identity.clone();
    f.memory
        .with_store(move |store| {
            store
                .trash_document_entry(
                    id.home,
                    DocumentId::from_uuid(id.schedule.as_uuid()),
                    Timestamp::from_millis(6),
                    None,
                )
                .unwrap();
        })
        .unwrap();
    assert!(
        verify(
            f.profile,
            &f.creation.identity,
            &definition,
            &Cancellation::default()
        )
        .is_err()
    );
}

#[test]
fn routine_store_retains_exact_origin_and_original_vv_after_lost_acknowledgement() {
    let (f, mut repo) = Fixture::new();
    let PreparedRoutineDocument::Confirmed(first) = f.prepare().unwrap() else {
        panic!()
    };
    let origin = f.origin();
    assert_eq!(origin.scope, BOT_ROUTINE_SCOPE);
    assert_eq!(
        origin.document.as_uuid(),
        f.creation.identity.schedule.as_uuid()
    );
    let PreparedRoutineDocument::Confirmed(retry) = f.prepare().unwrap() else {
        panic!()
    };
    assert_eq!(f.origin(), origin);
    let original = ScheduleDocument::verify_routine_seed(
        &origin.initial_update,
        &first.identity.seed.header,
        &first.identity.seed.name,
    )
    .unwrap();
    assert_eq!(
        loro::VersionVector::decode(&retry.origin_edit_revision).unwrap(),
        original.version()
    );
    let bound = repo
        .transition_bot_routine_creation(
            &first.identity,
            f.creation.observed(),
            BotRoutineCreationTransition::Confirmed(first.clone()),
            3,
        )
        .unwrap();
    assert_eq!(bound.origin_edit_revision, Some(first.origin_edit_revision));
}

#[test]
fn routine_store_refuses_current_profile_cancel_and_corrupt_identity_before_create() {
    let (f, _) = Fixture::new();
    assert!(matches!(
        f.adapter.prepare_routine_document(
            f.owner,
            f.other,
            &f.creation.identity,
            &Cancellation::default()
        ),
        Err(BotError::Forbidden)
    ));
    let cancel = Cancellation::default();
    cancel.cancel();
    assert!(matches!(
        f.adapter
            .prepare_routine_document(f.owner, f.profile, &f.creation.identity, &cancel),
        Err(BotError::Unavailable)
    ));
    let mut identity = f.creation.identity.clone();
    identity.seed_hash = RoutineDigest32::from_bytes([0; 32]);
    assert!(
        f.adapter
            .prepare_routine_document(f.owner, f.profile, &identity, &Cancellation::default())
            .is_err()
    );
    let id = f.creation.identity;
    f.memory
        .with_store(move |store| {
            assert!(matches!(
                store
                    .bounded_creation_origin(id.home, DocumentId::from_uuid(id.schedule.as_uuid()))
                    .unwrap(),
                CreationOriginLookup::Missing
            ))
        })
        .unwrap();
}

#[test]
fn routine_store_preserves_human_edits_retirement_and_trash() {
    for mutation in 0..3 {
        let (f, _) = Fixture::new();
        f.prepare().unwrap();
        let origin = f.origin();
        let id = f.creation.identity.clone();
        f.memory
            .with_store(move |store| {
                let candidate = DocumentId::from_uuid(id.schedule.as_uuid());
                let at = Timestamp::from_millis(4);
                if mutation == 2 {
                    store
                        .trash_document_entry(id.home, candidate, at, None)
                        .unwrap();
                    return;
                }
                let doc =
                    ScheduleDocument::from_snapshot(&store.snapshot(id.home, candidate).unwrap())
                        .unwrap();
                if mutation == 0 {
                    doc.doc()
                        .get_map("schedule")
                        .insert("name", "Human rename")
                        .unwrap();
                } else {
                    doc.doc()
                        .get_map("schedule")
                        .insert("retired", true)
                        .unwrap();
                }
                doc.doc().commit();
                store
                    .apply_update(
                        id.home,
                        candidate,
                        doc.export_updates_since(&loro::VersionVector::default())
                            .unwrap(),
                        at,
                        None,
                    )
                    .unwrap();
                store.flush(id.home, candidate, at).unwrap();
            })
            .unwrap();
        assert!(matches!(
            f.prepare().unwrap(),
            PreparedRoutineDocument::Disagreement(BotRoutineCreationRefusal::DocumentChanged)
        ));
        assert_eq!(f.origin(), origin);
    }
}

#[test]
fn routine_store_rejects_wrong_origin_kind_and_foreign_physical_document() {
    for wrong_kind in [false, true] {
        let (f, _) = Fixture::new();
        let id = f.creation.identity.clone();
        f.memory
            .with_store(move |store| {
                let update =
                    ScheduleDocument::routine_seed_update(&id.seed.header, &id.seed.name).unwrap();
                store
                    .create_seeded_document_with_origin(
                        id.home,
                        id.seed.name.as_str(),
                        if wrong_kind {
                            koru_core::explore::KIND_DOCUMENT
                        } else {
                            SCHEDULE_KIND
                        },
                        None,
                        None,
                        BOT_ROUTINE_SCOPE,
                        if wrong_kind {
                            id.store_request.to_string()
                        } else {
                            uuid::Uuid::new_v4().to_string()
                        }
                        .as_str(),
                        &String::from(id.seed_hash),
                        DocumentId::from_uuid(id.schedule.as_uuid()),
                        update,
                        None,
                        Timestamp::from_millis(3),
                    )
                    .unwrap();
            })
            .unwrap();
        assert!(matches!(
            f.prepare().unwrap(),
            PreparedRoutineDocument::Disagreement(BotRoutineCreationRefusal::OriginMismatch)
        ));
    }
    let (f, _) = Fixture::new();
    let id = f.creation.identity.clone();
    let alias = DocumentId::new();
    f.memory
        .with_store(move |store| {
            store
                .create_seeded_document_with_origin(
                    id.home,
                    id.seed.name.as_str(),
                    SCHEDULE_KIND,
                    None,
                    None,
                    BOT_ROUTINE_SCOPE,
                    &uuid::Uuid::new_v4().to_string(),
                    &String::from(id.seed_hash),
                    alias,
                    ScheduleDocument::routine_seed_update(&id.seed.header, &id.seed.name).unwrap(),
                    None,
                    Timestamp::from_millis(3),
                )
                .unwrap();
        })
        .unwrap();
    let PreparedRoutineDocument::Confirmed(proof) = f.prepare().unwrap() else {
        panic!()
    };
    assert_ne!(f.origin().document, alias);
    assert_eq!(
        f.origin().document.as_uuid(),
        proof.identity.schedule.as_uuid()
    );
}
