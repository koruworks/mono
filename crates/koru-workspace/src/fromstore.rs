// SPDX-License-Identifier: MPL-2.0

//! Opening the app on the Koru Store instead of a profile folder.
//!
//! The counterpart of [`open::open_profile`](crate::open::open_profile), which
//! reads a folder. Everything here comes over the wire.
//!
//! ## The `ProfileDoc` is a view of the store, not a document
//!
//! `App` reaches through a `ProfileDoc` in sixty-odd places — the window title,
//! the workspace switcher, the actor label on every trash action, the key that
//! window geometry is filed under. Most of those are deletions once the folder
//! world goes, and deleting them is step 4's work rather than this step's.
//!
//! So the `ProfileDoc` returned here is **built in memory from what the store
//! says, and never written anywhere**. There is no `ProfileSession` beside it,
//! nothing calls `save`, and no `profile/` directory is created. It is a
//! view-model wearing a document's type, which is what lets the switcher list
//! store workspaces without sixty edits landing in the same commit as the
//! transport swap.
//!
//! That is a transitional shape and it is worth saying so plainly: it will read
//! as "the profile document is still alive" to anyone who does not look. It is
//! not. Nothing loads it, nothing persists it, and it is rebuilt from the store
//! every launch.

use std::path::Path;

use koru_api_client::Client;
use koru_core::profile::ProfileDoc;
use koru_core::schema::WorkspaceDoc;
use koru_core::{ProfileId, ProfileSummary, WorkspaceEntry, WorkspaceId};
use koru_store_core::errors::{Code, Error, Result};
use koru_store_core::model::Profile;

use crate::Workspace;
use crate::explore::store_workspace;

/// What a store-backed launch produces.
///
/// `NeedsProfile` is returned directly from the empty Profile catalog. It owns
/// no fallback identity or document, so merely asking what to launch cannot
/// mint an id or make the Store non-empty.
pub enum StoreLaunch {
    NeedsProfile,
    Open(OpenedFromStore),
}

impl StoreLaunch {
    pub fn into_open(self) -> Option<OpenedFromStore> {
        match self {
            Self::NeedsProfile => None,
            Self::Open(opened) => Some(opened),
        }
    }
}

/// An open, real Store Profile and its selected Workspace state.
pub struct OpenedFromStore {
    /// Every profile the Store knows, for the profile switcher. Folder is empty
    /// by design: Store profiles are personas, not paths on this machine.
    pub profiles: Vec<ProfileSummary>,
    /// A view of the store's profile and its workspaces. Never saved.
    pub profile: ProfileDoc,
    /// The workspace, already listing its documents from the manifest.
    pub workspace: Workspace,
    /// Which workspace was opened.
    pub current: WorkspaceId,
    /// Whether `current` names Store content. False means `workspace` is the
    /// local welcome placeholder used when this profile has no active rows.
    pub has_workspace: bool,
}

/// What follows a successful, durable local Profile creation.
///
/// `AcknowledgedButNotOpened` is deliberately not an `Err`: the Store has
/// already committed the Profile, so submitting creation again could make a
/// duplicate. The shell may keep onboarding live and retry the strict open, or
/// let the next ordinary launch discover the acknowledged Profile.
pub enum LocalProfileCreation {
    Open(OpenedFromStore),
    AcknowledgedButNotOpened { profile: ProfileId, error: Error },
}

/// The Store's profile catalog, shaped for the transitional chrome API.
///
/// This is a network call. Shell callers refresh it on a worker thread rather
/// than from winit's maintenance loop.
pub fn profiles(segment: &str, root: Option<&Path>) -> Result<Vec<ProfileSummary>> {
    let client = match root {
        Some(root) => Client::open_at(segment, root),
        None => Client::open(segment),
    }?;
    profiles_with_client(&client)
}

/// Read the profile catalog through an already authenticated Store client.
///
/// The shell refreshes this list every two seconds. Reusing its worker's client
/// avoids minting a twelve-hour Store session for every poll while retaining
/// [`profiles`] for one-shot callers.
fn personas_only(profiles: Vec<Profile>) -> Vec<Profile> {
    profiles
        .into_iter()
        .filter(|profile| !profile.is_holder())
        .collect()
}

pub fn profiles_with_client(client: &Client) -> Result<Vec<ProfileSummary>> {
    // Personas only — this list is what a switcher draws. See the same filter
    // and the same argument in `open_from_store_with_policy`.
    Ok(personas_only(client.profiles()?)
        .iter()
        .map(profile_summary)
        .collect())
}

