// SPDX-License-Identifier: MPL-2.0

//! Existing root authority and trusted immutable-history capture for Bot intent.
use koru_ai_core::bots::*;
use koru_ai_core::{ProfileId, WorkspaceId};
use koru_store_server::StoreWorkerClient;

// The pure codec is staged before the reviewed Memory authority/delivery slice.
// Keep its production helpers compiled while that wiring is implemented.
#[allow(dead_code)]
mod memory;
pub(crate) mod routine;

/// Semantic, title-only seed identity. No document bytes are generated on the
/// AI writer; the original page is built/reconciled on the creation lane.
pub(crate) fn memory_seed(home: WorkspaceId, title: &str) -> Result<BotMemorySeed, BotError> {
    Ok(BotMemorySeed {
        title: title.to_owned(),
        hash: memory::seed::seed_hash(home, title).map_err(|_| BotError::Invalid)?,
    })
}

/// Called only on the bounded background preparation lane. Encoded history is
/// consumed here; only the bounded recipe and its exact revision leave it.
pub(crate) fn memory_edit_recipe(
    page: BotPage,
    version: koru_delegate_core::DocumentVersionId,
    evidence: koru_store_core::ports::BoundedDocumentVersion,
    operation: &koru_mcp_server::native::memory::MemoryRecipeOperation,
) -> Result<(Vec<u8>, koru_mcp_server::native::memory::MemoryEditRecipe), MemoryAuthorityError> {
    // Alternate ports/fakes must not force a large clone before the common
    // history validator gets to enforce its metadata bound.
    if evidence.version_vector.len() > koru_store_core::ports::MAX_BOUNDED_HISTORY_POSITION_BYTES {
        return Err(BotError::ContextTooLarge.into());
    }
    let revision = evidence.version_vector.clone();
    let verified =
        memory::history::validate(page, version, evidence).map_err(memory_write_error)?;
    let recipe =
        memory::recipe::prepare_recipe(&verified, operation).map_err(memory_write_error)?;
    Ok((revision, recipe))
}

fn memory_write_error(error: memory::MemoryCodecError) -> MemoryAuthorityError {
    use memory::MemoryCodecError as E;
    match error {
        E::InvalidWriteInput | E::InvalidDetailPosition => BotError::Invalid.into(),
        E::UnsupportedDetail => MemoryAuthorityError::DetailUnsupported,
        E::DetailTooLarge => MemoryAuthorityError::DetailTooLarge,
        E::TooLarge(_) => BotError::ContextTooLarge.into(),
        _ => MemoryAuthorityError::HistoryUnavailable,
    }
}

/// The rule of M8 D22 on two stored modes: a Bot is copied unless the lower
/// of its home workspace's mode and that workspace's Profile's is Local only.
pub(crate) fn home_copy_of(
    workspace: koru_store_core::transport::TransportMode,
    profile: koru_store_core::transport::TransportMode,
) -> HomeCopy {
    if workspace.min(profile) == koru_store_core::transport::TransportMode::LocalOnly {
        HomeCopy::LocalOnly
    } else {
        HomeCopy::Copied
    }
}

