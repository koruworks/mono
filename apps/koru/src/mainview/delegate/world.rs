// SPDX-License-Identifier: MPL-2.0

//! What the modal is built from — values, gathered once, on the window thread.
//!
//! Everything here is already in this window's memory when a gesture happens:
//! the launch profiles and cached host readiness, the session
//! projects the Agents navigator was handed, the current workspace and its
//! name. **Nothing in this file opens a file, asks a store or dials the
//! gateway** — that is the whole reason the modal takes a [`World`] instead of
//! a client, and it is what makes "opening it touches no disk" a property of
//! the type rather than a promise in a comment.
//!
//! The two choosers are pure functions over that material, so the agent filter
//! and the scope list are testable without a window.

use koru_delegate_context::ContextSource;
use koru_delegate_core::{
    ProjectId, RelativeFolder, SessionProfileKey, WorkspaceId, WorkspaceScope,
};
use koru_i18n::Key as I18nKey;
use koru_session_profiles::{LaunchProfile, ManagedProviderRegistration, ProfileLabel};
use koru_session_protocol::{
    ErrorCode, ExecutableAvailability, ProfileKind, ProjectSummary, ProviderKind,
    ProviderReadiness, capability,
};

/// One agent the modal may offer.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct AgentRow {
    pub(crate) key: SessionProfileKey,
    /// The brand name, from the one projection both launch surfaces use.
    pub(crate) name: String,
    pub(crate) provider: ProviderKind,
    pub(crate) readiness: ProviderReadiness,
}

impl AgentRow {
    pub(crate) fn choosable(&self) -> bool {
        matches!(
            self.readiness.availability,
            ExecutableAvailability::Available { .. }
        ) && self
            .readiness
            .capabilities
            .supports(capability::MANAGED_DELEGATION)
    }
}

/// One working folder the modal may offer.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct ScopeRow {
    pub(crate) scope: WorkspaceScope,
    /// The project's name, or the sentence for the temporary folder.
    pub(crate) name: String,
    /// **Where that is on disk** (ADR-365). A project's root as the session
    /// backend already told this window, or the folder Koru's own run
    /// directories are made in. `None` for a project with no root set — there
    /// is no path to show, which is the whole of why it cannot be chosen.
    ///
    /// Shown rather than implied because *"a folder Koru makes for this run"*
    /// answers what kind of folder it is and not which one, and somebody
    /// handing work to an agent is entitled to know where it will happen
    /// before they press the button rather than by going looking afterwards.
    pub(crate) path: Option<String>,
    /// Why this row cannot be chosen, as a key — a project with no root set.
    /// `None` means it can.
    pub(crate) disabled: Option<I18nKey>,
}

impl ScopeRow {
    pub(crate) fn choosable(&self) -> bool {
        self.disabled.is_none()
    }
}

/// One thing the run will be given, before it is pinned.
///
/// The label is what a person reads; the source is what
/// [`koru_delegate_context::StoreContext::pin`] turns into a version. Neither
/// ever reaches a log: the label is a document's title, which is content.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct ContextRow {
    pub(crate) source: ContextSource,
    pub(crate) label: String,
    /// Whether this row names one block rather than a whole document — the
    /// only difference a person sees between the two.
    pub(crate) block: bool,
}

/// **Where the crumb goes** when the delegation is accepted (D7's *"leave a
/// crumb behind"*, D8): the document the gesture happened in, and the row it
/// happened on.
///
/// Carried as a value like everything else in [`World`], rather than by
/// keeping the origin around: the modal must not know which of the four
/// gestures made it, or the four start differing. What each gesture chose is
/// [`super::DelegationOrigin::pill_anchor`]'s to say, once.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(crate) struct PillAnchor {
    pub(crate) document: koru_core::DocumentId,
    pub(crate) node: koru_core::NodeId,
}

/// Everything the surface needs about this window, resolved before it opens.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct World {
    pub(crate) workspace: WorkspaceId,
    /// The workspace's own name, for D28's one sentence about reach.
    pub(crate) workspace_name: String,
    /// Who is asking, as the profile they are acting under.
    pub(crate) profile: koru_store_core::ProfileId,
    pub(crate) agents: Vec<AgentRow>,
    pub(crate) scopes: Vec<ScopeRow>,
    pub(crate) context: Vec<ContextRow>,
    /// **The one thing a person may attach that the gesture did not**: the
    /// document open in the active tab, when there is one. A picker over every
    /// document in the workspace is the artifact surface's (P1.13); until then
    /// the useful half of "add" is the page you are looking at, and it costs
    /// no round trip because this window is already showing it.
    ///
    /// `None` when no tab is showing a document.
    pub(crate) attachable: Option<ContextRow>,
    /// The objective's first line, when the gesture knew one — a task's title.
    pub(crate) objective: String,
    /// Where the run's pill lands (D8). `None` for the `+` menu, which names
    /// no document — see [`super::DelegationOrigin::pill_anchor`].
    pub(crate) pill_anchor: Option<PillAnchor>,
    /// **The folder Koru makes a temporary run's own directory inside**
    /// (ADR-365), already resolved: the one a person chose, or this machine's
    /// temporary directory. A value rather than a setting to read, like
    /// everything else here — resolving it is
    /// [`koru_settings::delegate_temporary_base`]'s, and it reads the
    /// environment and no disk, so the window may ask.
    pub(crate) temporary_base: String,
    /// **The sentence a refused folder left behind.** A folder Koru cannot
    /// write into is answered at the moment it is chosen, which is where
    /// somebody is still looking at the picker, rather than minutes later
    /// when a run will not start.
    pub(crate) base_problem: Option<I18nKey>,
    /// One sentence saying that a re-run's Context is short of something the
    /// earlier run had, because its source is in the trash or gone (D48).
    /// `false` for every other gesture, which attaches only what it can see.
    pub(crate) dropped_context: bool,
    /// **The agent this gesture asks for**, when it knows one -- a re-run's
    /// (ADR-366). `None` is every other gesture, and leaves the first
    /// installed agent chosen. An agent that is no longer on this machine is
    /// not in the list, and the surface falls back rather than starting on a
    /// row that is not there.
    pub(crate) chosen_agent: Option<SessionProfileKey>,
    pub(crate) chosen_model: Option<koru_delegate_core::ModelName>,
    /// The working folder this gesture asks for, the same way. A project that
    /// has since lost its root cannot be chosen, and the surface falls back to
    /// the first that can.
    pub(crate) chosen_scope: Option<WorkspaceScope>,
    /// Whether D28's grant starts on. Only a re-run says otherwise: a run that
    /// was given no reach into the workspace is asked for again with none.
    pub(crate) grant: bool,
}