/// Create one local-only Profile through the ordinary Store API.
///
/// A successful return is the Store's durability acknowledgement. Callers must
/// not construct a [`ProfileDoc`] or enter the open shell before it arrives;
/// an error leaves them in `NeedsProfile`, where retry and cancel are harmless.
pub fn create_local_profile(segment: &str, root: Option<&Path>, name: &str) -> Result<ProfileId> {
    create_local_profile_with_avatar(segment, root, name, None)
}

/// Create the onboarding Profile with its chosen avatar in the same durable
/// Store operation. Purpose is deliberately not persisted: it only influences
/// which starter templates the form marks as suggested.
pub fn create_local_profile_with_avatar(
    segment: &str,
    root: Option<&Path>,
    name: &str,
    avatar: Option<&str>,
) -> Result<ProfileId> {
    let client = match root {
        Some(root) => Client::open_at(segment, root),
        None => Client::open(segment),
    }?;
    let profile = client.create_profile(name, None, avatar, None)?;
    Ok(ProfileId::from_uuid(profile.id.as_uuid()))
}

/// Create an onboarding Profile with the same emoji/color appearance the full
/// application edits later.
pub fn create_local_profile_with_appearance(
    segment: &str,
    root: Option<&Path>,
    name: &str,
    icon: Option<&str>,
    color: Option<&str>,
) -> Result<ProfileId> {
    let client = match root {
        Some(root) => Client::open_at(segment, root),
        None => Client::open(segment),
    }?;
    let profile = client.create_profile_with_appearance(name, None, None, icon, color, None)?;
    Ok(ProfileId::from_uuid(profile.id.as_uuid()))
}

/// Create a local Profile and return its open state only after the Store has
/// acknowledged that creation durably.
///
/// This is the transition seam for onboarding. There is no optimistic
/// `ProfileDoc`: an error from `create_local_profile` returns before opening,
/// and the exact acknowledged id is used for the strict follow-up read.
pub fn create_local_profile_and_open(
    segment: &str,
    root: Option<&Path>,
    name: &str,
) -> Result<LocalProfileCreation> {
    let profile = create_local_profile(segment, root, name)?;
    Ok(open_acknowledged_profile(profile, || {
        open_from_store_profile_preferring_empty(segment, root, profile, None)
    }))
}

fn open_acknowledged_profile(
    profile: ProfileId,
    open: impl FnOnce() -> Result<StoreLaunch>,
) -> LocalProfileCreation {
    match open() {
        Ok(StoreLaunch::Open(opened)) => LocalProfileCreation::Open(opened),
        Ok(StoreLaunch::NeedsProfile) => LocalProfileCreation::AcknowledgedButNotOpened {
            profile,
            error: Error::new(
                Code::Internal,
                "acknowledged Profile was absent from the strict follow-up open",
            ),
        },
        Err(error) => LocalProfileCreation::AcknowledgedButNotOpened { profile, error },
    }
}

/// Refresh the transitional profile view from Store workspace rows.
///
/// This view is never persisted; it exists only while shell call sites still
/// expect a `ProfileDoc`. Store rows and replicated manifest state remain
/// authoritative, so a refresh replaces names and lifecycle flags rather than
/// treating the view's previous values as edits.
pub fn apply_workspace_catalog(
    profile: &mut ProfileDoc,
    owner: ProfileId,
    rows: &[koru_store_core::model::Workspace],
) -> Result<()> {
    for w in rows.iter().filter(|w| w.owner.as_uuid() == owner.as_uuid()) {
        let id = WorkspaceId::from_uuid(w.id.as_uuid());
        if profile.workspace(id).is_none() {
            profile
                .ensure_workspace(id, &w.name, w.created_at.millis())
                .map_err(|e| Error::new(Code::Internal, format!("workspace view: {e}")))?;
        }

        let current = profile
            .workspace(id)
            .expect("workspace was present or was just ensured");
        let deleted_at = w.deleted_at.map(|at| at.millis());
        let view = &mut *profile;
        if current.name != w.name {
            view.rename_workspace(id, &w.name)
                .map_err(|e| Error::new(Code::Internal, format!("workspace view: {e}")))?;
        }
        if current.icon != w.icon {
            view.set_workspace_icon(id, w.icon.as_deref())
                .map_err(|e| Error::new(Code::Internal, format!("workspace view: {e}")))?;
        }
        if current.color != w.color {
            view.set_workspace_color(id, w.color.as_deref())
                .map_err(|e| Error::new(Code::Internal, format!("workspace view: {e}")))?;
        }
        if current.pinned != w.pinned {
            view.set_workspace_pinned(id, w.pinned)
                .map_err(|e| Error::new(Code::Internal, format!("workspace view: {e}")))?;
        }
        if current.archived != w.archived {
            view.set_workspace_archived(id, w.archived)
                .map_err(|e| Error::new(Code::Internal, format!("workspace view: {e}")))?;
        }
        if current.deleted_at != deleted_at {
            view.set_workspace_deleted_at(id, deleted_at)
                .map_err(|e| Error::new(Code::Internal, format!("workspace view: {e}")))?;
        }
        if current.order != w.position {
            view.set_workspace_order(id, w.position)
                .map_err(|e| Error::new(Code::Internal, format!("workspace view: {e}")))?;
        }
    }
    Ok(())
}

