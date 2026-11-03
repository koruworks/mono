// SPDX-License-Identifier: MPL-2.0

//! Profiles, workspaces and documents.
//!
//! Every handler here does three things in the same order, and the order is the
//! contract:
//!
//! 1. **check the scope** — before anything is read, so an unauthorised caller
//!    learns nothing, not even whether an id exists;
//! 2. take the store lock;
//! 3. call one method on [`Store`], and return what it gives back.
//!
//! There is no logic in these functions. Anything a handler decided would be a
//! decision the WebSocket path, and any later transport, did not make — which
//! is exactly how a "first-party only" behaviour appears without anyone
//! choosing it.
//!
//! [`Store`]: koru_store_core::service::Store

use std::sync::Arc;

use axum::Json;
use axum::extract::{Extension, Path, Query, State};
use koru_store_core::errors::{Code, Error};
use koru_store_core::ids::{DocumentId, DocumentVersionId, HistoryEntryId, ProfileId, WorkspaceId};
use koru_store_core::model::{Blob, BlobHash, DocumentEntry, Profile, Scope, Timestamp, Workspace};
use koru_store_core::ports::{HistoryReason, UpdateOrigin};
use koru_store_core::text::Locale;
// **The bodies are not declared here.** They live in `koru-store-wire`, which
// the client names too, so a renamed field breaks both builds instead of
// drifting silently — which is what happened while the client sent them as
// `serde_json::json!` literals against types it could not see.
use koru_store_core::ProfileAccountBinding;
use koru_store_core::personal_attributes::EntityRef;
use koru_store_wire::internal::{CloudChangeOrigin, CloudLocalChange, CloudSyncStatus};
use koru_store_wire::{
    ApplyPrivateChanges, AssociateBlob, BindProfileAccount, CaptureHistory, ClearProfileAccount,
    Created, DeleteWorkspaceQuery, DocumentLocation, DocumentOutline, EnsuredFileNote,
    HistoryEntry as WireHistoryEntry, HistoryPage, HistoryQuery, LocalProfileRemoval,
    LocalProfileRestoration, ManifestRef, MoveWorkspace, NewDocument, NewProfile, NewWorkspace,
    OutlineHeading, PauseBlobTransfer, PersonalAttributesQuery, PersonalAttributesResponse,
    PersonalDictionaryResponse, PrivateState, ProfileAccountBindingOutcome,
    ProfileAccountClearOutcome, ProfileAppearance, ProfileLifecycleInventory,
    ProfileLifecycleInventoryItem, ProfileTransportModeOutcome, RelabelHistory, RenameProfile,
    RenameWorkspace, SeedDocumentRequest, SeedDocumentWithCandidateRequest, SetDictionaryWord,
    SetPersonalAttribute, SetProfileTransportMode, TransferDocuments, Transferred,
    WorkspaceAppearance, WorkspaceListQuery, WorkspaceOrder, WorkspaceUsage,
};

use crate::auth::Caller;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

/// What the API needs from the derived node projection to measure live files.
///
/// Blob metadata is authoritative Store data; only the reference set comes
/// from this port. The daemon supplies it so this crate does not link either
/// `koru-projection` or the application's node model.
pub trait WorkspaceUsageService: Send + Sync + 'static {
    fn live_file_blob_hashes(&self, workspace: WorkspaceId) -> Result<Vec<String>, String>;
}

/// What the API needs from the derived node projection to answer an outline.
///
/// Primitives cross this seam for the same reason as the usage port: the HTTP
/// crate must not link `koru-projection`. The workspace travels with the
/// document so the implementation can nudge a cold workspace into being swept.
pub trait DocumentOutlineService: Send + Sync + 'static {
    /// `(node id, level, text)` in reading order, and whether the read model
    /// has this document at all — an empty list from a projected document
    /// means it has no headings, which is not the same answer.
    fn headings(
        &self,
        workspace: WorkspaceId,
        document: DocumentId,
    ) -> Result<(Vec<(String, u8, String)>, bool), String>;
}

// ------------------------------------------------------------------ profiles

pub async fn create_profile(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Json(body): Json<NewProfile>,
) -> ApiResult<Json<Profile>> {
    caller.require(Scope::ManageWorkspaces)?;
    let locale = match body.locale.as_deref() {
        None => Locale::default(),
        Some(tag) => Locale::parse(tag)
            .ok_or_else(|| ApiError::new(Code::InvalidLocale, "not a BCP 47 language tag"))?,
    };
    let profile = lock(&state)?.create_profile_with_appearance(
        &body.name,
        body.purpose.as_deref(),
        body.avatar.as_deref(),
        body.icon.as_deref(),
        body.color.as_deref(),
        locale,
        now(),
    )?;
    state.notify_replication();
    state.publish_catalog_event(koru_events::ProfileCreated {
        profile_id: profile.id.as_uuid(),
    });
    Ok(Json(profile))
}

pub async fn list_profiles(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
) -> ApiResult<Json<Vec<Profile>>> {
    caller.require(Scope::ReadDocuments)?;
    Ok(Json(lock(&state)?.profiles()?))
}

pub async fn provision_account_profile(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    Json(body): Json<koru_store_wire::ProvisionAccountProfile>,
) -> ApiResult<Json<Profile>> {
    caller.require(Scope::ManageWorkspaces)?;
    let id = ProfileId::parse(&id)
        .ok_or_else(|| ApiError::new(Code::InvalidId, "invalid Profile id"))?;
    let binding = ProfileAccountBinding::parse_identity_id(&body.identity_id)?;
    let locale = match body.locale.as_deref() {
        None => Locale::default(),
        Some(value) => Locale::parse(value)
            .ok_or_else(|| ApiError::new(Code::InvalidLocale, "not a BCP 47 language tag"))?,
    };
    let profile = lock(&state)?.provision_account_profile(
        id,
        binding,
        &body.name,
        locale,
        Timestamp::from_millis(body.created_at_millis),
    )?;
    state.notify_replication();
    state.publish_catalog_event(koru_events::ProfileCreated {
        profile_id: id.as_uuid(),
    });
    Ok(Json(profile))
}

pub async fn profile_lifecycle_inventory(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
) -> ApiResult<Json<ProfileLifecycleInventory>> {
    caller.require(Scope::ManageWorkspaces)?;
    let profiles = lock(&state)?
        .profile_lifecycle_inventory()?
        .into_iter()
        .map(|item| ProfileLifecycleInventoryItem {
            profile_id: item.profile,
            account_binding: item.account_binding.to_storage(),
            removed_at_millis: item.removed_at.map(Timestamp::millis),
        })
        .collect();
    Ok(Json(ProfileLifecycleInventory { profiles }))
}

