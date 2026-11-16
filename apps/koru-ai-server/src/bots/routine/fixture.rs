// SPDX-License-Identifier: MPL-2.0
use super::*;
#[cfg(test)]
use koru_ai_core::DocumentId;
use koru_ai_core::{AiProviderId, ChatId, ModelSelection, ReasoningChoice, Timestamp};
use koru_ai_sqlite::Repository;
use koru_delegate_core::RoutineDigest32;
use koru_store_server::memory::MemoryStore;
use std::sync::Arc;

pub(crate) struct Fixture {
    pub memory: MemoryStore,
    pub adapter: Arc<BotAuthorityAdapter>,
    pub owner: BotOwner,
    pub profile: ProfileId,
    #[cfg(test)]
    pub other: ProfileId,
    pub bot: Bot,
    pub creation: BotRoutineCreation,
    #[cfg(test)]
    pub command: BotRoutineCreate,
}
impl Fixture {
    #[cfg(test)]
    pub(crate) fn new() -> (Self, Repository) {
        Self::with_pause(true)
    }
    pub(crate) fn with_pause(paused: bool) -> (Self, Repository) {
        let memory = MemoryStore::open("routine-store-fixture").unwrap();
        let (profile, _other, home, role) = memory
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
                avatar: None,
            },
            access: BotAccess::Default,
            selected_pages: vec![],
            revision: 1,
            policy_revision: 1,
            main_chat: ChatId::new(),
            paused,
            folder_lock_request: None,
            folder_frozen_at_ms: None,
            created_at_ms: 1,
            updated_at_ms: 1,
            lifecycle: Default::default(),
            open_operation: None,
            held_by: None,
            pending_change: None,
            connections: Default::default(),
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
                #[cfg(test)]
                other: _other,
                bot,
                creation,
                #[cfg(test)]
                command,
            },
            repository,
        )
    }
    #[cfg(test)]
    pub(crate) fn prepare(&self) -> Result<PreparedRoutineDocument, BotError> {
        self.adapter.prepare_routine_document(
            self.owner,
            self.profile,
            &self.creation.identity,
            &Cancellation::default(),
        )
    }
    #[cfg(test)]
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

#[cfg(windows)]
impl Fixture {
    pub(crate) fn occurrence() -> (
        Self,
        Repository,
        koru_delegate_api::routines::PrepareOccurrence,
    ) {
        use koru_delegate_api::routines::PrepareOccurrence;
        use koru_delegate_core::RoutineSlot;
        let (fixture, mut repository) = Self::with_pause(false);
        let id = fixture.creation.identity.clone();
        let PreparedRoutineDocument::Confirmed(proof) = fixture
            .adapter
            .prepare_routine_document(
                fixture.owner,
                fixture.profile,
                &id,
                &Cancellation::default(),
            )
            .unwrap()
        else {
            panic!("actual retained Store proof")
        };
        repository
            .transition_bot_routine_creation(
                &id,
                fixture.creation.observed(),
                BotRoutineCreationTransition::Confirmed(proof),
                3,
            )
            .unwrap();
        let mut definition = id.seed.header.definition.clone();
        definition.revision += 1;
        definition.effective_at_ms += 1;
        definition.lifecycle = serde_json::from_str("\"active\"").unwrap();
        let live = definition.clone();
        fixture
            .memory
            .with_store(move |store| {
                let candidate = koru_ai_core::DocumentId::from_uuid(id.schedule.as_uuid());
                let doc = koru_delegate_document::ScheduleDocument::from_snapshot(
                    &store.snapshot(id.home, candidate).unwrap(),
                )
                .unwrap();
                let before = doc.version();
                doc.update_definition(id.owner.device, &live).unwrap();
                store
                    .apply_update(
                        id.home,
                        candidate,
                        doc.export_updates_since(&before).unwrap(),
                        Timestamp::from_millis(4),
                        None,
                    )
                    .unwrap();
                store
                    .flush(id.home, candidate, Timestamp::from_millis(4))
                    .unwrap();
            })
            .unwrap();
        let occurrence_ms = Timestamp::now().millis() / 60_000 * 60_000;
        let request = PrepareOccurrence {
            slot: RoutineSlot {
                owner: fixture.creation.identity.owner,
                bot: fixture.bot.id,
                schedule: fixture.creation.identity.schedule,
                occurrence_ms,
            },
            workspace: fixture.bot.home_workspace,
            owner_handoff: None,
            schedule_definition: definition,
        };
        (fixture, repository, request)
    }
}