pub struct BotAuthorityAdapter {
    client: StoreWorkerClient,
    device: DeviceId,
}
impl BotAuthorityAdapter {
    pub fn from_device(client: StoreWorkerClient, device: &str) -> Option<Self> {
        trusted_device(device).map(|device| Self { client, device })
    }
}
impl BotAuthority for BotAuthorityAdapter {
    fn read_routine_detail(
        &self,
        owner: BotOwner,
        reader: ProfileId,
        identity: &BotRoutineCreationIdentity,
        _observed_at_ms: i64,
        cancellation: &koru_ai_core::Cancellation,
    ) -> Result<RoutineDetailObservation, BotError> {
        routine::overview::read_detail(self, owner, reader, identity, cancellation)
    }
    fn read_routine_overview(
        &self,
        owner: BotOwner,
        reader: ProfileId,
        identity: &BotRoutineCreationIdentity,
        cancellation: &koru_ai_core::Cancellation,
    ) -> Result<RoutineOverviewObservation, BotError> {
        routine::overview::read(self, owner, reader, identity, cancellation)
    }
    fn read_routine_detail_page(
        &self,
        owner: BotOwner,
        reader: ProfileId,
        identity: &BotRoutineCreationIdentity,
        _observed_at_ms: i64,
        before: Option<i64>,
        limit: u8,
        cancellation: &koru_ai_core::Cancellation,
    ) -> Result<(RoutineDetailObservation, RoutineFiringsObservation), BotError> {
        routine::overview::read_detail_page(
            self,
            owner,
            reader,
            identity,
            Some((before, limit)),
            cancellation,
        )
    }
    fn verify_routine_definition(
        &self,
        owner: BotOwner,
        reader: ProfileId,
        identity: &BotRoutineCreationIdentity,
        definition: &koru_ai_core::bots::RoutineScheduleDefinition,
        owner_handoff: Option<koru_delegate_core::HandoffId>,
        cancellation: &koru_ai_core::Cancellation,
    ) -> Result<(), BotError> {
        routine::verify_definition(
            self,
            owner,
            reader,
            identity,
            definition,
            owner_handoff,
            koru_delegate_document::RoutineOccurrenceLifecycle::Active,
            cancellation,
        )
    }
    fn verify_routine_test_definition(
        &self,
        owner: BotOwner,
        reader: ProfileId,
        identity: &BotRoutineCreationIdentity,
        definition: &koru_ai_core::bots::RoutineScheduleDefinition,
        owner_handoff: Option<koru_delegate_core::HandoffId>,
        cancellation: &koru_ai_core::Cancellation,
    ) -> Result<(), BotError> {
        routine::verify_definition(
            self,
            owner,
            reader,
            identity,
            definition,
            owner_handoff,
            koru_delegate_document::RoutineOccurrenceLifecycle::TestPausedManual,
            cancellation,
        )
    }
    fn prepare_routine_document(
        &self,
        owner: BotOwner,
        reader: ProfileId,
        identity: &BotRoutineCreationIdentity,
        cancellation: &koru_ai_core::Cancellation,
    ) -> Result<PreparedRoutineDocument, BotError> {
        routine::prepare(self, owner, reader, identity, cancellation)
    }
    fn memory_edit_history(
        &self,
        owner: BotOwner,
        reader: ProfileId,
        binding: &BotMemoryBinding,
        version: koru_delegate_core::DocumentVersionId,
        cancellation: &koru_ai_core::Cancellation,
    ) -> Result<koru_store_core::ports::BoundedDocumentVersion, MemoryAuthorityError> {
        memory::adapter::write::exact(self, owner, reader, binding, version, cancellation)
    }

    fn memory_original_seed_history(
        &self,
        owner: BotOwner,
        reader: ProfileId,
        identity: &BotMemoryIdentity,
        binding: &BotMemoryBinding,
        cancellation: &koru_ai_core::Cancellation,
    ) -> Result<koru_store_core::ports::BoundedDocumentVersion, MemoryAuthorityError> {
        memory::adapter::write::original_seed(self, owner, reader, identity, binding, cancellation)
    }

    fn prepare_memory_page(
        &self,
        owner: BotOwner,
        reader: ProfileId,
        identity: &BotMemoryIdentity,
        cancellation: &koru_ai_core::Cancellation,
    ) -> Result<PreparedMemoryPage, MemoryAuthorityError> {
        memory::adapter::prepare(self, owner, reader, identity, cancellation)
    }

    fn capture_memory(
        &self,
        owner: BotOwner,
        reader: ProfileId,
        binding: &BotMemoryBinding,
        cancellation: &koru_ai_core::Cancellation,
    ) -> Result<PreparedMemoryCapture, MemoryAuthorityError> {
        memory::adapter::capture(self, owner, reader, binding, cancellation)
    }

    fn replay_memory_index(
        &self,
        owner: BotOwner,
        reader: ProfileId,
        home: WorkspaceId,
        capture: &BotLibraryCapture,
        binding: Option<&BotMemoryBinding>,
        cancellation: &koru_ai_core::Cancellation,
    ) -> Result<VerifiedMemoryIndex, MemoryAuthorityError> {
        memory::adapter::replay(self, owner, reader, home, capture, binding, cancellation)
    }

    fn read_memory_page(
        &self,
        owner: BotOwner,
        reader: ProfileId,
        binding: &BotMemoryBinding,
        snapshot: MemorySnapshot,
        position: MemoryReadPosition,
        cancellation: &koru_ai_core::Cancellation,
    ) -> Result<MemoryPageResult, MemoryAuthorityError> {
        memory::adapter::read(
            self,
            owner,
            reader,
            binding,
            snapshot,
            position,
            cancellation,
        )
    }

