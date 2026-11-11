// SPDX-License-Identifier: MPL-2.0

//! Existing root authority and trusted immutable-history capture for Bot intent.
use koru_ai_core::bots::*;
use koru_ai_core::{ProfileId, WorkspaceId};
use koru_store_server::StoreWorkerClient;

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
    fn capture(
        &self,
        owner: BotOwner,
        workspace: WorkspaceId,
        profile: ProfileId,
        pages: &[BotPage],
    ) -> Result<Vec<koru_delegate_core::RunContextItem>, BotError> {
        if owner.device != self.device
            || pages.is_empty()
            || pages.len() > 17
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
    /// before this call: a lost reply replays exactly them, and after the
    /// Store's own receipt expires its retained origin still lets the
    /// original first update win. Human edits made since stay current.
    fn create_role_page(&self, seed: &BotRoleSeed) -> Result<BotRoleProof, BotError> {
        if seed.owner.device != self.device {
            return Err(BotError::Forbidden);
        }
        let seed = seed.clone();
        self.client
            .with_store(move |store| {
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
                store
                    .create_seeded_document_with_origin(
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
                    )
                    .map_err(|error| match error.code() {
                        koru_store_core::errors::Code::IdempotencyConflict => BotError::Conflict,
                        // Once the seed transaction begins, any error may
                        // follow durable bytes: the same candidate replays.
                        _ => BotError::Unavailable,
                    })?;
                let origin = match store
                    .creation_origin(seed.workspace, seed.candidate)
                    .map_err(|_| BotError::Unavailable)?
                {
                    koru_store_core::CreationOriginLookup::Known(origin) => origin,
                    _ => return Err(BotError::Conflict),
                };
                let page_live = store
                    .document_entry_live(seed.workspace, seed.candidate)
                    .map_err(|_| BotError::Unavailable)?
                    .is_some_and(|entry| entry.kind == koru_core::explore::KIND_DOCUMENT);
                Ok(BotRoleProof {
                    scope: origin.scope,
                    request_key: origin.request_key,
                    request_hash: origin.request_hash,
                    initial_update: origin.initial_update,
                    page_live,
                })
            })
            .map_err(|_| BotError::Unavailable)?
    }
}

fn trusted_device(value: &str) -> Option<DeviceId> {
    let id = uuid::Uuid::parse_str(value).ok()?;
    (!id.is_nil()).then(|| DeviceId::from_uuid(id))
}
#[cfg(test)]
mod tests;