pub async fn remove_profile_local(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> ApiResult<Json<LocalProfileRemoval>> {
    caller.require(Scope::ManageWorkspaces)?;
    let id = ProfileId::parse(&id).ok_or_else(invalid_id)?;
    let outcome = lock(&state)?.remove_profile_local(id, now())?;
    Ok(Json(match outcome {
        koru_store_core::LocalProfileRemoval::Removed => LocalProfileRemoval::Removed,
        koru_store_core::LocalProfileRemoval::AlreadyRemoved => LocalProfileRemoval::AlreadyRemoved,
    }))
}

pub async fn restore_profile_local(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> ApiResult<Json<LocalProfileRestoration>> {
    caller.require(Scope::ManageWorkspaces)?;
    let id = ProfileId::parse(&id).ok_or_else(invalid_id)?;
    let outcome = lock(&state)?.restore_profile_local(id)?;
    Ok(Json(match outcome {
        koru_store_core::LocalProfileRestoration::Restored => LocalProfileRestoration::Restored,
        koru_store_core::LocalProfileRestoration::AlreadyActive => {
            LocalProfileRestoration::AlreadyActive
        }
    }))
}

pub async fn bind_profile_account(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    Json(body): Json<BindProfileAccount>,
) -> ApiResult<Json<ProfileAccountBindingOutcome>> {
    caller.require(Scope::ManageWorkspaces)?;
    let id = ProfileId::parse(&id).ok_or_else(invalid_id)?;
    let expected = body
        .expected_identity_id
        .as_deref()
        .map(ProfileAccountBinding::parse_identity_id)
        .transpose()?;
    let new = ProfileAccountBinding::parse_identity_id(&body.new_identity_id)?;
    let outcome = lock(&state)?.bind_profile_account(id, expected, new)?;
    if outcome == koru_store_core::ProfileAccountBindingOutcome::Bound {
        state.notify_replication();
    }
    Ok(Json(match outcome {
        koru_store_core::ProfileAccountBindingOutcome::Bound => ProfileAccountBindingOutcome::Bound,
        koru_store_core::ProfileAccountBindingOutcome::AlreadyBound => {
            ProfileAccountBindingOutcome::AlreadyBound
        }
    }))
}

pub async fn clear_profile_account(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    Json(body): Json<ClearProfileAccount>,
) -> ApiResult<Json<ProfileAccountClearOutcome>> {
    caller.require(Scope::ManageWorkspaces)?;
    let id = ProfileId::parse(&id).ok_or_else(invalid_id)?;
    let expected = ProfileAccountBinding::parse_identity_id(&body.expected_identity_id)?;
    let outcome = lock(&state)?.clear_profile_account(id, expected)?;
    if outcome == koru_store_core::ProfileAccountClearOutcome::Cleared {
        state.notify_replication();
    }
    Ok(Json(match outcome {
        koru_store_core::ProfileAccountClearOutcome::Cleared => ProfileAccountClearOutcome::Cleared,
        koru_store_core::ProfileAccountClearOutcome::AlreadyOffline => {
            ProfileAccountClearOutcome::AlreadyOffline
        }
    }))
}

pub async fn get_profile(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> ApiResult<Json<Profile>> {
    caller.require(Scope::ReadDocuments)?;
    let id = ProfileId::parse(&id).ok_or_else(invalid_id)?;
    Ok(Json(lock(&state)?.profile(id)?))
}

pub async fn rename_profile(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    Json(body): Json<RenameProfile>,
) -> ApiResult<Json<Profile>> {
    caller.require(Scope::ManageWorkspaces)?;
    let id = ProfileId::parse(&id).ok_or_else(invalid_id)?;
    let profile = lock(&state)?.rename_profile(id, &body.name)?;
    state.notify_replication();
    state.publish_catalog_event(koru_events::ProfileRenamed {
        profile_id: id.as_uuid(),
    });
    Ok(Json(profile))
}

pub async fn set_profile_appearance(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    Json(body): Json<ProfileAppearance>,
) -> ApiResult<Json<Profile>> {
    caller.require(Scope::ManageWorkspaces)?;
    let id = ProfileId::parse(&id).ok_or_else(invalid_id)?;
    let profile =
        lock(&state)?.set_profile_appearance(id, body.icon.as_deref(), body.color.as_deref())?;
    state.notify_replication();
    state.publish_catalog_event(koru_events::ProfileAppearanceChanged {
        profile_id: id.as_uuid(),
    });
    Ok(Json(profile))
}

// ------------------------------------------------------- personal attributes

pub async fn personal_attributes(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    Query(query): Query<PersonalAttributesQuery>,
) -> ApiResult<Json<PersonalAttributesResponse>> {
    caller.require(Scope::ReadDocuments)?;
    let id = ProfileId::parse(&id).ok_or_else(invalid_id)?;
    let entity = query
        .entity
        .as_deref()
        .and_then(EntityRef::parse)
        .ok_or_else(invalid_entity)?;
    let attributes = lock(&state)?.personal_attributes(id, &entity)?;
    Ok(Json(PersonalAttributesResponse {
        entity: entity.to_storage(),
        attributes,
    }))
}

pub async fn set_personal_attribute(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    Json(body): Json<SetPersonalAttribute>,
) -> ApiResult<Json<PersonalAttributesResponse>> {
    caller.require(Scope::EditDocuments)?;
    let id = ProfileId::parse(&id).ok_or_else(invalid_id)?;
    let entity = EntityRef::parse(&body.entity).ok_or_else(invalid_entity)?;
    let attributes =
        lock(&state)?.set_personal_attribute(id, &entity, &body.key, body.value.as_ref())?;
    state.notify_replication();
    Ok(Json(PersonalAttributesResponse {
        entity: entity.to_storage(),
        attributes,
    }))
}

// ------------------------------------------------- document-private state

/// The workspace that holds `document`, for a route that names only the
/// document: ids are global, so the client never has to know it.
fn private_target(
    state: &Arc<AppState>,
    profile: &str,
    document: &str,
) -> ApiResult<(ProfileId, WorkspaceId, DocumentId)> {
    let profile = ProfileId::parse(profile).ok_or_else(invalid_id)?;
    let document = DocumentId::parse(document).ok_or_else(invalid_id)?;
    let workspace = lock(state)?
        .locate(document)?
        .ok_or_else(|| Error::bare(Code::DocumentNotFound))?;
    Ok((profile, workspace, document))
}

pub async fn document_private_state(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path((profile, document)): Path<(String, String)>,
) -> ApiResult<Json<PrivateState>> {
    caller.require(Scope::ReadDocuments)?;
    let (profile, workspace, document) = private_target(&state, &profile, &document)?;
    Ok(Json(
        lock(&state)?.document_private_state(workspace, document, profile)?,
    ))
}

/// Folding needs only read access: a reader changes their own view of a
/// document, never its content. **No replication wake** — this state is
/// device-local — and the change event goes out only after the commit.
pub async fn apply_document_private_changes(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path((profile, document)): Path<(String, String)>,
    Json(body): Json<ApplyPrivateChanges>,
) -> ApiResult<Json<PrivateState>> {
    caller.require(Scope::ReadDocuments)?;
    let (profile, workspace, document) = private_target(&state, &profile, &document)?;
    let (before, after) = {
        let mut store = lock(&state)?;
        let before = store
            .document_private_state(workspace, document, profile)?
            .revision;
        let after = store.apply_document_private_changes(
            workspace,
            document,
            profile,
            &body.changes,
            now(),
        )?;
        (before, after)
    };
    if after.revision != before {
        state.publish_catalog_event(koru_events::DocumentPrivateChanged {
            profile_id: profile.as_uuid(),
            workspace_id: workspace.as_uuid(),
            document_id: document.as_uuid(),
        });
    }
    Ok(Json(after))
}

// ------------------------------------------------------- personal dictionary

pub async fn personal_dictionary(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> ApiResult<Json<PersonalDictionaryResponse>> {
    caller.require(Scope::ReadDocuments)?;
    let id = ProfileId::parse(&id).ok_or_else(invalid_id)?;
    let words = lock(&state)?.personal_dictionary(id)?;
    Ok(Json(PersonalDictionaryResponse { words }))
}

pub async fn set_dictionary_word(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    Json(body): Json<SetDictionaryWord>,
) -> ApiResult<Json<PersonalDictionaryResponse>> {
    caller.require(Scope::EditDocuments)?;
    let id = ProfileId::parse(&id).ok_or_else(invalid_id)?;
    let words = lock(&state)?.set_dictionary_word(id, &body.language, &body.word, body.present)?;
    state.notify_replication();
    Ok(Json(PersonalDictionaryResponse { words }))
}

pub async fn clear_personal_dictionary(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> ApiResult<Json<PersonalDictionaryResponse>> {
    caller.require(Scope::EditDocuments)?;
    let id = ProfileId::parse(&id).ok_or_else(invalid_id)?;
    let words = lock(&state)?.clear_personal_dictionary(id)?;
    state.notify_replication();
    Ok(Json(PersonalDictionaryResponse { words }))
}

// ---------------------------------------------------------------- workspaces

pub async fn create_workspace(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Json(body): Json<NewWorkspace>,
) -> ApiResult<Json<Workspace>> {
    caller.require(Scope::ManageWorkspaces)?;
    let workspace = lock(&state)?.create_workspace(body.owner, &body.name, now())?;
    state.notify_replication();
    state.notify_catalog_change();
    state.publish_catalog_event(koru_events::WorkspaceCatalogCreated {
        workspace_id: workspace.id.as_uuid(),
    });
    Ok(Json(workspace))
}

/// Rename a workspace.
///
/// `PATCH` rather than `PUT`: this changes one field and leaves the owner, the
/// id and the timestamps alone, so a client that sends only a name is saying
/// exactly what it means.
pub async fn rename_workspace(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    Json(body): Json<RenameWorkspace>,
) -> ApiResult<Json<Workspace>> {
    caller.require(Scope::ManageWorkspaces)?;
    let id = WorkspaceId::parse(&id).ok_or_else(invalid_id)?;
    let workspace = lock(&state)?.rename_workspace(id, &body.name, now())?;
    state.notify_replication();
    state.publish_catalog_event(koru_events::WorkspaceCatalogRenamed {
        workspace_id: id.as_uuid(),
    });
    Ok(Json(workspace))
}

/// How a workspace looks in a switcher.
///
/// **The whole set, every time.** These four are edited together and the caller
/// always knows all of them, so sending them whole sidesteps the *absent versus
/// null* question a partial update has to answer in JSON — and which nobody
/// answers the same way twice.
pub async fn set_appearance(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    Json(body): Json<WorkspaceAppearance>,
) -> ApiResult<Json<Workspace>> {
    caller.require(Scope::ManageWorkspaces)?;
    let id = WorkspaceId::parse(&id).ok_or_else(invalid_id)?;
    // The archived flag before and after, read under the one lock the write
    // takes, so the transition reported is the transition that happened.
    // Re-sending `archived: true` for a workspace that already was is not news
    // and says nothing beyond the appearance event.
    //
    // **Archiving hides behind an appearance route, and the scope check has to
    // follow it there.** Icon, colour and pinning are ordinary management;
    // `archived` closes a workspace's terminals and hides its projects, and it
    // travels in the same body because these four are edited together. So the
    // route keeps `manage_workspaces` — which is what the table says a caller
    // must hold — and the *transition* additionally takes `destroy_workspaces`.
    // Reading the current flag before that second check is not a leak: the
    // caller has already proved it may read and write this workspace.
    let (was_archived, workspace) = {
        let mut store = lock(&state)?;
        let was_archived = store.workspace(id)?.archived;
        if was_archived != body.archived {
            caller.require(Scope::DestroyWorkspaces)?;
        }
        let workspace = store.set_workspace_appearance(
            id,
            body.icon.as_deref(),
            body.color.as_deref(),
            body.pinned,
            body.archived,
        )?;
        (was_archived, workspace)
    };
    state.notify_replication();
    let workspace_id = id.as_uuid();
    state.publish_catalog_event(koru_events::WorkspaceCatalogAppearanceChanged { workspace_id });
    match (was_archived, workspace.archived) {
        (false, true) => {
            state.publish_catalog_event(koru_events::WorkspaceCatalogArchived { workspace_id });
        }
        (true, false) => {
            state.publish_catalog_event(koru_events::WorkspaceCatalogUnarchived { workspace_id });
        }
        _ => {}
    }
    Ok(Json(workspace))
}

pub async fn list_workspaces(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Query(query): Query<WorkspaceListQuery>,
) -> ApiResult<Json<Vec<Workspace>>> {
    caller.require(Scope::ReadDocuments)?;
    let store = lock(&state)?;
    Ok(Json(if query.include_deleted {
        store.workspaces_including_deleted()?
    } else {
        store.workspaces()?
    }))
}

/// One indexed pass over every workspace owned by a profile.
///
/// This is a batch because the workspace manager always needs the complete
/// table. Sixteen loopback requests would add latency and sixteen chances for
/// a partially refreshed table without adding information.
pub async fn workspace_usage(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(owner): Path<String>,
) -> ApiResult<Json<Vec<WorkspaceUsage>>> {
    caller.require(Scope::ReadDocuments)?;
    let owner = ProfileId::parse(&owner).ok_or_else(invalid_id)?;
    let inventories = {
        let mut store = lock(&state)?;
        store.profile(owner)?;
        let workspaces: Vec<_> = store
            .workspaces_including_deleted()?
            .into_iter()
            .filter(|workspace| workspace.owner == owner)
            .collect();
        workspaces
            .into_iter()
            .map(|workspace| {
                let usage = store.workspace_usage(workspace.id)?;
                Ok((workspace, usage))
            })
            .collect::<koru_store_core::errors::Result<Vec<_>>>()?
    };
    // The projection has its own SQLite connection. Do not hold the Store's
    // one global mutex while querying it: a read-model delay must not hold an
    // unrelated edit or sync commit behind the workspace manager.
    let usage_service = state.workspace_usage_service();
    let mut answer = Vec::with_capacity(inventories.len());
    for (workspace, (kinds, blobs)) in inventories {
        let stored_bytes = match usage_service
            .as_ref()
            .map(|service| service.live_file_blob_hashes(workspace.id))
        {
            Some(Ok(references)) => {
                let references: std::collections::HashSet<_> = references.into_iter().collect();
                blobs
                    .iter()
                    .filter(|blob| references.contains(&blob.hash.to_string()))
                    .fold(0_u64, |total, blob| total.saturating_add(blob.size))
            }
            Some(Err(error)) => {
                tracing::warn!(
                    workspace = %workspace.id,
                    error,
                    "live workspace size projection failed; falling back to stored blob bytes"
                );
                blobs
                    .iter()
                    .fold(0_u64, |total, blob| total.saturating_add(blob.size))
            }
            None => blobs
                .iter()
                .fold(0_u64, |total, blob| total.saturating_add(blob.size)),
        };
        answer.push(WorkspaceUsage {
            workspace: workspace.id,
            entries: kinds.values().copied().sum(),
            kinds,
            stored_bytes,
        });
    }
    Ok(Json(answer))
}

pub async fn reorder_workspaces(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(owner): Path<String>,
    Json(body): Json<WorkspaceOrder>,
) -> ApiResult<Json<Vec<Workspace>>> {
    caller.require(Scope::ManageWorkspaces)?;
    let owner = ProfileId::parse(&owner).ok_or_else(invalid_id)?;
    let workspaces = lock(&state)?.reorder_workspaces(owner, &body.workspaces)?;
    state.publish_catalog_event(koru_events::WorkspaceCatalogReordered {
        profile_id: owner.as_uuid(),
    });
    Ok(Json(workspaces))
}

pub async fn delete_workspace(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    Query(query): Query<DeleteWorkspaceQuery>,
) -> ApiResult<Json<Workspace>> {
    // **The bypass, closed.** This route used to take the same scope as
    // renaming, so an ordinary local token could soft-delete a workspace with
    // live terminals in it without ever meeting the Gateway operation that
    // closes them first. It now refuses exactly the callers that operation
    // refuses.
    caller.require(Scope::DestroyWorkspaces)?;
    let id = WorkspaceId::parse(&id).ok_or_else(invalid_id)?;
    let mut store = lock(&state)?;
    let mut rooms = vec![crate::rooms::Room::Manifest(id)];
    rooms.extend(
        store
            .documents(id)?
            .into_iter()
            .map(|document| crate::rooms::Room::Document(document.id)),
    );
    let occupied = state.rooms.occupants(&rooms);
    if occupied > 0 && !query.force {
        return Err(Error::new(
            Code::WorkspaceInUse,
            format!("{occupied} active connection(s)"),
        )
        .into());
    }
    state.rooms.block_and_disconnect(&rooms);
    match store.delete_workspace(id, now()) {
        Ok(workspace) => {
            drop(store);
            state.notify_replication();
            state.notify_catalog_change();
            state.publish_catalog_event(koru_events::WorkspaceCatalogDeleted {
                workspace_id: id.as_uuid(),
            });
            Ok(Json(workspace))
        }
        Err(error) => {
            state.rooms.unblock(&rooms);
            Err(error.into())
        }
    }
}

pub async fn restore_workspace(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> ApiResult<Json<Workspace>> {
    // **Restore takes the destructive scope too**, which reads oddly until you
    // notice what it is the inverse of: whoever may bring a workspace back out
    // of the trash is deciding what the trash contains, and a caller that may
    // not put one in has no business taking one out. Recorded as an open
    // question in ADR-267 rather than treated as settled.
    caller.require(Scope::DestroyWorkspaces)?;
    let id = WorkspaceId::parse(&id).ok_or_else(invalid_id)?;
    let mut store = lock(&state)?;
    let workspace = store.restore_workspace(id)?;
    let mut rooms = vec![crate::rooms::Room::Manifest(id)];
    rooms.extend(
        store
            .documents(id)?
            .into_iter()
            .map(|document| crate::rooms::Room::Document(document.id)),
    );
    state.rooms.unblock(&rooms);
    drop(store);
    state.notify_replication();
    state.notify_catalog_change();
    state.publish_catalog_event(koru_events::WorkspaceCatalogRestored {
        workspace_id: id.as_uuid(),
    });
    Ok(Json(workspace))
}

/// Give a workspace to another profile.
///
/// **No room is blocked and none is disconnected**, which is the whole
/// difference between this and `DELETE`. A soft delete hides a workspace's
/// documents, so it has to refuse — or force — while anything holds them open,
/// and `workspace_in_use` is that refusal. A move changes one row's `owner` and
/// `position`; the workspace id does not change, so every room name stays what
/// it was, every open document stays open on the same manifest, and there is
/// nothing for a connection to be wrong about afterwards. Refusing a move under
/// open documents would be a rule copied for the symmetry rather than for a
/// reason, and it would make the operation unusable exactly when a person most
/// wants it: on the workspace they are working in.
///
/// The old owner is read back before the write so the fact can name both ends.
pub async fn move_workspace(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    Json(body): Json<MoveWorkspace>,
) -> ApiResult<Json<Workspace>> {
    caller.require(Scope::MigrateWorkspaces)?;
    let id = WorkspaceId::parse(&id).ok_or_else(invalid_id)?;
    let (from, workspace) = {
        let mut store = lock(&state)?;
        // Under the same lock as the write, so the `from` this reports is the
        // owner the move actually took it away from and not one another caller
        // changed in between.
        let from = store.workspace_including_deleted(id)?.owner;
        let workspace = store.move_workspace(id, body.owner)?;
        (from, workspace)
    };
    state.notify_replication();
    // **Nothing to say when nothing moved.** Asking for the owner a workspace
    // already has succeeds — that is what makes a retry a retry — but it is not
    // a fact, and publishing one would tell every window to refetch a catalog
    // that did not change.
    if from != workspace.owner {
        state.publish_catalog_event(koru_events::WorkspaceCatalogMoved {
            workspace_id: id.as_uuid(),
            from_profile: from.as_uuid(),
            to_profile: workspace.owner.as_uuid(),
        });
    }
    Ok(Json(workspace))
}

pub async fn get_workspace(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> ApiResult<Json<Workspace>> {
    caller.require(Scope::ReadDocuments)?;
    let id = WorkspaceId::parse(&id).ok_or_else(invalid_id)?;
    Ok(Json(lock(&state)?.workspace(id)?))
}

// ----------------------------------------------------------------- documents

pub async fn locate_document(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(document): Path<String>,
) -> ApiResult<Json<DocumentLocation>> {
    caller.require(Scope::ReadDocuments)?;
    let document = DocumentId::parse(&document).ok_or_else(invalid_id)?;
    let (profile, workspace) = {
        let mut store = lock(&state)?;
        let workspace = store
            .locate(document)?
            .ok_or_else(|| ApiError::bare(Code::DocumentNotFound))?;
        (store.workspace(workspace)?.owner, workspace)
    };
    Ok(Json(DocumentLocation {
        profile,
        workspace,
        document,
    }))
}

/// The document's headings, without opening it.
///
/// A missing outline service is not an error. A store built without a
/// projection thread still serves documents, and a picker that cannot offer
/// jump targets must degrade to offering none rather than failing the request.
pub async fn document_outline(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(document): Path<String>,
) -> ApiResult<Json<DocumentOutline>> {
    caller.require(Scope::ReadDocuments)?;
    let document = DocumentId::parse(&document).ok_or_else(invalid_id)?;
    // Resolved here rather than in the port so an unknown document is a 404
    // from the authoritative catalog, never an empty outline that reads as a
    // document with no headings.
    let workspace = {
        let mut store = lock(&state)?;
        store
            .locate(document)?
            .ok_or_else(|| ApiError::bare(Code::DocumentNotFound))?
    };
    let (headings, projected) = match state.document_outline_service() {
        Some(service) => service
            .headings(workspace, document)
            .map_err(|error| ApiError::new(Code::StorageFailed, error))?,
        None => (Vec::new(), false),
    };
    Ok(Json(DocumentOutline {
        document,
        headings: headings
            .into_iter()
            .map(|(node, level, text)| OutlineHeading { node, level, text })
            .collect(),
        projected,
    }))
}

pub async fn create_document(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(workspace): Path<String>,
    Json(body): Json<NewDocument>,
) -> ApiResult<Json<Created>> {
    caller.require(Scope::EditDocuments)?;
    let workspace = WorkspaceId::parse(&workspace).ok_or_else(invalid_id)?;
    let (id, delta, manifest) = {
        let mut store = lock(&state)?;
        let (id, delta) = match body.snapshot {
            Some(snapshot) => store.import_document_at_with_mime_and_trash(
                workspace,
                body.id.unwrap_or_else(DocumentId::new),
                &body.title,
                &body.kind,
                body.mime.as_deref(),
                body.parent,
                body.index,
                body.trashed_at,
                snapshot,
                now(),
            )?,
            None if body.id.is_none() && body.trashed_at.is_none() => store.create_document(
                workspace,
                &body.title,
                &body.kind,
                body.parent,
                body.index,
                now(),
            )?,
            None => {
                return Err(ApiError::from(Error::new(
                    Code::InvalidRequest,
                    "a client-minted id or trash timestamp requires an initial snapshot",
                )));
            }
        };
        (id, delta, store.manifest_id(workspace)?)
    };

    // **After the commit, and to everyone in the manifest room.** An HTTP
    // mutation and a WebSocket one are the same mutation; a client watching
    // the room must not have to know which door an edit came through. Without
    // this a document created over HTTP is invisible until the client
    // rejoins — a silent divergence between two clients doing ordinary things.
    let _ = manifest;
    state.broadcast_manifest(workspace, delta);
    state.notify_replication();
    Ok(Json(Created { id }))
}

/// The one note a file carries, creating it when it has none.
///
/// Idempotent, like the store call it wraps: the answer to "open this file's
/// note" and to "add a note to this file" is the same route. `created` says
/// which of the two just happened — the store reports it, so the handler
/// passes the fact through rather than inferring it from the delta.
pub async fn ensure_file_note(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path((workspace, file)): Path<(String, String)>,
) -> ApiResult<Json<EnsuredFileNote>> {
    caller.require(Scope::EditDocuments)?;
    let workspace = WorkspaceId::parse(&workspace).ok_or_else(invalid_id)?;
    let file = DocumentId::parse(&file).ok_or_else(invalid_id)?;
    let note = lock(&state)?.ensure_file_note(workspace, file, now())?;
    // The idempotent answer changed nothing; broadcasting an empty delta into
    // the manifest room would wake every watcher for nothing.
    if note.created {
        state.broadcast_manifest(workspace, note.delta);
        state.notify_replication();
    }
    Ok(Json(EnsuredFileNote {
        id: note.id,
        created: note.created,
    }))
}

/// Plant opaque content before publishing the document in the manifest.
///
/// Internal because raw Loro bytes are an adapter-to-Store concern, not Koru's
/// public semantic API. The original body is returned on idempotent replay so
/// the adapter can reproduce its first success projection exactly.
pub async fn seed_document(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(workspace): Path<String>,
    Json(body): Json<SeedDocumentRequest>,
) -> ApiResult<axum::response::Response> {
    use axum::http::{HeaderName, HeaderValue, header};
    use axum::response::IntoResponse;

    caller.require(Scope::EditDocuments)?;
    let workspace = WorkspaceId::parse(&workspace).ok_or_else(invalid_id)?;
    let creation = {
        let mut store = lock(&state)?;
        store.create_seeded_document(
            workspace,
            &body.title,
            &body.kind,
            body.parent,
            body.index,
            &caller.label,
            &body.idempotency_key,
            &body.request_hash,
            None,
            body.initial_update,
            now(),
        )?
    };
    if let Some(delta) = creation.manifest_delta {
        state.broadcast_manifest(workspace, delta);
        state.notify_replication();
    }

    let mut response = creation.initial_update.into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    response.headers_mut().insert(
        HeaderName::from_static("x-koru-document-id"),
        HeaderValue::from_str(&creation.document.to_storage())
            .map_err(|_| ApiError::new(Code::Internal, "invalid document response header"))?,
    );
    response.headers_mut().insert(
        HeaderName::from_static("x-koru-idempotent-replay"),
        HeaderValue::from_static(if creation.replayed { "true" } else { "false" }),
    );
    Ok(response)
}

/// Version-fenced seeded creation with a domain-owned document identity.
/// An older Store has no v2 route and therefore refuses before mutation.
pub async fn seed_document_with_candidate(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(workspace): Path<String>,
    Json(body): Json<SeedDocumentWithCandidateRequest>,
) -> ApiResult<axum::response::Response> {
    use axum::http::{HeaderName, HeaderValue, header};
    use axum::response::IntoResponse;

    caller.require(Scope::EditDocuments)?;
    let workspace = WorkspaceId::parse(&workspace).ok_or_else(invalid_id)?;
    let creation = {
        let mut store = lock(&state)?;
        store.create_seeded_document(
            workspace,
            &body.title,
            &body.kind,
            body.parent,
            body.index,
            &caller.label,
            &body.idempotency_key,
            &body.request_hash,
            Some(body.candidate),
            body.initial_update,
            now(),
        )?
    };
    if let Some(delta) = creation.manifest_delta {
        state.broadcast_manifest(workspace, delta);
        state.notify_replication();
    }

    let mut response = creation.initial_update.into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    response.headers_mut().insert(
        HeaderName::from_static("x-koru-document-id"),
        HeaderValue::from_str(&creation.document.to_storage())
            .map_err(|_| ApiError::new(Code::Internal, "invalid document response header"))?,
    );
    response.headers_mut().insert(
        HeaderName::from_static("x-koru-idempotent-replay"),
        HeaderValue::from_static(if creation.replayed { "true" } else { "false" }),
    );
    Ok(response)
}

pub async fn list_documents(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(workspace): Path<String>,
) -> ApiResult<Json<Vec<DocumentEntry>>> {
    caller.require(Scope::ReadDocuments)?;
    let workspace = WorkspaceId::parse(&workspace).ok_or_else(invalid_id)?;
    Ok(Json(lock(&state)?.documents(workspace)?))
}

pub async fn transfer_documents(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(source): Path<String>,
    Json(body): Json<TransferDocuments>,
) -> ApiResult<Json<Transferred>> {
    caller.require(Scope::EditDocuments)?;
    let source = WorkspaceId::parse(&source).ok_or_else(invalid_id)?;
    let target = body.target;
    let (ids, source_delta, target_delta) = {
        let mut store = lock(&state)?;
        store.transfer_documents(source, target, body.root, body.parent, body.index, now())?
    };
    state.broadcast_manifest(source, source_delta);
    state.broadcast_manifest(target, target_delta);
    state.notify_replication();
    Ok(Json(Transferred { ids }))
}

pub async fn get_manifest(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(workspace): Path<String>,
) -> ApiResult<Json<ManifestRef>> {
    caller.require(Scope::ReadDocuments)?;
    let workspace = WorkspaceId::parse(&workspace).ok_or_else(invalid_id)?;
    let document = lock(&state)?.manifest_id(workspace)?;
    Ok(Json(ManifestRef { document }))
}

/// A document's whole state, as bytes.
///
/// `POST` rather than `GET` because it opens the document, which is work —
/// and because a caching layer must not decide on its own that a CRDT snapshot
/// is safe to reuse.
///
/// The body is `application/octet-stream`: a Loro snapshot, exactly as a client
/// would `import` it. Not base64 in JSON — that is a third larger and turns a
/// memcpy into a parse.
pub async fn snapshot(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path((workspace, document)): Path<(String, String)>,
) -> ApiResult<axum::response::Response> {
    use axum::response::IntoResponse;
    caller.require(Scope::ReadDocuments)?;
    let workspace = WorkspaceId::parse(&workspace).ok_or_else(invalid_id)?;
    let document = DocumentId::parse(&document).ok_or_else(invalid_id)?;
    let bytes = lock(&state)?.snapshot(workspace, document)?;
    Ok((
        [(axum::http::header::CONTENT_TYPE, "application/octet-stream")],
        bytes,
    )
        .into_response())
}

pub async fn capture_history(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path((workspace, document)): Path<(String, String)>,
    Json(body): Json<CaptureHistory>,
) -> ApiResult<Json<WireHistoryEntry>> {
    caller.require(Scope::EditDocuments)?;
    let workspace = WorkspaceId::parse(&workspace).ok_or_else(invalid_id)?;
    let document = DocumentId::parse(&document).ok_or_else(invalid_id)?;
    let reason = HistoryReason::parse(&body.reason)
        .ok_or_else(|| ApiError::new(Code::InvalidRequest, "unknown history reason"))?;
    let entry = lock(&state)?.capture_history(
        workspace,
        document,
        body.frontiers.as_deref(),
        reason,
        Timestamp::now(),
        body.label.as_deref(),
        body.message.as_deref(),
        body.boundary.as_deref(),
    )?;
    Ok(Json(history_entry(entry)))
}

pub async fn list_history(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path((workspace, document)): Path<(String, String)>,
    Query(query): Query<HistoryQuery>,
) -> ApiResult<Json<HistoryPage>> {
    caller.require(Scope::ReadDocuments)?;
    let workspace = WorkspaceId::parse(&workspace).ok_or_else(invalid_id)?;
    let document = DocumentId::parse(&document).ok_or_else(invalid_id)?;
    let before = match (query.before_at, query.before_id) {
        (Some(at), Some(id)) => Some((Timestamp::from_millis(at), id)),
        (None, None) => None,
        _ => {
            return Err(ApiError::new(
                Code::InvalidRequest,
                "history cursor is incomplete",
            ));
        }
    };
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    // One lock for both answers. `stale` is a fact about the document rather
    // than about the page, and reading it here — while the document is already
    // open for the listing — costs one indexed row and no snapshot bytes.
    let (entries, stale) = {
        let mut store = lock(&state)?;
        let entries = store.history_entries(workspace, document, before, limit)?;
        let stale = store.history_staleness(workspace, document)?;
        (entries, stale)
    };
    let next = (entries.len() == limit).then(|| {
        let last = entries.last().expect("a full page is non-empty");
        (last.recorded_at.millis(), last.id)
    });
    Ok(Json(HistoryPage {
        entries: entries.into_iter().map(history_entry).collect(),
        next_before_at: next.map(|n| n.0),
        next_before_id: next.map(|n| n.1),
        stale,
    }))
}

/// **Everything one open document's Cloud sync state is, in one round trip.**
///
/// Four sources, and none of them is a new one: the in-memory pass record the
/// Cloud driver writes, the same record's per-dataset half, the document's own
/// row and newest journal provenance, and the live permit set the control host
/// already holds. They are answered together because a surface that asked four
/// times would show four moments and call it one.
///
/// Content-free, like every report in `koru_store_wire::internal`: times,
/// counts, stable codes and ids. A refusal to read any one part is an absent
/// field rather than a failed request — this exists to explain a sync that is
/// not working, and it must not stop working for the same reason.
pub async fn cloud_sync_status(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path((workspace, document)): Path<(String, String)>,
    Query(query): Query<koru_store_wire::internal::CloudSyncQuery>,
) -> ApiResult<Json<CloudSyncStatus>> {
    caller.require(Scope::ReadDocuments)?;
    let workspace = WorkspaceId::parse(&workspace).ok_or_else(invalid_id)?;
    let document = DocumentId::parse(&document).ok_or_else(invalid_id)?;

    let (document_activity, pass) = match state.cloud_activity.lock() {
        Ok(activity) => (
            activity.document(workspace.as_uuid(), document.as_uuid()),
            activity.dataset(workspace.as_uuid()),
        ),
        Err(_) => (None, None),
    };
    let runtime = state
        .sync_control
        .lock()
        .ok()
        .and_then(|control| control.cloud_workspace_stage(workspace.as_uuid()));
    // Read policy with the provenance under the existing Store lock. This is
    // scalar catalog metadata, never an extra document walk or network request.
    let (policy, provenance) = {
        let mut store = lock(&state)?;
        (
            store
                .transport_admission(koru_store_core::transport::TransportSubject::Workspace(
                    workspace,
                ))
                .ok(),
            store.document_provenance(workspace, document),
        )
    };
    let sync = cloud_status::state(policy, runtime);
    let local = provenance.ok().map(|provenance| CloudLocalChange {
        changed_at: provenance.updated_at.millis(),
        origin: match provenance.origin {
            None | Some(UpdateOrigin::Store) => CloudChangeOrigin::Store,
            Some(UpdateOrigin::LocalClient { .. }) => CloudChangeOrigin::ThisDevice,
            Some(UpdateOrigin::Cloud { .. }) => CloudChangeOrigin::Cloud,
            Some(UpdateOrigin::RemoteDevice { .. }) => CloudChangeOrigin::PeerDevice,
        },
        device_id: match provenance.origin {
            Some(UpdateOrigin::RemoteDevice { device_id, .. }) => {
                Some(uuid::Uuid::from_bytes(device_id).to_string())
            }
            _ => None,
        },
    });

    // **The one place** (D256). The blob half arrives with the document half
    // rather than through a second poll: two polls are two moments, and a
    // surface that showed them side by side would be calling them one.
    // A build with no engine answers `None`, which is not zero.
    let blobs = state
        .blob_transfer_status_service()
        .map(|service| service.status(workspace));

    // **And the one file a surface is explaining** (D259 §4), in the same
    // round trip for the same reason. A request that named no file asks
    // nothing, so the ordinary poll is exactly what it was.
    let selected = query.file.as_deref().and_then(DocumentId::parse);
    let file = match (selected, state.blob_transfer_status_service()) {
        (Some(selected), Some(service)) => service.file(workspace, selected).await,
        _ => None,
    };

    Ok(Json(CloudSyncStatus {
        workspace: Some(workspace),
        document: Some(document),
        sync,
        document_activity,
        pass,
        local,
        blobs,
        file,
    }))
}

/// **One document's own Cloud state** — whether Cloud holds it as it is now,
/// and how many of the images and other files it references Cloud holds
/// durably.
///
/// The blob engine answers all of it from what the last coverage computation
/// already decided; this carries the counts and says which document they are
/// about. Read-only all the way down: looking at a document never clears a
/// park or starts a pass.
///
/// A store with no blob engine answers `BlobTransferUnavailable` rather than
/// an empty report — *nothing measured here* and *nothing to measure* must not
/// be the same answer.
pub async fn cloud_sync_coverage(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path((workspace, document)): Path<(String, String)>,
) -> ApiResult<Json<koru_store_wire::internal::CloudDocumentCoverage>> {
    caller.require(Scope::ReadDocuments)?;
    let workspace = WorkspaceId::parse(&workspace).ok_or_else(invalid_id)?;
    let document = DocumentId::parse(&document).ok_or_else(invalid_id)?;
    let service = state.blob_transfer_status_service().ok_or_else(|| {
        ApiError::new(
            Code::BlobTransferUnavailable,
            "this store has no blob transfer engine to ask",
        )
    })?;
    let coverage = service
        .document_coverage(workspace, document)
        .await
        .map_err(|code| {
            ApiError::new(
                Code::BlobTransferUnavailable,
                format!("the document's Cloud coverage could not be read: {code}"),
            )
        })?;
    Ok(Json(koru_store_wire::internal::CloudDocumentCoverage {
        workspace: Some(workspace),
        document: Some(document),
        ..coverage
    }))
}

/// **Which documents are waiting on a file Koru Cloud does not hold yet**, and
/// which device each one is waiting for.
///
/// Three sources, none of them new. The blob engine names the documents and
/// the files each is missing (its `object_missing` parks, mapped back through
/// the canonical walk); the manifest listing names the documents, the same
/// titles every other listing reads; and the Store's record of *which device
/// first held these bytes* — the blob's own origin attribute, or the one a
/// peer's inventory advertised — together with every device the peer runtime
/// heard advertise a copy this session ([`crate::state::BlobHolderDirectory`]),
/// is turned into a name through the authorization snapshot the Agent
/// installed, which is where every peer's display name already lives.
///
/// **A holder is named only when it is known.** An origin this device does
/// not recognise, this device itself, or a blob nobody recorded an origin for
/// contributes no name, and a document whose files have none says so with an
/// empty list rather than a guess.
///
/// Read-only all the way down: nothing here clears a park or starts a pass,
/// so a person expanding a row is never a trigger.
pub async fn cloud_sync_waiting(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(workspace): Path<String>,
) -> ApiResult<Json<koru_store_wire::CloudWaitingDocuments>> {
    caller.require(Scope::ReadDocuments)?;
    let workspace = WorkspaceId::parse(&workspace).ok_or_else(invalid_id)?;
    let service = state.blob_transfer_status_service().ok_or_else(|| {
        ApiError::new(
            Code::BlobTransferUnavailable,
            "this store has no blob transfer engine to ask",
        )
    })?;
    let waiting = service.waiting(workspace).await.map_err(|code| {
        ApiError::new(
            Code::BlobTransferUnavailable,
            format!("the waiting documents could not be read: {code}"),
        )
    })?;
    // **The same mapping the phone runs in process** (`store.cloud_waiting`):
    // one function, so the two surfaces cannot name a holder or order the
    // list two ways. The snapshot is cloned out so the control lock is never
    // held across the store's.
    let authorization = match waiting.is_empty() {
        true => None,
        false => state
            .sync_control
            .lock()
            .ok()
            .and_then(|control| control.snapshot().cloned()),
    };
    // The devices the peer runtime heard advertise these blobs, beside each
    // blob's origin: a copy-holder that did not create the file is named too.
    let advertised = match (waiting.is_empty(), state.blob_holder_directory()) {
        (false, Some(directory)) => {
            directory.advertised_holders(workspace, &crate::cloud_waiting::hashes(&waiting))
        }
        _ => std::collections::HashMap::new(),
    };
    let documents = crate::cloud_waiting::documents(
        &mut *lock(&state)?,
        workspace,
        waiting,
        &advertised,
        authorization.as_ref(),
    )?;
    Ok(Json(koru_store_wire::CloudWaitingDocuments {
        workspace: Some(workspace),
        documents,
    }))
}

/// **A person's pause, per workspace** (D256).
///
/// Answers the report as it stands afterwards, so the surface that pressed it
/// does not have to ask again to draw the result.
///
/// A store with no blob engine answers `Conflict` rather than pretending: a
/// button that reported success against nothing is exactly the silence §13
/// already paid for.
pub async fn pause_blob_transfer(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(workspace): Path<String>,
    Json(body): Json<PauseBlobTransfer>,
) -> ApiResult<Json<koru_store_wire::internal::BlobTransferReport>> {
    caller.require(Scope::ManageWorkspaces)?;
    let workspace = WorkspaceId::parse(&workspace).ok_or_else(invalid_id)?;
    let service = state.blob_transfer_status_service().ok_or_else(|| {
        ApiError::new(
            Code::BlobTransferUnavailable,
            "this store has no blob transfer engine to pause",
        )
    })?;
    Ok(Json(service.set_paused(workspace, body.paused)))
}

/// **Clear local files** (rung 2's space reclaim): every copy of a file that
/// Koru Cloud holds leaves this device, and every reference stays.
///
/// The safety rule is the engine's — each address is asked of the control
/// plane before it is touched — and the answer says how many went, how many
/// were kept because Cloud has not confirmed them, and where it stopped if
/// the control plane stopped answering. A store with no engine answers
/// `Conflict`, for `pause_blob_transfer`'s reason.
pub async fn release_local_blobs(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(workspace): Path<String>,
) -> ApiResult<Json<koru_store_wire::internal::BlobReleaseReport>> {
    caller.require(Scope::ManageWorkspaces)?;
    let workspace = WorkspaceId::parse(&workspace).ok_or_else(invalid_id)?;
    let service = state.blob_transfer_status_service().ok_or_else(|| {
        ApiError::new(
            Code::BlobTransferUnavailable,
            "this store has no blob transfer engine to release through",
        )
    })?;
    service
        .release_local(workspace)
        .await
        .map(Json)
        .map_err(|code| {
            ApiError::new(
                Code::BlobTransferUnavailable,
                format!("local files could not be cleared: {code}"),
            )
        })
}

/// **The local Store operation behind the Profile prompt** (D258).
///
/// Not a hosted route in disguise: D230 settled that the transport mode is a
/// device-local choice, and nothing here writes a hosted row, a policy CRDT, a
/// peer message or a remote stop notice. Another device is told nothing and
/// decides for itself.
///
/// A Workspace somebody already chose for is never in the group, whichever
/// way this is answered — see [`koru_store_core::Store::choose_transport_mode`].
pub async fn set_profile_transport_mode(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    Json(body): Json<SetProfileTransportMode>,
) -> ApiResult<Json<ProfileTransportModeOutcome>> {
    caller.require(Scope::ManageWorkspaces)?;
    let id = ProfileId::parse(&id).ok_or_else(invalid_id)?;
    // An argument that is not understood is not run: an unknown token is
    // refused here rather than quietly read as Local-only.
    let mode = koru_store_core::TransportMode::parse(&body.mode).map_err(|_| {
        ApiError::new(
            Code::InvalidRequest,
            "mode is not one of local_only, p2p, cloud",
        )
    })?;
    let (choice, enabled) = {
        let mut store = lock(&state)?;
        let choice = store.choose_transport_mode(id, mode, body.include_workspaces)?;
        // **Which Workspaces may now move bytes**, read after the commit
        // rather than inferred from it: the effective answer is the child's
        // own mode capped at its Profile's, and a child left alone under a
        // Profile that has just risen is exactly as enabled as one that moved.
        let group = store.transport_mode_group(id)?;
        let enabled: Vec<WorkspaceId> = group
            .children
            .iter()
            .filter(|child| child.mode.min(group.profile.mode).allows_cloud())
            .filter_map(|child| child.subject.workspace())
            .collect();
        (choice, enabled)
    };
    // **Tell, then start** (D256). §10's first trigger, raised here because
    // this is the only place that knows Cloud was just turned on. Each pass
    // walks its Workspace once, publishes what is about to move and then
    // begins; none of that happens on this request's thread.
    if let Some(blobs) = state.blob_transfer_status_service() {
        for workspace in enabled {
            blobs.enabled(workspace);
        }
    }
    let profile = choice.profile.state();
    Ok(Json(ProfileTransportModeOutcome {
        mode: profile.mode.as_storage().to_owned(),
        profile_changed: choice.profile.wrote(),
        explicit_choice: profile.explicit_choice,
        workspaces_submitted: choice.workspaces_submitted,
        workspaces_moved: choice.workspaces_moved,
        workspaces_left_alone: choice.workspaces_left_alone,
    }))
}

pub async fn relabel_history(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path((workspace, entry)): Path<(String, String)>,
    Json(body): Json<RelabelHistory>,
) -> ApiResult<Json<WireHistoryEntry>> {
    caller.require(Scope::EditDocuments)?;
    let workspace = WorkspaceId::parse(&workspace).ok_or_else(invalid_id)?;
    let entry = HistoryEntryId::parse(&entry).ok_or_else(invalid_id)?;
    let entry = lock(&state)?.relabel_history(workspace, entry, body.label.as_deref())?;
    Ok(Json(history_entry(entry)))
}

pub async fn history_snapshot(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path((workspace, entry)): Path<(String, String)>,
) -> ApiResult<axum::response::Response> {
    use axum::response::IntoResponse;
    caller.require(Scope::ReadDocuments)?;
    let workspace = WorkspaceId::parse(&workspace).ok_or_else(invalid_id)?;
    let entry = HistoryEntryId::parse(&entry).ok_or_else(invalid_id)?;
    let version = lock(&state)?.history_version(workspace, entry)?;
    Ok((
        [(axum::http::header::CONTENT_TYPE, "application/octet-stream")],
        version.snapshot,
    )
        .into_response())
}

/// The immutable bytes a `DocumentVersionId` names — what a delegated run's
/// pin resolves to (ROADMAP §7.2, D-h).
///
/// **By version, not by marker.** A history marker can be relabelled and
/// two markers can name one version; a pin keeps the version id, because a
/// version cannot be anything but its bytes. A `GET`, unlike the two
/// snapshot routes above: those open a live document, which is work and
/// which changes, while a version is immutable and this is a read of it.
///
/// The body is the checksum-verified Loro snapshot. The three facts the
/// caller needs beside it ride headers, the way a seeded create names its
/// document and a blob names its filename: `x-koru-document-id` (the
/// document the version belongs to, so a pin assembled wrong is caught
/// without a second call), `x-koru-checksum` (the store's own, which a
/// client compares against what it received) and `x-koru-captured-at`
/// (epoch milliseconds). Absent — never captured here, or purged — answers
/// `document_version_not_found`.
pub async fn version_snapshot(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path((workspace, version)): Path<(String, String)>,
) -> ApiResult<axum::response::Response> {
    use axum::http::{HeaderName, HeaderValue};
    use axum::response::IntoResponse;
    caller.require(Scope::ReadDocuments)?;
    let workspace = WorkspaceId::parse(&workspace).ok_or_else(invalid_id)?;
    let version = DocumentVersionId::parse(&version).ok_or_else(invalid_id)?;
    let version = lock(&state)?
        .document_version(workspace, version)?
        .ok_or_else(|| ApiError::from(Error::bare(Code::DocumentVersionNotFound)))?;
    let header = |value: &str| {
        HeaderValue::from_str(value)
            .map_err(|_| ApiError::new(Code::Internal, "invalid version response header"))
    };
    let mut response = version.snapshot.into_response();
    let headers = response.headers_mut();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    headers.insert(
        HeaderName::from_static("x-koru-document-id"),
        header(&version.document.to_storage())?,
    );
    headers.insert(
        HeaderName::from_static("x-koru-checksum"),
        header(&version.checksum)?,
    );
    headers.insert(
        HeaderName::from_static("x-koru-captured-at"),
        header(&version.captured_at.millis().to_string())?,
    );
    Ok(response)
}

fn history_entry(entry: koru_store_core::ports::HistoryEntry) -> WireHistoryEntry {
    WireHistoryEntry {
        id: entry.id,
        document: entry.document,
        version: entry.version,
        reason: entry.reason.as_str().to_string(),
        recorded_at: entry.recorded_at.millis(),
        label: entry.label,
        message: entry.message,
        boundary: entry.boundary,
        pinned: entry.pinned,
    }
}

// --------------------------------------------------------------------- blobs

/// Store bytes.
///
/// The body is the bytes themselves — `application/octet-stream` — not base64
/// in JSON, which is a third larger and turns a copy into a parse. The mime
/// type comes from `Content-Type` and the filename from `X-Koru-Filename`,
/// which is **percent-encoded** because a filename is arbitrary Unicode and a
/// header value is not: a raw newline in one would split the response.
pub async fn put_blob(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(workspace): Path<String>,
    headers: axum::http::HeaderMap,
    body: axum::body::Body,
) -> ApiResult<Json<Blob>> {
    caller.require(Scope::EditDocuments)?;
    let workspace = WorkspaceId::parse(&workspace).ok_or_else(invalid_id)?;
    let mime = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();
    let filename = headers
        .get("x-koru-filename")
        .and_then(|v| v.to_str().ok())
        .map(percent_decode);

    // The storing device names itself as the blob's origin, when identity is
    // up. Before the Agent hands over an authorization snapshot there is no
    // local device id to record, and none is better than a wrong one.
    let origin_device = state
        .sync_control
        .lock()
        .ok()
        .and_then(|control| control.snapshot().map(|s| s.local_device.to_storage()));

    let begin_state = Arc::clone(&state);
    let pending = tokio::task::spawn_blocking(move || {
        lock(&begin_state)?
            .begin_blob_upload(workspace)
            .map_err(ApiError::from)
    })
    .await
    .map_err(|error| ApiError::new(Code::Internal, error.to_string()))??;
    let completed = receive_blob(
        pending,
        body,
        koru_store_core::ports::BLOB_UPLOAD_BODY_LIMIT,
    )
    .await?;
    let blob = tokio::task::spawn_blocking(move || {
        lock(&state)?
            .finish_blob_upload(
                workspace,
                completed,
                &mime,
                filename.as_deref(),
                now(),
                origin_device.as_deref(),
            )
            .map_err(ApiError::from)
    })
    .await
    .map_err(|error| ApiError::new(Code::Internal, error.to_string()))??;
    Ok(Json(blob))
}

const BLOB_UPLOAD_CHUNK: usize = 64 * 1024;
const BLOB_UPLOAD_CHANNEL: usize = 4;

enum UploadMessage {
    Chunk(axum::body::Bytes),
    Finish,
}

pub(crate) async fn receive_blob(
    pending: Box<dyn koru_store_core::ports::PendingBlobUpload>,
    body: axum::body::Body,
    limit: u64,
) -> ApiResult<koru_store_core::ports::CompletedBlobUpload> {
    use futures_util::StreamExt as _;

    let (sender, mut receiver) = tokio::sync::mpsc::channel(BLOB_UPLOAD_CHANNEL);
    let writer = tokio::task::spawn_blocking(move || {
        let mut pending = pending;
        while let Some(message) = receiver.blocking_recv() {
            match message {
                UploadMessage::Chunk(bytes) => pending.write_chunk(&bytes)?,
                UploadMessage::Finish => return pending.finish(),
            }
        }
        Err(koru_store_core::Error::new(
            Code::InvalidRequest,
            "blob request ended before the body completed",
        ))
    });

    let mut stream = body.into_data_stream();
    let mut received = 0_u64;
    while let Some(frame) = stream.next().await {
        let bytes = match frame {
            Ok(bytes) => bytes,
            Err(error) => {
                drop(sender);
                let _ = writer.await;
                return Err(ApiError::new(
                    Code::InvalidRequest,
                    format!("read blob body: {error}"),
                ));
            }
        };
        received = received
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| ApiError::bare(Code::PayloadTooLarge))?;
        if received > limit {
            drop(sender);
            let _ = writer.await;
            return Err(ApiError::new(
                Code::PayloadTooLarge,
                format!("blob exceeds the {limit} byte limit"),
            ));
        }
        for offset in (0..bytes.len()).step_by(BLOB_UPLOAD_CHUNK) {
            let end = offset.saturating_add(BLOB_UPLOAD_CHUNK).min(bytes.len());
            if sender
                .send(UploadMessage::Chunk(bytes.slice(offset..end)))
                .await
                .is_err()
            {
                return writer
                    .await
                    .map_err(|error| ApiError::new(Code::Internal, error.to_string()))?
                    .map_err(ApiError::from);
            }
        }
    }
    sender
        .send(UploadMessage::Finish)
        .await
        .map_err(|_| ApiError::new(Code::StorageFailed, "blob writer stopped"))?;
    writer
        .await
        .map_err(|error| ApiError::new(Code::Internal, error.to_string()))?
        .map_err(ApiError::from)
}

/// The bytes, verified against the hash they are filed under. A local miss
/// waits on the daemon's event-driven peer hydration before reading again.
pub async fn get_blob(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path((workspace, hash)): Path<(String, String)>,
) -> ApiResult<axum::response::Response> {
    use axum::response::IntoResponse;
    caller.require(Scope::ReadDocuments)?;
    let workspace = WorkspaceId::parse(&workspace).ok_or_else(invalid_id)?;
    let hash = BlobHash::parse(&hash).map_err(ApiError::from)?;

    let (blob, bytes) = match local_blob(&state, workspace, &hash) {
        Ok(found) => found,
        Err(error) if error.code() == Code::BlobNotFound => {
            let Some(hydration) = state.blob_hydration_service() else {
                return Err(error.into());
            };
            hydration
                .hydrate(workspace, hash.clone())
                .await
                .map_err(|failure| {
                    let code = if failure.code == Code::BlobNotFound.as_str() {
                        Code::BlobNotFound
                    } else if failure.code == Code::BlobWaitingForPeer.as_str() {
                        // The bytes exist on a device that is offline right
                        // now. Hydration keeps waiting behind this answer and
                        // completes when that device connects — the caller
                        // should retry then, not despair now.
                        Code::BlobWaitingForPeer
                    } else if failure.code == Code::NoLinkedDevices.as_str() {
                        Code::NoLinkedDevices
                    } else {
                        Code::StorageFailed
                    };
                    ApiError::new(
                        code,
                        format!(
                            "blob hydration failed: {} (retryable={})",
                            failure.code, failure.retryable
                        ),
                    )
                    .with_waiting_on(failure.waiting_on)
                    // **The hold survives the flattening above** (D259). Four
                    // Store codes cannot carry six blob holds, and the one
                    // that matters most — a Workspace this device has never
                    // been switched on for — was landing as `storage_failed`
                    // with its reason buried in developer text a client must
                    // not show. It rides beside the code instead.
                    .with_hold(failure.hold)
                })?;
            local_blob(&state, workspace, &hash)?
        }
        Err(error) => return Err(error.into()),
    };
    Ok((
        [
            (axum::http::header::CONTENT_TYPE, blob.mime.clone()),
            // Percent-encoded on the way out too, for the same reason.
            (
                axum::http::HeaderName::from_static("x-koru-filename"),
                blob.filename
                    .as_deref()
                    .map(percent_encode)
                    .unwrap_or_default(),
            ),
        ],
        bytes,
    )
        .into_response())
}

/// Associate another workspace with a blob before publishing references to it
/// there. **No bytes move** — see [`AssociateBlob`].
pub async fn associate_blob(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path((source, hash)): Path<(String, String)>,
    Json(body): Json<AssociateBlob>,
) -> ApiResult<Json<Blob>> {
    caller.require(Scope::EditDocuments)?;
    let source = WorkspaceId::parse(&source).ok_or_else(invalid_id)?;
    let hash = BlobHash::parse(&hash).map_err(ApiError::from)?;
    Ok(Json(lock(&state)?.associate_blob(
        source,
        body.target,
        &hash,
    )?))
}

fn local_blob(
    state: &Arc<AppState>,
    workspace: WorkspaceId,
    hash: &BlobHash,
) -> koru_store_core::Result<(Blob, Vec<u8>)> {
    let mut store = state.store.lock().map_err(|_| {
        Error::new(
            Code::Internal,
            "the store lock was poisoned by an earlier failure",
        )
    })?;
    let read = |store: &mut crate::state::KoruStore| -> koru_store_core::Result<(Blob, Vec<u8>)> {
        let blob = store.blob(workspace, hash)?;
        let bytes = store.get_blob(workspace, hash)?;
        Ok((blob, bytes))
    };
    match read(&mut store) {
        Ok(found) => Ok(found),
        Err(error) if error.code() == Code::BlobNotFound => {
            if store.repair_blob_association(workspace, hash)? {
                read(&mut store)
            } else {
                Err(error)
            }
        }
        Err(error) => Err(error),
    }
}

pub async fn list_blobs(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(workspace): Path<String>,
) -> ApiResult<Json<Vec<Blob>>> {
    caller.require(Scope::ReadDocuments)?;
    let workspace = WorkspaceId::parse(&workspace).ok_or_else(invalid_id)?;
    Ok(Json(lock(&state)?.blobs(workspace)?))
}

/// Percent-encode everything that is not unreserved ASCII.
///
/// A filename is arbitrary Unicode — it can hold a newline, a colon, or a
/// right-to-left override — and an HTTP header value is a narrow ASCII grammar.
/// Encoding is the difference between carrying a Japanese filename and
/// truncating the response at the first byte the grammar rejects.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char);
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(value) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(value);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    // Invalid UTF-8 becomes replacement characters rather than an error: a
    // filename is metadata, and a mangled one must not fail the upload of
    // bytes that are perfectly fine.
    String::from_utf8_lossy(&out).into_owned()
}

// ------------------------------------------------------------------ shared

fn now() -> Timestamp {
    Timestamp::now()
}

fn invalid_id() -> ApiError {
    ApiError::new(Code::InvalidId, "not a uuid")
}

fn invalid_entity() -> ApiError {
    ApiError::new(
        Code::InvalidRequest,
        "not an entity reference: expected <kind>:<workspace>:<document>:<node>",
    )
}

/// Take the store lock, turning a poisoned mutex into an error rather than a
/// second panic.
///
/// A poisoned lock means a handler panicked while holding it, so the store's
/// in-memory state may be inconsistent with the disk. Reporting it is honest;
/// `unwrap()` here would turn one bug into every subsequent request panicking.
fn lock(state: &Arc<AppState>) -> ApiResult<std::sync::MutexGuard<'_, crate::state::KoruStore>> {
    state.store.lock().map_err(|_| {
        ApiError::from(Error::new(
            Code::Internal,
            "the store lock was poisoned by an earlier failure",
        ))
    })
}

#[cfg(test)]
mod cloud_coverage_tests;
mod cloud_status;
#[cfg(test)]
mod cloud_waiting_tests;
#[cfg(test)]
mod transport_mode_tests;
#[cfg(test)]
mod version_tests;