    fn pinned_document_revision(
        &self,
        owner: BotOwner,
        workspace: WorkspaceId,
        reader: ProfileId,
        document: koru_ai_core::DocumentId,
        version: koru_delegate_core::DocumentVersionId,
    ) -> Result<koru_ai_core::StoreRevision, BotError> {
        if owner.device != self.device {
            return Err(BotError::Forbidden);
        }
        self.client
            .with_store(move |store| {
                if store
                    .local_user_scope()
                    .map_err(|_| BotError::Unavailable)?
                    != owner.user_scope
                    || store
                        .current_placement_profile(workspace)
                        .map_err(|_| BotError::Unavailable)?
                        != Some(reader)
                {
                    return Err(BotError::Forbidden);
                }
                store.profile(reader).map_err(|_| BotError::Forbidden)?;
                store
                    .workspace(workspace)
                    .map_err(|_| BotError::Forbidden)?;
                store
                    .document_entry_live(workspace, document)
                    .map_err(|_| BotError::Unavailable)?
                    .ok_or(BotError::Forbidden)?;
                let metadata = store
                    .document_version_revision(
                        workspace,
                        version,
                        koru_store_core::ports::MAX_HISTORY_REVISION_BYTES,
                    )
                    .map_err(|error| history_revision_error(error.code()))?
                    .ok_or(BotError::Unavailable)?;
                if metadata.id != version || metadata.document != document {
                    return Err(BotError::Invalid);
                }
                Ok(koru_ai_core::StoreRevision(metadata.version_vector))
            })
            .map_err(|_| BotError::Unavailable)?
    }
    fn capture(
        &self,
        owner: BotOwner,
        workspace: WorkspaceId,
        profile: ProfileId,
        pages: &[BotPage],
    ) -> Result<Vec<koru_delegate_core::RunContextItem>, BotError> {
        if owner.device != self.device
            || pages.is_empty()
            || pages.len() > 33
            || pages.iter().any(|p| p.workspace != workspace)
        {
            return Err(BotError::Forbidden);
        }
        let pages = pages.to_vec();
        self.client
            .with_store(move |store| {
                if store
                    .local_user_scope()
                    .map_err(|_| BotError::Unavailable)?
                    != owner.user_scope
                    || store
                        .current_placement_profile(workspace)
                        .map_err(|_| BotError::Unavailable)?
                        != Some(profile)
                {
                    return Err(BotError::Forbidden);
                }
                store
                    .workspace(workspace)
                    .map_err(|_| BotError::Forbidden)?;
                store.profile(profile).map_err(|_| BotError::Forbidden)?;
                let entries = store
                    .documents(workspace)
                    .map_err(|_| BotError::Unavailable)?;
                if pages.iter().any(|p| {
                    !entries
                        .iter()
                        .any(|e| e.id == p.document && e.trashed_at.is_none())
                }) {
                    return Err(BotError::Forbidden);
                }
                pages
                    .into_iter()
                    .map(|page| {
                        let entry = store
                            .capture_history(
                                workspace,
                                page.document,
                                None,
                                koru_store_core::ports::HistoryReason::Manual,
                                koru_store_core::model::Timestamp::now(),
                                None,
                                None,
                                None,
                            )
                            .map_err(|_| BotError::Unavailable)?;
                        Ok(koru_delegate_core::RunContextItem::Document {
                            workspace,
                            document: page.document,
                            version: entry.version,
                        })
                    })
                    .collect()
            })
            .map_err(|_| BotError::Unavailable)?
    }
    fn inspect(
        &self,
        workspaces: &[WorkspaceId],
        pages: &[BotPage],
    ) -> Result<BotAuthoritySnapshot, BotError> {
        let workspaces = workspaces.to_vec();
        let pages = pages.to_vec();
        let device = self.device;
        self.client
            .with_store(move |store| {
                let owner = BotOwner {
                    user_scope: store
                        .local_user_scope()
                        .map_err(|_| BotError::Unavailable)?,
                    device,
                };
                let mut checked = std::collections::BTreeMap::new();
                let mut checked_pages = std::collections::BTreeMap::new();
                for workspace in workspaces {
                    let profile = (|| {
                        store
                            .workspace(workspace)
                            .map_err(|_| BotError::Forbidden)?;
                        let profile = store
                            .current_placement_profile(workspace)
                            .map_err(|_| BotError::Unavailable)?
                            .ok_or(BotError::Forbidden)?;
                        store.profile(profile).map_err(|_| BotError::Forbidden)?;
                        Ok(profile)
                    })();
                    for page in pages.iter().filter(|p| p.workspace == workspace) {
                        let allowed = if profile.is_ok() {
                            store
                                .document_entry_live(workspace, page.document)
                                .map_err(|_| BotError::Unavailable)
                                .and_then(|entry| entry.map(|_| ()).ok_or(BotError::Forbidden))
                        } else {
                            Err(BotError::Forbidden)
                        };
                        checked_pages.insert(page.clone(), allowed);
                    }
                    checked.insert(workspace, profile);
                }
                Ok(BotAuthoritySnapshot {
                    owner,
                    workspaces: checked,
                    pages: checked_pages,
                })
            })
            .map_err(|_| BotError::Unavailable)?
    }
    fn owner(&self) -> Result<BotOwner, BotError> {
        let scope = self
            .client
            .with_store(|store| store.local_user_scope())
            .map_err(|_| BotError::Unavailable)?
            .map_err(|_: koru_store_core::Error| BotError::Unavailable)?;
        Ok(BotOwner {
            user_scope: scope,
            device: self.device,
        })
    }
    /// The two stored rows, and nothing else: the workspace's own mode and
    /// its Profile's, the lower of which decides. `transport_admission` is
    /// never asked, because it also answers Local only while a workspace is
    /// fenced in memory, which is not the person's choice. A missing or
    /// unreadable row, or a Store that does not answer, is `Unavailable`,
    /// and the caller withholds the Bot.
    fn home_copy(&self, workspace: WorkspaceId) -> Result<HomeCopy, BotError> {
        use koru_store_core::transport::TransportSubject;
        self.client
            .with_store(move |store| {
                let own = store.transport_mode(TransportSubject::Workspace(workspace))?;
                let profile = store.transport_mode(TransportSubject::Profile(own.owner))?;
                Ok(home_copy_of(own.mode, profile.mode))
            })
            .map_err(|_| BotError::Unavailable)?
            .map_err(|_: koru_store_core::Error| BotError::Unavailable)
    }
    fn workspace(&self, workspace: WorkspaceId) -> Result<ProfileId, BotError> {
        self.client
            .with_store(move |store| {
                store.workspace(workspace)?;
                let profile = store.current_placement_profile(workspace)?;
                if let Some(profile) = profile {
                    store.profile(profile)?;
                }
                Ok(profile)
            })
            .map_err(|_| BotError::Unavailable)?
            .map_err(|_: koru_store_core::Error| BotError::Unavailable)?
            .ok_or(BotError::Forbidden)
    }
    fn page(&self, page: &BotPage) -> Result<(), BotError> {
        let page = page.clone();
        let found = self
            .client
            .with_store(move |store| {
                store.workspace(page.workspace)?;
                let Some(profile) = store.current_placement_profile(page.workspace)? else {
                    return Ok(false);
                };
                store.profile(profile)?;
                Ok(store
                    .document_entry_live(page.workspace, page.document)?
                    .is_some())
            })
            .map_err(|_| BotError::Unavailable)?
            .map_err(|_: koru_store_core::Error| BotError::Unavailable)?;
        if found {
            Ok(())
        } else {
            Err(BotError::NotFound)
        }
    }
    /// The Store's strict creation origin, under the same owner, placement
    /// and Profile checks as every other Bot effect, in one Store hop. The
    /// candidate, key, hash and seed are the creation receipt's, durable
    /// before this call.
    ///
    /// The Store is asked to create only a candidate it has no origin for.
    /// One whose origin is already recorded (a lost reply, a restart, a
    /// Retry) is never sent again: its stored origin answers, with where the
    /// page stands now. This matters because the Store's strict path
    /// re-registers a known candidate that is missing from the manifest, and
    /// "delete forever" removes only the manifest entry: a replay would bring
    /// back a page the person deleted. Here a known page that is not in the
    /// manifest is answered `Missing` and nothing is written. Trash is told
    /// apart from absence by the derived listing entry, which keeps trashed
    /// entries; if that listing lags the manifest the answer is still one
    /// of the two non-live placements, so no outcome ever registers a page.
    /// Human edits made since creation stay current; the origin keeps the
    /// original first update.
    ///
    /// A page this call creates is told to the workspace's manifest room
    /// before the call answers, as the Store's own creation routes tell it,
    /// so a window with the workspace open lists it at once. A replay
    /// committed nothing and tells nothing.
    fn create_role_page(&self, seed: &BotRoleSeed) -> Result<BotRoleProof, BotError> {
        if seed.owner.device != self.device {
            return Err(BotError::Forbidden);
        }
        let seed = seed.clone();
        self.client
            .with_store_announcing(move |store, announcements| {
                if store
                    .local_user_scope()
                    .map_err(|_| BotError::Unavailable)?
                    != seed.owner.user_scope
                    || store
                        .current_placement_profile(seed.workspace)
                        .map_err(|_| BotError::Unavailable)?
                        != Some(seed.profile)
                {
                    return Err(BotError::Forbidden);
                }
                store
                    .workspace(seed.workspace)
                    .map_err(|_| BotError::Forbidden)?;
                store
                    .profile(seed.profile)
                    .map_err(|_| BotError::Forbidden)?;
                let lookup = |store: &mut koru_store_server::WorkerStore| {
                    store
                        .creation_origin(seed.workspace, seed.candidate)
                        .map_err(|_| BotError::Unavailable)
                };
                let origin = match lookup(store)? {
                    koru_store_core::CreationOriginLookup::Known(origin) => origin,
                    koru_store_core::CreationOriginLookup::Unknown => {
                        return Err(BotError::Conflict);
                    }
                    koru_store_core::CreationOriginLookup::Missing => {
                        let created = store.create_seeded_document_with_origin(
                            seed.workspace,
                            &seed.title,
                            koru_core::explore::KIND_DOCUMENT,
                            None,
                            None,
                            BOT_ROLE_SCOPE,
                            &seed.request_key,
                            &seed.request_hash,
                            seed.candidate,
                            seed.seed.clone(),
                            None,
                            koru_store_core::model::Timestamp::now(),
                        );
                        match created {
                            Ok(proven) => {
                                // The page is in the manifest now: tell a
                                // window that has the workspace open, as
                                // the Store's own creation routes do.
                                if let Some(delta) = proven.creation.manifest_delta {
                                    announcements.manifest(seed.workspace, delta);
                                }
                                proven.origin
                            }
                            Err(error) => {
                                // Still no origin: nothing was written, so a
                                // refusal of the input itself is definite.
                                // Otherwise bytes may be durable, and the
                                // recorded origin answers as for a replay.
                                match lookup(store)? {
                                    koru_store_core::CreationOriginLookup::Known(origin) => origin,
                                    koru_store_core::CreationOriginLookup::Unknown => {
                                        return Err(BotError::Conflict);
                                    }
                                    koru_store_core::CreationOriginLookup::Missing => {
                                        return Err(role_refusal(error.code()));
                                    }
                                }
                            }
                        }
                    }
                };
                let placement = match store
                    .document_entry_live(seed.workspace, seed.candidate)
                    .map_err(|_| BotError::Unavailable)?
                {
                    Some(entry) if entry.kind == koru_core::explore::KIND_DOCUMENT => {
                        BotRolePlacement::Live
                    }
                    Some(_) => return Err(BotError::Conflict),
                    None => match store
                        .document_entry(seed.workspace, seed.candidate)
                        .map_err(|_| BotError::Unavailable)?
                    {
                        Some(_) => BotRolePlacement::Trashed,
                        None => BotRolePlacement::Missing,
                    },
                };
                Ok(BotRoleProof {
                    scope: origin.scope,
                    request_key: origin.request_key,
                    request_hash: origin.request_hash,
                    initial_update: origin.initial_update,
                    placement,
                })
            })
            .map_err(|_| BotError::Unavailable)?
    }
}

