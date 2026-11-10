// SPDX-License-Identifier: MPL-2.0

//! Profile/Workspace-owned state inside the desktop shell.
//!
//! The winit application owns windows, rendering, wake plumbing and service
//! availability. This value owns the content identity which those surfaces
//! project. It is deliberately non-optional: Plan 3.3 can place the whole value
//! behind `ContentState::Open` without making ordinary editing code negotiate a
//! scatter of optional Profile and Workspace fields.

use std::path::PathBuf;
use std::sync::mpsc;

use koru_core::{ProfileId, ProfileSummary, WorkspaceId};
use koru_workspace::Workspace;

use crate::binding_markers::PendingBindings;
use crate::profile_activation;
use crate::profile_names::ProfileNames;
use crate::settings::{ProfileSettings, Written};

/// Persistence for the transitional `ProfileDoc` view.
///
/// Store-backed Profiles are rebuilt from the Store catalog and therefore have
/// no legacy folder session to publish. The legacy variant remains for the
/// explicitly supported folder paths until their separate retirement decision.
pub(crate) enum ProfilePersistence {
    Store,
    #[allow(dead_code)] // Explicit legacy folder paths retain this owner.
    Legacy(koru_workspace::profile::ProfileSession),
}

impl ProfilePersistence {
    pub(crate) fn flush(&mut self, profile: &koru_core::profile::ProfileDoc) -> Result<(), String> {
        match self {
            Self::Store => Ok(()),
            Self::Legacy(session) => session.flush(profile),
        }
    }

    pub(crate) fn tick(&mut self, profile: &mut koru_core::profile::ProfileDoc) -> bool {
        match self {
            Self::Store => false,
            Self::Legacy(session) => session.tick(profile),
        }
    }

    pub(crate) fn save(&mut self, profile: &koru_core::profile::ProfileDoc) {
        if let Self::Legacy(session) = self {
            session.save(profile);
        }
    }
}

/// One periodic Store catalog read, kept together so profile and workspace
/// chrome can never show snapshots from different polls.
pub(crate) struct StoreCatalogRefresh {
    pub(crate) profiles: Vec<ProfileSummary>,
    pub(crate) workspaces: Vec<koru_store_core::model::Workspace>,
}

/// Blob bytes for document export, fetched from the Store on the worker thread.
pub(crate) struct StoreBlobSource {
    pub(crate) client: koru_api_client::Client,
    pub(crate) workspace: koru_store_core::ids::WorkspaceId,
}

impl koru_export::BlobSource for StoreBlobSource {
    fn get(&self, hash: &koru_core::node::BlobHash) -> Result<Vec<u8>, koru_export::ExportError> {
        let parsed = koru_store_core::model::BlobHash::parse(&hash.0)
            .map_err(|error| koru_export::ExportError::new(error.to_string()))?;
        self.client
            .get_blob(self.workspace, &parsed)
            .map_err(|error| koru_export::ExportError::new(error.to_string()))
    }
}

/// Blob destination for document import, backed by the current Store
/// workspace. Format crates see only `BlobSink`, never the HTTP client.
pub(crate) struct StoreBlobSink {
    pub(crate) client: koru_api_client::Client,
    pub(crate) workspace: koru_store_core::ids::WorkspaceId,
}

impl koru_import::BlobSink for StoreBlobSink {
    fn put(
        &self,
        bytes: Vec<u8>,
        mime: &str,
        filename: Option<&str>,
    ) -> Result<koru_import::StoredBlob, koru_import::ImportError> {
        let blob = self
            .client
            .put_blob(self.workspace, bytes, mime, filename)
            .map_err(|error| koru_import::ImportError::new(error.to_string()))?;
        Ok(koru_import::StoredBlob {
            hash: blob.hash.as_str().to_owned(),
            mime: blob.mime,
        })
    }
    fn put_from(
        &self,
        factory: &(dyn Fn() -> Box<dyn std::io::Read + Send> + Send + Sync),
        mime: &str,
        filename: Option<&str>,
    ) -> Result<koru_import::StoredBlob, koru_import::ImportError> {
        let blob = self
            .client
            .put_blob_from(self.workspace, factory, mime, filename)
            .map_err(|error| koru_import::ImportError::new(error.to_string()))?;
        Ok(koru_import::StoredBlob {
            hash: blob.hash.as_str().to_owned(),
            mime: blob.mime,
        })
    }
}