/// One Store workspace row as the switcher's entry. The same fields
/// [`apply_workspace_catalog`] copies into the open Profile's view, for a
/// Profile that is not open and so has no view to copy into.
pub fn workspace_entry(w: &koru_store_core::model::Workspace) -> WorkspaceEntry {
    WorkspaceEntry {
        id: WorkspaceId::from_uuid(w.id.as_uuid()),
        name: w.name.clone(),
        icon: w.icon.clone(),
        color: w.color.clone(),
        pinned: w.pinned,
        archived: w.archived,
        deleted_at: w.deleted_at.map(|at| at.millis()),
        order: w.position,
        created: w.created_at.millis(),
    }
}

fn profile_summary(profile: &koru_store_core::model::Profile) -> ProfileSummary {
    ProfileSummary {
        id: ProfileId::from_uuid(profile.id.as_uuid()),
        name: profile.name.clone(),
        // `ProfileSummary` predates the Store and calls its secondary label an
        // email. Purpose is the Store field the picker should show until that
        // transitional view-model is replaced.
        email: profile.purpose.clone().unwrap_or_default(),
        // Store avatars are typed tokens (`color:`, later `blob:`), while this
        // old field means a blob hash. Treating one as the other would make a
        // colour token look like a missing image.
        avatar: None,
        icon: profile.icon.clone(),
        color: profile.color.clone().or_else(|| {
            profile
                .avatar
                .as_deref()
                .and_then(|avatar| avatar.strip_prefix("color:"))
                .map(str::trim)
                .filter(|color| !color.is_empty())
                .map(str::to_owned)
        }),
        account: profile.account.clone(),
        folder: String::new(),
    }
}

/// This build's own channel — the segment a store is discovered on.
///
/// Asked rather than written down: both this binary and the store it talks to
/// are built by the same cargo invocation, so a `--release` build moves both to
/// `internal-release` together and a hardcoded string would move neither.
pub fn segment() -> &'static str {
    koru_channel::segment()
}

/// Create a workspace in the store, owned by the profile that is open.
///
/// **Not `open::create_workspace`**, which makes a folder and registers it in a
/// `ProfileDoc`. Under the store that document is a view rebuilt every launch,
/// so a workspace created there existed only in the window: it appeared in the
/// switcher, and every later call naming it answered `404` because the store had
/// never been told.
pub fn create_workspace(
    segment: &str,
    root: Option<&Path>,
    owner: ProfileId,
    name: &str,
) -> Result<WorkspaceId> {
    let client = match root {
        Some(root) => Client::open_at(segment, root),
        None => Client::open(segment),
    }?;
    let created = client.create_workspace(
        koru_store_core::ids::ProfileId::from_uuid(owner.as_uuid()),
        name,
    )?;
    Ok(WorkspaceId::from_uuid(created.id.as_uuid()))
}

/// Create a profile in the store, and answer with its id.
///
/// **The call the open path used to make behind everybody's back**, now made
/// only where somebody asked for it: first-run's *Create profile* (S21). The
/// name is the same one that launch used to invent, which is what keeps a fresh
/// install's first persona identical to the one it has always had — the change
/// is who decided it should exist, not what it is called. Renaming it is the
/// persona switcher's job and always was.
pub fn create_profile(segment: &str, root: Option<&Path>, name: &str) -> Result<ProfileId> {
    let client = match root {
        Some(root) => Client::open_at(segment, root),
        None => Client::open(segment),
    }?;
    let created = client.create_profile(name, None, None, None)?;
    Ok(ProfileId::from_uuid(created.id.as_uuid()))
}

/// The name a profile nobody has named yet is created with.
///
/// One constant because two spellings of it would be two different first
/// personas depending on which route somebody took.
pub const DEFAULT_PROFILE_NAME: &str = "You";