/// Registered managed candidates, including unavailable rows and their reason.
/// Raw presence is independent. A received snapshot is authoritative: an absent
/// kind is unsupported, while no snapshot yet means Checking. The cached
/// delegation hint admits selection, never replaces the live launch verdict.
pub(crate) fn agents_for(
    profiles: &[LaunchProfile],
    registrations: &[ManagedProviderRegistration],
    snapshot: Option<&[ProviderReadiness]>,
    platform_shell: &str,
    custom: &str,
) -> Vec<AgentRow> {
    profiles
        .iter()
        .filter_map(|profile| {
            if profile.kind != ProfileKind::Agent {
                return None;
            }
            let provider = profile.provider.as_ref()?;
            let readiness = snapshot.and_then(|rows| rows.iter().find(|row| row.kind == *provider));
            if readiness.is_none()
                && !registrations
                    .iter()
                    .any(|entry| entry.kind == provider.as_str())
            {
                return None;
            }
            let readiness = readiness.cloned().unwrap_or_else(|| ProviderReadiness {
                kind: provider.clone(),
                availability: if snapshot.is_none() {
                    ExecutableAvailability::Checking
                } else {
                    ExecutableAvailability::incompatible(ErrorCode::CapabilityNotNegotiated)
                },
                capabilities: Default::default(),
                failure: None,
            });
            Some(AgentRow {
                key: profile.id.clone(),
                name: koru_session_profiles::display_name(profile, platform_shell, custom)
                    .to_owned(),
                provider: provider.clone(),
                readiness,
            })
        })
        .collect()
}

/// **Where a run may work**, from the session projects this window knows.
///
/// A project with no root is listed and **dimmed with the reason**, never
/// hidden: "Set folder…" is a thing a person does to a project, and a row that
/// vanished would be a project they cannot find out about. The temporary
/// folder closes the list, because it is the answer for work that belongs to
/// no project rather than the first thing to reach for.
pub(crate) fn scopes_for(
    projects: &[ProjectSummary],
    temporary: &str,
    temporary_base: &str,
) -> Vec<ScopeRow> {
    let mut rows: Vec<ScopeRow> = projects
        .iter()
        .map(|project| {
            let root = project
                .root_directory
                .as_deref()
                .map(str::trim)
                .filter(|root| !root.is_empty());
            ScopeRow {
                scope: WorkspaceScope::ProjectRoot {
                    project: project.id,
                },
                name: project.name.clone(),
                path: root.map(str::to_owned),
                disabled: root.is_none().then_some(I18nKey::DelegateScopeNoRoot),
            }
        })
        .collect();
    rows.push(ScopeRow {
        scope: WorkspaceScope::Temporary,
        name: temporary.to_owned(),
        // **The parent of the run's own directory**, not the base alone: it is
        // the deepest folder that is the same for every run, and it is what a
        // person would open to find their work. The run's own name cannot be
        // shown — it is minted when *Delegate* is pressed — so the row shows
        // the folder those directories appear in and says, in its own name,
        // that one is made inside it.
        path: Some(run_folder(temporary_base)),
        disabled: None,
    });
    rows
}

/// `<base>/koru-runs` — where a temporary run's own directory is made.
///
/// The middle segment is `koru_settings::RUNS_FOLDER`, spelled once there and
/// joined here so the window shows the same folder the worker writes into.
pub(crate) fn run_folder(base: &str) -> String {
    std::path::Path::new(base)
        .join(koru_settings::RUNS_FOLDER)
        .to_string_lossy()
        .into_owned()
}

/// The first row a freshly opened modal should sit on: the first project that
/// can actually be chosen, or the temporary folder when none can.
pub(crate) fn first_choosable_scope(rows: &[ScopeRow]) -> Option<usize> {
    rows.iter().position(ScopeRow::choosable)
}

/// A folder inside a project's root, bounded and hidden the way every other
/// path in a run is. Unused by M1's picker — a scope is a whole project root
/// or a temporary folder — and kept because `WorkspaceScope::ProjectFolder`
/// exists and a caller reaching for it should find the one constructor rather
/// than writing a second.
#[allow(dead_code)]
pub(crate) fn project_folder(project: ProjectId, folder: &str) -> Option<WorkspaceScope> {
    RelativeFolder::new(folder)
        .ok()
        .map(|folder| WorkspaceScope::ProjectFolder { project, folder })
}

/// The brand name a label falls back to when a profile resolves to nothing —
/// never shown today, and here so the `_` arm of a match is not a panic.
#[allow(dead_code)]
pub(crate) fn fallback_name(label: ProfileLabel) -> &'static str {
    label.fallback()
}