/// Everything whose meaning is scoped to the open Profile and Workspace.
pub(crate) struct ProfileRuntime {
    /// Temporary legacy folder owner. Removed with the scaffold in Plan 3.3.
    pub(crate) profile_dir: PathBuf,
    pub(crate) profile: koru_core::profile::ProfileDoc,
    pub(crate) profile_catalog: Vec<ProfileSummary>,
    pub(crate) catalog_workspaces: Vec<koru_store_core::model::Workspace>,
    pub(crate) profile_session: ProfilePersistence,
    pub(crate) profile_activation: profile_activation::Inbox,
    pub(crate) workspace: Workspace,
    pub(crate) current: WorkspaceId,
    pub(crate) live_workspace: bool,
    pub(crate) settings: ProfileSettings,
    pub(crate) written: Written,
    pub(crate) profile_refresh_tx: mpsc::SyncSender<()>,
    pub(crate) profile_refresh_rx: mpsc::Receiver<Result<StoreCatalogRefresh, String>>,
    pub(crate) profile_refresh_pending: bool,
    pub(crate) profile_refresh_error: Option<String>,
    /// The Profile-to-account bindings asked of the Agent and not yet
    /// answered, mirrored to disk off this thread.
    pub(crate) pending_bindings: PendingBindings,
    /// Every Profile name this device has seen, remembered locally so a
    /// deletion carried here from another device — which names no Profile and
    /// arrives after the catalog row has gone — can still be worded with one.
    /// Device-local and never sent: see [`crate::profile_names`].
    pub(crate) profile_names: ProfileNames,
}