/// Rename a workspace in the store.
///
/// A free function rather than a method on [`Workspace`], because the switcher
/// can rename a workspace that is not the one open — the store is asked
/// directly, and the in-memory view catches up from it.
pub fn rename_workspace(
    segment: &str,
    root: Option<&Path>,
    id: WorkspaceId,
    name: &str,
) -> Result<()> {
    let client = match root {
        Some(root) => Client::open_at(segment, root),
        None => Client::open(segment),
    }?;
    client.rename_workspace(store_workspace(id), name)?;
    Ok(())
}

pub fn rename_profile(segment: &str, root: Option<&Path>, id: ProfileId, name: &str) -> Result<()> {
    let client = match root {
        Some(root) => Client::open_at(segment, root),
        None => Client::open(segment),
    }?;
    client.rename_profile(
        koru_store_core::ids::ProfileId::from_uuid(id.as_uuid()),
        name,
    )?;
    Ok(())
}

pub fn set_profile_appearance(
    segment: &str,
    root: Option<&Path>,
    id: ProfileId,
    icon: Option<&str>,
    color: Option<&str>,
) -> Result<()> {
    let client = match root {
        Some(root) => Client::open_at(segment, root),
        None => Client::open(segment),
    }?;
    client.set_profile_appearance(
        koru_store_core::ids::ProfileId::from_uuid(id.as_uuid()),
        icon,
        color,
    )?;
    Ok(())
}

/// Set how a workspace appears in a switcher, in the store.
///
/// Beside [`rename_workspace`] and for the same reason: the switcher can change
/// a workspace that is not the one open, so the store is asked directly and the
/// in-memory view catches up from it on the next launch.
pub fn set_workspace_appearance(
    segment: &str,
    root: Option<&Path>,
    id: WorkspaceId,
    icon: Option<&str>,
    color: Option<&str>,
    pinned: bool,
    archived: bool,
) -> Result<()> {
    let client = match root {
        Some(root) => Client::open_at(segment, root),
        None => Client::open(segment),
    }?;
    client.set_workspace_appearance(store_workspace(id), icon, color, pinned, archived)?;
    Ok(())
}

/// Open the newest workspace the store holds, creating one if it holds none.
///
/// **Newest is the right answer only when nobody has a preference** — a first
/// run, or a test. A real launch has one, and goes through
/// [`open_from_store_preferring`]. Which workspace to reopen is a question
/// `Settings` answers today and will answer from the store once settings live
/// there; guessing cleverly here would be a second answer competing with it.
pub fn open_from_store(segment: &str, root: Option<&Path>) -> Result<StoreLaunch> {
    open_from_store_preferring(segment, root, None)
}

/// The same, but reopen the workspace somebody was last in.
///
/// **Newest-created is the wrong answer for a returning window.** Without a
/// preference this opens whichever workspace was made last, so making one
/// permanently changes what Koru opens on — and switching back to an older one
/// is forgotten the moment the window closes. `wanted` is what the shell
/// remembered; it is honoured only if that workspace is still there and still
/// this profile's, so a stale id from a deleted workspace falls back rather than
/// failing.
pub fn open_from_store_preferring(
    segment: &str,
    root: Option<&Path>,
    wanted: Option<WorkspaceId>,
) -> Result<StoreLaunch> {
    open_from_store_with_policy(segment, root, None, wanted, true)
}

/// App startup variant: an existing profile with no active workspaces opens on
/// the welcome screen instead of silently creating content.
pub fn open_from_store_preferring_empty(
    segment: &str,
    root: Option<&Path>,
    wanted: Option<WorkspaceId>,
) -> Result<StoreLaunch> {
    open_from_store_with_policy(segment, root, None, wanted, false)
}

/// App startup variant for a profile chosen from the Store-backed switcher.
///
/// An exact profile id never falls back to the newest profile. A stale menu row
/// is an honest `ProfileNotFound`, not permission to open somebody else.
/// Without a remembered workspace, open the first active row in that Profile.
pub fn open_from_store_profile_preferring_empty(
    segment: &str,
    root: Option<&Path>,
    selected_profile: ProfileId,
    wanted: Option<WorkspaceId>,
) -> Result<StoreLaunch> {
    open_from_store_with_policy(segment, root, Some(selected_profile), wanted, false)
}