/// A Store refusal of the Role page with no origin recorded after it, so
/// nothing was written. Refusals of the input are definite and not worth
/// retrying; the candidate held by another workspace or kind is a conflict;
/// anything else may pass.
fn role_refusal(code: koru_store_core::errors::Code) -> BotError {
    use koru_store_core::errors::Code;
    match code {
        Code::IdempotencyConflict | Code::DocumentExists => BotError::Conflict,
        Code::InvalidRequest
        | Code::InvalidId
        | Code::PayloadTooLarge
        | Code::UnsupportedMediaType => BotError::Invalid,
        Code::WorkspaceNotFound | Code::Forbidden => BotError::Forbidden,
        _ => BotError::Unavailable,
    }
}

fn history_revision_error(code: koru_store_core::errors::Code) -> BotError {
    use koru_store_core::errors::Code;
    match code {
        Code::PayloadTooLarge => BotError::ContextTooLarge,
        Code::CorruptState | Code::InvalidRequest | Code::InvalidId => BotError::Invalid,
        _ => BotError::Unavailable,
    }
}

fn trusted_device(value: &str) -> Option<DeviceId> {
    let id = uuid::Uuid::parse_str(value).ok()?;
    (!id.is_nil()).then(|| DeviceId::from_uuid(id))
}
#[cfg(test)]
mod tests;