impl ProfileRuntime {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        profile_dir: PathBuf,
        profile: koru_core::profile::ProfileDoc,
        profile_catalog: Vec<ProfileSummary>,
        profile_session: ProfilePersistence,
        profile_activation: profile_activation::Inbox,
        workspace: Workspace,
        current: WorkspaceId,
        live_workspace: bool,
        settings: ProfileSettings,
        written: Written,
        profile_refresh_tx: mpsc::SyncSender<()>,
        profile_refresh_rx: mpsc::Receiver<Result<StoreCatalogRefresh, String>>,
        pending_bindings: PendingBindings,
    ) -> Self {
        debug_assert_eq!(profile.id(), Some(settings.profile_id()));
        // The first listing of the launch is the first chance to remember what
        // it names, and it costs a write only when a name changed.
        let mut profile_names = ProfileNames::open(profile_dir.clone());
        profile_names.observe(&profile_catalog);
        Self {
            profile_dir,
            profile,
            profile_catalog,
            catalog_workspaces: Vec::new(),
            profile_session,
            profile_activation,
            workspace,
            current,
            live_workspace,
            settings,
            written,
            profile_refresh_tx,
            profile_refresh_rx,
            profile_refresh_pending: false,
            profile_refresh_error: None,
            pending_bindings,
            profile_names,
        }
    }

    /// Take the names from the catalog this runtime is holding.
    ///
    /// Called wherever [`Self::profile_catalog`] is replaced. A listing that
    /// names nothing new writes nothing.
    pub(crate) fn remember_profile_names(&mut self) {
        self.profile_names.observe(&self.profile_catalog);
    }

    pub(crate) fn profile_id(&self) -> ProfileId {
        self.profile
            .id()
            .expect("ProfileRuntime is constructed only for a real Profile")
    }

    /// Start Profile-session events after Store availability is proven, and
    /// take the first reading of this device's telemetry answer beside them.
    pub(crate) fn start_events(
        gateway_root: Option<PathBuf>,
        proxy: winit::event_loop::EventLoopProxy<crate::app::Wake>,
    ) {
        crate::telemetry_consent::install(proxy, gateway_root.clone());
        crate::events_host::start(gateway_root);
    }

    /// The unchanged workspace hello payload for Agent and scanner routing.
    pub(crate) fn agent_showing(&self) -> (ProfileId, WorkspaceId, String, String) {
        let profile_name = self.profile.name().unwrap_or_default();
        let workspace_name = self
            .profile
            .workspace(self.current)
            .map(|workspace| workspace.name)
            .unwrap_or_default();
        (
            self.profile_id(),
            self.current,
            profile_name,
            workspace_name,
        )
    }

    pub(crate) fn rebind_activation(&mut self, profile: ProfileId) {
        self.profile_activation = profile_activation::Inbox::bind(Some(profile));
    }

    /// Ask the bounded worker for a fresh Store catalog, coalescing requests.
    pub(crate) fn request_catalog_refresh(&mut self) {
        if self.profile_refresh_pending {
            return;
        }
        match self.profile_refresh_tx.try_send(()) {
            Ok(()) | Err(mpsc::TrySendError::Full(())) => {
                self.profile_refresh_pending = true;
            }
            Err(mpsc::TrySendError::Disconnected(())) => {}
        }
    }

    /// Adopt only the newest queued Store catalog without blocking the loop.
    /// Returns whether Profile/Workspace chrome needs republishing.
    pub(crate) fn drain_catalog(&mut self) -> bool {
        let mut latest = None;
        while let Ok(result) = self.profile_refresh_rx.try_recv() {
            self.profile_refresh_pending = false;
            latest = Some(result);
        }
        match latest {
            Some(Ok(refresh)) => {
                self.profile_refresh_error = None;
                let mut changed = false;
                if refresh.workspaces != self.catalog_workspaces {
                    self.catalog_workspaces = refresh.workspaces.clone();
                    changed = true;
                }
                if refresh.profiles != self.profile_catalog {
                    self.profile_catalog = refresh.profiles;
                    // While the rows are still here: the names are kept for
                    // the notice a deletion elsewhere will need after they
                    // are not (see [`crate::profile_names`]).
                    self.remember_profile_names();
                    changed = true;
                }
                if let Some(owner) = self.profile.id() {
                    match refresh_open_profile_name(&mut self.profile, owner, &self.profile_catalog)
                    {
                        Ok(renamed) => changed |= renamed,
                        Err(error) => self.profile_refresh_error = Some(error),
                    }
                    let before = self.profile.workspaces();
                    if let Err(error) = koru_workspace::fromstore::apply_workspace_catalog(
                        &mut self.profile,
                        owner,
                        &refresh.workspaces,
                    ) {
                        self.profile_refresh_error = Some(error.to_string());
                    } else if self.profile.workspaces() != before {
                        changed = true;
                    }
                }
                changed
            }
            Some(Err(error)) => {
                if self.profile_refresh_error.as_deref() != Some(&error) {
                    eprintln!("[Koru][profiles] Store catalog refresh failed: {error}");
                    self.profile_refresh_error = Some(error);
                }
                false
            }
            None => false,
        }
    }

    pub(crate) fn active_workspace_counts(&self) -> std::collections::HashMap<ProfileId, usize> {
        let mut counts = self
            .profile_catalog
            .iter()
            .map(|profile| (profile.id, 0usize))
            .collect::<std::collections::HashMap<_, _>>();
        for workspace in self
            .catalog_workspaces
            .iter()
            .filter(|workspace| workspace.deleted_at.is_none())
        {
            let owner = ProfileId::from_uuid(workspace.owner.as_uuid());
            *counts.entry(owner).or_default() += 1;
        }
        counts
    }
    /// Every Profile's workspaces, from the same catalog the counts come from,
    /// in the Store's order. The open Profile is included; its live view is
    /// fresher and the pane prefers that one.
    pub(crate) fn workspaces_by_profile(
        &self,
    ) -> std::collections::HashMap<ProfileId, Vec<koru_core::WorkspaceEntry>> {
        let mut out: std::collections::HashMap<ProfileId, Vec<koru_core::WorkspaceEntry>> =
            std::collections::HashMap::new();
        for workspace in &self.catalog_workspaces {
            let owner = ProfileId::from_uuid(workspace.owner.as_uuid());
            out.entry(owner)
                .or_default()
                .push(koru_workspace::fromstore::workspace_entry(workspace));
        }
        for rows in out.values_mut() {
            rows.sort_by_key(|row| (row.order, row.created));
        }
        out
    }

    /// The name to put in a message when the row may just have left the list.
    pub(crate) fn workspace_name(&self, id: WorkspaceId) -> String {
        self.profile
            .workspace(id)
            .map(|workspace| workspace.name)
            .unwrap_or_else(|| "that workspace".to_string())
    }

    /// The nearest ordinary workspace after `id`, then before it.
    pub(crate) fn replacement_workspace(&self, id: WorkspaceId) -> Option<WorkspaceId> {
        let rows = self.profile.workspaces();
        let position = rows.iter().position(|row| row.id == id).unwrap_or(0);
        rows[position.saturating_add(1)..]
            .iter()
            .chain(rows[..position].iter().rev())
            .find(|row| row.id != id && !row.archived && row.deleted_at.is_none())
            .map(|row| row.id)
    }
}

