// SPDX-License-Identifier: MPL-2.0

//! New storage is durable before any catalog row or wake exposes it.

use super::*;

#[derive(Clone, Debug)]
pub struct FirstWorkspace {
    pub name: String,
    pub icon: Option<String>,
    pub color: Option<String>,
}

#[derive(Clone, Debug)]
pub struct CreateProfileWithWorkspace {
    pub id: ProfileId,
    pub name: String,
    pub locale: Locale,
    pub purpose: Option<String>,
    pub avatar: Option<String>,
    pub icon: Option<String>,
    pub color: Option<String>,
    pub workspace: FirstWorkspace,
}

#[derive(Clone, Debug)]
pub struct CreatedProfile {
    pub profile: Profile,
    pub workspace: Workspace,
}

pub(super) struct Prepared<W> {
    pub workspace: Workspace,
    pub open: Open<W>,
    committed: crate::commit_signal::Commits,
    dirty: HashMap<WorkspaceId, BTreeSet<DocumentId>>,
}

impl<C: Catalog, B: Backend> Store<C, B> {
    pub fn create_profile_with_workspace(
        &mut self,
        request: &CreateProfileWithWorkspace,
        at: Timestamp,
    ) -> Result<CreatedProfile> {
        if let Some(existing) = self.catalog.profile_including_removed(request.id)? {
            if self.catalog.profile_catalog_state(request.id)? == Some(ProfileCatalogState::Active)
                && self
                    .catalog
                    .state(&profile_origin_key(request.id))?
                    .as_deref()
                    == Some(ORIGIN_CREATED_HERE)
                && existing.name == request.name
                && let Some(workspace) = self.catalog.workspaces()?.into_iter().find(|w| {
                    w.owner == request.id
                        && w.name == request.workspace.name
                        && !w.archived
                        && w.deleted_at.is_none()
                })
            {
                self.flush_workspace_manifest(workspace.id, at)?;
                let WorkspaceManifestInspection::Verified(authority) =
                    self.backend.inspect_workspace_manifest(workspace.id)
                else {
                    return Err(Error::bare(Code::ProfileExists));
                };
                if authority.name != request.workspace.name
                    || authority.archived
                    || authority.deleted_at.is_some()
                {
                    return Err(Error::bare(Code::ProfileExists));
                }
                let workspace = self.reconcile_verified_workspace(&workspace, &authority, at)?;
                return Ok(CreatedProfile {
                    profile: existing,
                    workspace,
                });
            }
            return Err(Error::bare(Code::ProfileExists));
        }
        let mut profile = Profile::new(&request.name, request.locale.clone(), at)?
            .with_purpose(request.purpose.as_deref())
            .with_avatar(request.avatar.as_deref())
            .with_appearance(request.icon.as_deref(), request.color.as_deref());
        profile.id = request.id;
        let identity = IdentityManifest::create(&profile)?;
        let private = PrivateManifest::create(profile.id)?;
        identity.set_peer_id(self.peer_id)?;
        private.set_peer_id(self.peer_id)?;
        // Export before preparing storage: an export failure owns no repository.
        let identity_bytes = identity.export_snapshot()?;
        let private_bytes = private.export_snapshot()?;
        let prepared = self.prepare_workspace(
            profile.id,
            &request.workspace.name,
            request.workspace.icon.as_deref(),
            request.workspace.color.as_deref(),
            at,
        )?;
        if let Err(error) = self.catalog.publish_profile_with_workspace(
            &profile,
            &identity_bytes,
            &private_bytes,
            (&profile_origin_key(profile.id), ORIGIN_CREATED_HERE),
            &prepared.workspace,
            prepared.open.manifest_id,
        ) {
            self.discard_prepared(prepared);
            return Err(error);
        }
        self.open_profiles.insert(profile.id, identity);
        self.private_profiles.insert(profile.id, private);
        let workspace = self.adopt_prepared(prepared);
        Ok(CreatedProfile { profile, workspace })
    }

    pub(super) fn prepare_workspace(
        &mut self,
        owner: ProfileId,
        name: &str,
        icon: Option<&str>,
        color: Option<&str>,
        at: Timestamp,
    ) -> Result<Prepared<B::Workspace>> {
        let mut workspace = Workspace::new(owner, name, at)?;
        workspace.position = self.next_workspace_position(owner)?;
        workspace.icon = icon.map(str::to_owned);
        workspace.color = color.map(str::to_owned);
        let id = workspace.id;
        // A failure here has no cleanup permission. The backend only grants
        // ownership on success, and refuses pre-existing repositories.
        let repo = self.backend.create_workspace(id)?;
        let result = (|| {
            let manifest_id = DocumentId::new();
            let mut repo = repo;
            repo.create_document(manifest_id, DocumentKind::Manifest, at)?;
            let manifest = Manifest::create(id, name, at)?;
            manifest.set_peer_id(self.peer_id)?;
            manifest.set_appearance(icon, color, false, false)?;
            let mut open = Open {
                repo,
                manifest,
                documents: HashMap::new(),
                windows: HashMap::new(),
                manifest_id,
            };
            let mut committed = crate::commit_signal::Commits::default();
            let mut dirty = HashMap::new();
            let whole = open.manifest.export_all_updates()?;
            commit_one(
                &mut open,
                id,
                &mut committed,
                &mut dirty,
                manifest_id,
                whole,
                at,
                crate::ports::UpdateOrigin::Store,
            )?;
            open.repo.reindex(&open.manifest.entries())?;
            Ok(Prepared {
                workspace,
                open,
                committed,
                dirty,
            })
        })();
        if result.is_err() {
            // The closure has dropped every database handle before deletion.
            self.cleanup_fresh_repository(id);
        }
        result
    }

    pub(super) fn discard_prepared(&self, prepared: Prepared<B::Workspace>) {
        let id = prepared.workspace.id;
        drop(prepared);
        self.cleanup_fresh_repository(id);
    }

    fn cleanup_fresh_repository(&self, id: WorkspaceId) {
        if let Err(error) = self.backend.delete_workspace(id) {
            tracing::warn!(workspace = %id, error = %error,
                "could not clean up this operation's unpublished workspace");
        }
    }

    pub(super) fn adopt_prepared(&mut self, mut prepared: Prepared<B::Workspace>) -> Workspace {
        let id = prepared.workspace.id;
        self.open.insert(id, prepared.open);
        for record in prepared.committed.take() {
            self.committed
                .insert((record.workspace, record.document), record);
        }
        for (workspace, documents) in prepared.dirty {
            self.sync_dirty
                .entry(workspace)
                .or_default()
                .extend(documents);
        }
        prepared.workspace
    }
}