fn open_from_store_with_policy(
    segment: &str,
    root: Option<&Path>,
    selected_profile: Option<ProfileId>,
    wanted: Option<WorkspaceId>,
    create_workspace_if_empty: bool,
) -> Result<StoreLaunch> {
    let client = match root {
        Some(root) => Client::open_at(segment, root),
        None => Client::open(segment),
    }?;

    // ---- the person
    // A system-owned holder may exist in databases opened by an earlier dev
    // build, but it is never a persona and must not enter onboarding, switching,
    // or account attachment.
    let profile_rows = personas_only(client.profiles()?);
    let profile_row = match selected_profile {
        Some(selected) => profile_rows
            .iter()
            .find(|profile| profile.id.as_uuid() == selected.as_uuid())
            .cloned()
            .ok_or_else(|| {
                Error::new(
                    Code::ProfileNotFound,
                    format!("profile {selected} is not in the Store"),
                )
            })?,
        None if profile_rows.is_empty() => return Ok(StoreLaunch::NeedsProfile),
        None => profile_rows
            .iter()
            .max_by_key(|profile| profile.created_at.millis())
            .cloned()
            .expect("non-empty profile catalog"),
    };
    let profiles = profile_rows.iter().map(profile_summary).collect();

    // ---- the workspace
    let mine: Vec<_> = client
        .workspaces()?
        .into_iter()
        .filter(|w| w.owner == profile_row.id)
        .collect();
    let workspace_row = match choose_workspace(&mine, wanted, selected_profile.is_some()).cloned() {
        Some(workspace) => Some(workspace),
        None if create_workspace_if_empty => {
            Some(client.create_workspace(profile_row.id, "My Workspace")?)
        }
        None => None,
    };

    let current = workspace_row
        .as_ref()
        .map(|workspace| WorkspaceId::from_uuid(workspace.id.as_uuid()))
        .unwrap_or_else(WorkspaceId::new);

    // ---- the profile document, as a view
    let mut profile = ProfileDoc::create(
        ProfileId::from_uuid(profile_row.id.as_uuid()),
        &profile_row.name,
        None,
    )
    .map_err(|e| Error::new(Code::Internal, format!("profile view: {e}")))?;
    // The profile view also feeds Manage Workspaces, which can opt into its
    // deleted section. Ordinary opening above deliberately used only active
    // rows, so neither an archived nor a deleted workspace can become
    // `current` here.
    let workspace_rows = client.workspaces_including_deleted()?;
    apply_workspace_catalog(
        &mut profile,
        ProfileId::from_uuid(profile_row.id.as_uuid()),
        &workspace_rows,
    )?;
    // ---- the workspace itself
    //
    // `ws` starts empty and is replaced by the first document opened. It is not
    // a workspace-shaped document holding everything, which is the shape this
    // whole change exists to end.
    let mut workspace = Workspace::new(
        WorkspaceDoc::create(
            WorkspaceId::new(),
            workspace_row
                .as_ref()
                .map_or("Welcome", |row| row.name.as_str()),
        )
        .map_err(|e| Error::new(Code::Internal, format!("empty document: {e}")))?,
    );
    if workspace_row.is_some() {
        workspace.open_from_store(segment, root, current)?;
    }

    // Open the first document that still resolves, so the window opens on
    // something. A manifest entry can outlive its lifecycle row after an
    // interrupted or historically broken import. That is damaged content, not
    // an unreachable Store: keep the workspace manager usable and try the next
    // entry. Every other failure remains fatal because storage or corrupt CRDT
    // bytes cannot be made safe by silently opening an empty placeholder.
    for id in workspace.entries().iter().map(|entry| entry.id) {
        match workspace.open_document(id) {
            Ok(()) => break,
            Err(error) if error.code() == Code::DocumentNotFound => continue,
            Err(error) => return Err(error),
        }
    }

    if workspace_row.is_some() {
        let _ = store_workspace(current);
    }
    Ok(StoreLaunch::Open(OpenedFromStore {
        profiles,
        profile,
        workspace,
        current,
        has_workspace: workspace_row.is_some(),
    }))
}

// Explicit Profile navigation follows its list. Ordinary startup still honors
// the remembered workspace and retains its existing newest-created fallback.
fn choose_workspace(
    rows: &[koru_store_core::model::Workspace],
    wanted: Option<WorkspaceId>,
    explicit_profile: bool,
) -> Option<&koru_store_core::model::Workspace> {
    let active = || {
        rows.iter()
            .filter(|row| !row.archived && row.deleted_at.is_none())
    };
    wanted
        .and_then(|id| active().find(|row| row.id.as_uuid() == id.as_uuid()))
        .or_else(|| {
            if explicit_profile {
                active().min_by_key(|row| (row.position, row.created_at.millis(), row.id.as_uuid()))
            } else {
                active().max_by_key(|row| row.created_at.millis())
            }
        })
}

#[cfg(test)]
mod tests;