/// Bring the open Profile's name in the view back in line with its Store row.
///
/// **The open Profile's name is read off this view, not off the catalog.**
/// `App::sync_pane_context` overlays `ProfileDoc::summary()` on top of the row
/// it has just read, and `App::sync_titles` reads the view directly, so a
/// rename that arrived from a peer or the Cloud reached the catalog and stopped
/// there: the switcher and the window title kept the old name for as long as
/// the window stayed open. Every other Profile in the switcher was already
/// correct, which is what made this look like a sync failure rather than a
/// stale view.
///
/// The view is a projection of the Store, exactly like the workspace rows
/// beside it (see `fromstore::apply_workspace_catalog`), so this is a refresh
/// and never an edit somebody made. Returns whether anything moved.
fn refresh_open_profile_name(
    profile: &mut koru_core::profile::ProfileDoc,
    owner: ProfileId,
    catalog: &[ProfileSummary],
) -> Result<bool, String> {
    let Some(row) = catalog.iter().find(|row| row.id == owner) else {
        // A refresh can race profile creation; the next one owns the list.
        return Ok(false);
    };
    if profile.name().as_deref() == Some(row.name.as_str()) {
        return Ok(false);
    }
    profile
        .set_name(&row.name)
        .map(|()| true)
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(id: ProfileId, name: &str) -> ProfileSummary {
        ProfileSummary {
            id,
            name: name.into(),
            email: String::new(),
            avatar: None,
            icon: None,
            color: None,
            account: None,
            folder: String::new(),
        }
    }

    /// The defect this closes: a Profile renamed on another device arrives in
    /// the catalog, and the name the desktop actually draws must follow it.
    #[test]
    fn a_renamed_profile_row_refreshes_the_name_the_shell_draws() {
        let id = ProfileId::new();
        let mut profile = koru_core::profile::ProfileDoc::create(id, "Before", None).unwrap();
        assert!(
            refresh_open_profile_name(&mut profile, id, &[summary(id, "After")]).unwrap(),
            "a differing catalog row must move the view"
        );
        assert_eq!(profile.name().as_deref(), Some("After"));
        // Idempotent: an unchanged row is not a redraw, and the two-second
        // catalog poll runs this on every tick.
        assert!(!refresh_open_profile_name(&mut profile, id, &[summary(id, "After")]).unwrap());
        // Another Profile's row is not this one's.
        assert!(
            !refresh_open_profile_name(
                &mut profile,
                id,
                &[summary(ProfileId::new(), "Somebody else")]
            )
            .unwrap()
        );
        assert_eq!(profile.name().as_deref(), Some("After"));
    }
}
