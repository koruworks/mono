// SPDX-License-Identifier: MPL-2.0

//! **Workspace management without a picture.**
//!
//! Everything [`crate::WorkspacesSurface`] knows that is not about pixels: the
//! catalog it holds, the two authorizations, where a confirmation has got to,
//! the notice a refusal left, the host-menu intent waiting for its facts, and
//! the reasons a control cannot be pressed. A surface that draws a list asks
//! this; a second surface that draws the same workspaces differently — a
//! profile's own page, say — asks the same thing and cannot disagree with the
//! first.
//!
//! **What it does not hold:** a text engine, a viewport, a scroll offset, a
//! hover, a rect. Those are the drawing's, and a controller that held one would
//! be a surface with a second name.
//!
//! The intent vocabulary is [`Action`]: a drawing hands one to
//! [`WorkspacesController::perform`] when somebody presses the control it drew
//! for it, after asking [`WorkspacesController::reason_for`] whether the press
//! may run. **`perform` does not ask on the caller's behalf**, because the
//! drawing has already shown the reason and decided the press is consumed —
//! and a retry or a host-menu intent that has checked its own preconditions
//! must not be refused by a rule written for a button.

use koru_api_wire::WorkspaceLifecycleAction;
use koru_i18n::{Key, Lang, fill, t};
use koru_store_core::ids::{ProfileId, WorkspaceId};
use koru_ui::confirm::Outcome;

use crate::action::{Action, Request};
use crate::links::WorkspaceLinks;
use crate::pending::{CommandId, Pending};
use crate::state::{
    Answer, Authorization, Availability, Consequence, Group, MoveConsequence, Notice, ProfileRow,
    Retry, Row,
};
use crate::{
    NOTICE_MOVE_PROJECTS_PENDING, REFUSAL_DOCUMENTS_OPEN, REFUSAL_MOVE_NO_SUCH_PROFILE,
    REFUSAL_MOVE_NOT_PERMITTED, REFUSAL_MOVE_PARTIAL, REFUSAL_NOT_PERMITTED, REFUSAL_PARTIAL,
};

mod pins;
#[cfg(test)]
mod tests;

/// Where a confirmation has got to.
///
/// **Three stages and not two**, because asking the gateway what an action
/// would do is a round trip: a dialog that drew itself before the answer
/// arrived would either show an empty consequence or a guessed one, and the
/// point of the server-issued token is that it is neither.
pub(crate) enum Stage {
    /// Waiting for the gateway to say what this would close.
    ///
    /// **`confirm` is decided before the answer arrives**, because it is a fact
    /// about the action rather than about what came back. Archiving and
    /// soft-deleting close terminals and ask; unarchiving and restoring close
    /// nothing, destroy nothing, and have no consequence to accept — but they
    /// still take a token, because they still go through the one operation the
    /// destructive scope gates (ADR-267).
    Asking { confirm: bool },
    /// On screen, with the consequence and its token.
    Confirming(Consequence, koru_ui::Confirm),
    /// Accepted; the operation is running.
    Working,
    /// **The destination picker is open.** A move takes an argument the other
    /// operations do not, so it has a stage the others do not: the profiles are
    /// a section of the column rather than a modal, because choosing from a
    /// list needs no window and this surface is not entitled to make one.
    Choosing(WorkspaceId),
    /// Waiting for the gateway to say what moving it there would close.
    AskingMove,
    /// The move's consequence, on screen with its token.
    ConfirmingMove(MoveConsequence, koru_ui::Confirm),
}

/// **The workspace-management state machine**, with no drawing in it.
pub struct WorkspacesController {
    lang: Lang,
    links: WorkspaceLinks,
    rows: Vec<Row>,
    catalog_received: bool,
    removals_received: bool,
    removals: std::collections::BTreeMap<ProfileId, koru_store_core::profile_removal::RemovalMark>,
    /// Every profile, so the picker has somewhere to send a workspace. A
    /// profile is only ever a destination here.
    profiles: Vec<ProfileRow>,
    availability: Availability,
    authorization: Authorization,
    /// The same three-valued probe about `workspace.migrate`. Two, because the
    /// two scopes are two (ADR-268) and a window can hold either alone.
    migration: Authorization,

    /// An id rather than an index: a selection must survive the list moving
    /// under it, and a workspace that has left the list is no selection at all.
    selected: Option<WorkspaceId>,
    show_archived: bool,
    show_trash: bool,

    pub(crate) stage: Option<Stage>,
    notice: Option<Notice>,
    notice_owner: Option<ProfileId>,
    lifecycle_owner: Option<ProfileId>,
    /// A lifecycle verb chosen in a host-owned workspace menu. It waits for
    /// the catalog and authorization rather than falling back to the host's
    /// unscoped Store route.
    pending_host_action: Option<Action>,
    requests: Vec<Request>,
    next_command: u64,
    pending: std::collections::BTreeMap<CommandId, Pending>,
    pin_repairs: std::collections::BTreeMap<WorkspaceId, pins::PinRepair>,
}

impl WorkspacesController {
    /// **A controller with no link.** It dials nothing and the host feeds its
    /// rows — which is what every test constructs.
    pub fn new() -> Self {
        Self::with_links(WorkspaceLinks::disabled())
    }

    /// A controller that reads and acts through its own link.
    pub fn with_links(links: WorkspaceLinks) -> Self {
        let controller = Self {
            lang: Lang::En,
            links,
            rows: Vec::new(),
            catalog_received: false,
            removals_received: false,
            removals: std::collections::BTreeMap::new(),
            profiles: Vec::new(),
            availability: Availability::default(),
            authorization: Authorization::default(),
            migration: Authorization::default(),
            selected: None,
            show_archived: false,
            show_trash: false,
            stage: None,
            notice: None,
            notice_owner: None,
            lifecycle_owner: None,
            pending_host_action: None,
            requests: Vec::new(),
            next_command: 0,
            pending: std::collections::BTreeMap::new(),
            pin_repairs: std::collections::BTreeMap::new(),
        };
        // **Both, at construction.** The catalog is what it presents and the
        // authorization is what it may offer, and a controller that asked for
        // the second only when somebody reached for a destructive row would
        // have to dim it after they had already pressed it.
        if controller.links.is_held() {
            controller.links.fetch_catalog();
            controller.links.probe_authorization();
            // **And the profiles, at construction too.** The picker draws from
            // this list, and fetching it when somebody presses Move would put a
            // round trip between the press and the list — which is the pause
            // that makes a person press again.
            controller.links.fetch_profiles();
        }
        controller
    }

    // ------------------------------------------------------- the host feeds it

    /// The language every sentence this controller words is in: notices,
    /// reasons and the confirmation's text.
    pub fn set_lang(&mut self, lang: Lang) {
        self.lang = lang;
        for repair in self.pin_repairs.values_mut() {
            repair.notice.text = t(lang, Key::WorkspacePinListingPending).into();
        }
    }

    pub fn lang(&self) -> Lang {
        self.lang
    }

    /// The links this controller holds, so a host can assert what it handed
    /// over.
    pub fn links(&self) -> &WorkspaceLinks {
        &self.links
    }

    /// A changed pushed listing invalidates the catalog without doing I/O here.
    pub fn invalidate(&self) {
        self.links.invalidate();
    }

    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
            || self
                .pin_repairs
                .values()
                .any(|repair| repair.has_automatic_attempt())
    }

    pub fn keep_profile(
        &mut self,
        owner: ProfileId,
        attempt: koru_store_core::ids::RemovalAttemptId,
    ) {
        if self
            .pending
            .values()
            .any(|pending| matches!(pending, Pending::KeepProfile(id) if *id == owner))
        {
            return;
        }
        let id = self.start_command(Pending::KeepProfile(owner));
        self.links.keep_profile(id, owner, attempt);
    }

    /// The embedded form's already validated draft uses this window's worker.
    /// The host's template worker waits for this exact command's result.
    pub fn create_profile(
        &mut self,
        request: koru_api_client::CreateProfileWithWorkspace,
        reply: std::sync::mpsc::Sender<
            koru_store_core::errors::Result<koru_api_client::CreatedProfile>,
        >,
    ) {
        let id = self.start_command(Pending::CreateProfile(reply));
        self.links.create_profile(id, request);
    }

    /// Availability resolution shares this window's worker and correlation.
    /// The host owns the window/Profile attempt and decides whether to land.
    pub fn ensure_live(
        &mut self,
        owner: ProfileId,
        name: String,
        attempt: u64,
        reply: std::sync::mpsc::Sender<
            koru_store_core::errors::Result<koru_api_client::EnsuredWorkspace>,
        >,
    ) {
        let id = self.start_command(Pending::EnsureLive {
            owner,
            attempt,
            reply,
        });
        self.links.ensure_live(id, owner, name, attempt);
    }

    pub fn pending_pin(&self, workspace: WorkspaceId) -> Option<bool> {
        self.pending
            .values()
            .find_map(|pending| match pending {
                Pending::Pin {
                    workspace: id,
                    pinned,
                } if *id == workspace => Some(*pinned),
                _ => None,
            })
            .or_else(|| self.pin_repairs.get(&workspace).map(|repair| repair.pinned))
    }

    pub fn pending_order(&self, owner: ProfileId) -> Option<&[WorkspaceId]> {
        self.pending.values().find_map(|pending| match pending {
            Pending::Order { owner: id, ids } if *id == owner => Some(ids.as_slice()),
            _ => None,
        })
    }

    fn start_command(&mut self, pending: Pending) -> CommandId {
        // Availability may start after a committed retirement. Its read must
        // not erase the lifecycle retry that still owes publication.
        if !matches!(
            pending,
            Pending::EnsureLive { .. } | Pending::ReconcileListing(_)
        ) {
            self.notice = None;
            self.notice_owner = None;
        }
        self.next_command = self
            .next_command
            .checked_add(1)
            .expect("command id exhausted");
        let id = CommandId(self.next_command);
        self.pending.insert(id, pending);
        id
    }

    /// **Take a catalog the host read.** For a host that already holds a
    /// client; answers whether anything presented moved.
    pub fn adopt_rows(&mut self, rows: &[Row]) -> bool {
        let moved =
            !self.catalog_received || self.rows != rows || self.availability != Availability::Live;
        self.catalog_received = true;
        self.rows = rows.to_vec();
        self.adopt_pin_repairs();
        self.availability = Availability::Live;
        // A selection that has left the list is dropped rather than moved: the
        // detail panel would otherwise describe a workspace that is not there.
        if self
            .selected
            .is_some_and(|id| !self.rows.iter().any(|row| row.id == id))
        {
            self.selected = None;
        }
        moved | self.run_pending_host_action()
    }

    /// **Take a profile list the host read.** Same reason as `adopt_rows`.
    pub fn adopt_profiles(&mut self, profiles: &[ProfileRow]) -> bool {
        let moved = self.profiles != profiles;
        self.profiles = profiles.to_vec();
        moved
    }

    /// What this window may do, when the host knows before this asks.
    pub fn set_authorization(&mut self, authorization: Authorization) {
        self.authorization = authorization;
        self.run_pending_host_action();
    }

    pub fn authorization(&self) -> Authorization {
        self.authorization
    }

    /// The same, for `workspace.migrate`.
    pub fn set_migration_authorization(&mut self, authorization: Authorization) {
        self.migration = authorization;
    }

    pub fn migration_authorization(&self) -> Authorization {
        self.migration
    }

    pub fn set_availability(&mut self, availability: Availability) {
        self.availability = availability;
    }

    pub fn availability(&self) -> Availability {
        self.availability
    }

    /// Every profile this controller knows of, as destinations.
    pub fn profiles(&self) -> &[ProfileRow] {
        &self.profiles
    }

    /// The quiet sentence a refusal left behind, if there is one.
    pub fn notice(&self) -> Option<&Notice> {
        self.notice.as_ref()
    }

    /// A waiting Profile only presents failures belonging to that Profile.
    pub fn notice_for(&self, owner: ProfileId) -> Option<&Notice> {
        (self.notice_owner == Some(owner))
            .then_some(self.notice.as_ref())
            .flatten()
            .or_else(|| {
                self.pin_repairs.iter().find_map(|(id, repair)| {
                    (!repair.awaiting_catalog
                        && self.row(*id).is_some_and(|row| row.owner == owner))
                    .then_some(&repair.notice)
                })
            })
    }

    /// Retry the notice actually displayed in this Profile's menu.
    pub fn retry_for(&mut self, owner: ProfileId) {
        if let Some(notice) = self.notice_for(owner).cloned() {
            self.notice = Some(notice);
            self.notice_owner = Some(owner);
            self.perform(Action::Retry);
        }
    }

    /// Which workspace the details apply to.
    pub fn selected(&self) -> Option<WorkspaceId> {
        self.selected
    }

    pub fn set_selected(&mut self, workspace: Option<WorkspaceId>) {
        self.selected = workspace;
    }

    /// The catalog row of one workspace, whatever group it is in.
    pub fn row(&self, workspace: WorkspaceId) -> Option<&Row> {
        self.rows.iter().find(|row| row.id == workspace)
    }

    /// The last complete catalog, independent of Manage's filters. `None`
    /// means no answer yet; an empty answer is a real catalog. Transport
    /// failure retains this snapshot while availability reports it stale.
    pub fn catalog_rows(&self) -> Option<&[Row]> {
        self.catalog_received.then_some(&self.rows)
    }

    pub fn profile_removal(
        &self,
        owner: ProfileId,
    ) -> Option<&koru_store_core::profile_removal::RemovalMark> {
        self.removals.get(&owner)
    }

    pub fn profile_removal_state(
        &self,
        owner: ProfileId,
    ) -> Option<Option<koru_store_core::profile_removal::RemovalMark>> {
        if let Some(mark) = self.removals.get(&owner) {
            Some(Some(mark.clone()))
        } else {
            self.removals_received.then_some(None)
        }
    }

    /// **Whether the archived group is open**, which a host persists.
    ///
    /// It is window furniture rather than a setting (rev 4's program log): a
    /// window remembers whether somebody had the archive open, and the settings
    /// authority holds nothing about it. The trash toggle is deliberately *not*
    /// persisted — the manager this replaces did not persist it either, and a
    /// window that opened showing the trash every time would be a window that
    /// kept bringing up deleted things.
    pub fn show_archived(&self) -> bool {
        self.show_archived
    }

    pub fn set_show_archived(&mut self, show: bool) {
        self.show_archived = show;
    }

    /// Whether the trash group is open.
    pub fn show_trash(&self) -> bool {
        self.show_trash
    }

    pub fn set_show_trash(&mut self, show: bool) {
        self.show_trash = show;
    }

    /// What only the host can do. Drained, so a request is performed once.
    pub fn take_requests(&mut self) -> Vec<Request> {
        std::mem::take(&mut self.requests)
    }

    /// **Ask to rename whatever is selected**, for a host with a chord for it.
    ///
    /// Answers whether there was anything to rename, so a host can fall
    /// through to its own target when there is not.
    pub fn rename_selected(&mut self) -> bool {
        let Some(workspace) = self.selected else {
            return false;
        };
        self.requests.push(Request::Rename(workspace));
        true
    }

    // ------------------------------------------------------ host-menu intents

    /// Carry a host-owned workspace menu's archive choice into this flow.
    ///
    /// The context menu knows which workspace was chosen, while this
    /// controller alone owns the vouched client and server-issued
    /// confirmation. Keeping the intent here closes that seam without reviving
    /// the shell's unscoped lifecycle route.
    pub fn archive_workspace(&mut self, workspace: WorkspaceId, archived: bool) -> bool {
        self.queue_host_action(Action::Archive(workspace, archived))
    }

    /// Carry a host-owned Delete Workspace choice into the supervised flow.
    pub fn delete_workspace(&mut self, workspace: WorkspaceId) -> bool {
        self.queue_host_action(Action::SoftDelete(workspace))
    }

    /// Carry a host-owned Restore Workspace choice into the supervised flow.
    pub fn restore_workspace(&mut self, workspace: WorkspaceId) -> bool {
        self.queue_host_action(Action::Restore(workspace))
    }

    fn queue_host_action(&mut self, action: Action) -> bool {
        self.selected = Some(match action {
            Action::Archive(workspace, _)
            | Action::SoftDelete(workspace)
            | Action::Restore(workspace) => workspace,
            _ => unreachable!("only lifecycle actions cross the host-menu seam"),
        });
        self.pending_host_action = Some(action);
        self.run_pending_host_action();
        true
    }

    /// Run a menu intent only once both facts the shared flow needs have
    /// arrived. A newly opened manager fetches catalog and authorization on
    /// separate worker messages, in either order; dropping the intent between
    /// them is the bug that made Delete merely open this tab.
    fn run_pending_host_action(&mut self) -> bool {
        let Some(action) = self.pending_host_action else {
            return false;
        };
        if self.availability.is_down() || self.authorization == Authorization::Refused {
            self.pending_host_action = None;
            return true;
        }
        if self.availability.is_loading() || self.authorization == Authorization::Unknown {
            return false;
        }
        let workspace = match action {
            Action::Archive(workspace, _)
            | Action::SoftDelete(workspace)
            | Action::Restore(workspace) => workspace,
            _ => unreachable!("only lifecycle actions cross the host-menu seam"),
        };
        let Some(row) = self.row(workspace) else {
            // The including-deleted catalog is live and the menu's subject is
            // absent, so another window already removed the thing this intent
            // named. It must never spring back into action if that id is later
            // imported again.
            self.pending_host_action = None;
            return true;
        };
        let valid = match action {
            Action::Archive(_, archived) => !row.deleted && row.archived != archived,
            Action::SoftDelete(_) => !row.deleted,
            Action::Restore(_) => row.deleted,
            _ => unreachable!("only lifecycle actions cross the host-menu seam"),
        };
        self.pending_host_action = None;
        if valid {
            self.perform(action);
        }
        true
    }

    // ----------------------------------------------------------- the links

    /// **Take whatever the link has, and refetch if a fact says to.**
    ///
    /// Never blocks. Answers whether anything moved, so a host repaints only
    /// when there is something to repaint.
    pub fn poll(&mut self) -> bool {
        let mut moved = false;
        while let Some(answer) = self.links.take() {
            moved |= self.receive(answer);
        }
        moved |= self.poll_pin_repairs_at(std::time::Instant::now());
        // Invalidate-on-event, fetch-on-poll. A committed fact from any window
        // — including this one's own operation — lands here, and two windows
        // converge because both are reading the same journal.
        let workspaces_stale = self.links.take_stale();
        let profiles_stale = self.links.take_profiles_stale();
        if workspaces_stale || profiles_stale {
            self.links.fetch_catalog();
        }
        if profiles_stale {
            self.links.fetch_profiles();
        }
        moved
    }

    pub(crate) fn receive(&mut self, answer: Answer) -> bool {
        match answer {
            Answer::Settled(id, answer) => {
                let Some(pending) = self.pending.remove(&id) else {
                    return false;
                };
                if let Pending::Pin { workspace, .. } = &pending {
                    if let Answer::Pinned(result) = *answer {
                        self.pin_settled(*workspace, result, std::time::Instant::now());
                        return true;
                    }
                }
                if let Pending::ReconcileListing(workspace) = pending {
                    self.pin_repair_settled(workspace, *answer);
                    return true;
                }
                if let Pending::KeepProfile(owner) = pending {
                    if !matches!(*answer, Answer::ProfileKept(Ok(()))) {
                        self.accept_notice(Notice {
                            text: crate::REFUSAL_UNAVAILABLE.into(),
                            retry: None,
                        });
                        self.notice_owner = Some(owner);
                    }
                    // A refusal may mean a newer removal replaced the shown
                    // choice. Always read its current attempt before redraw.
                    self.links.fetch_catalog();
                    return true;
                }
                if let Pending::EnsureLive {
                    owner,
                    attempt,
                    reply,
                } = pending
                {
                    let result = match *answer {
                        Answer::WorkspaceEnsured {
                            attempt: answered,
                            result,
                        } if answered == attempt => result,
                        _ => Err(koru_store_core::Error::new(
                            koru_store_core::Code::StorageFailed,
                            "Workspace preparation did not receive its Store acknowledgement",
                        )),
                    };
                    if let Ok(koru_api_client::EnsuredWorkspace::Removing { mark }) = &result {
                        self.removals.insert(owner, mark.clone());
                        self.links.fetch_catalog();
                    }
                    // `Empty` refreshes too: the listing is current and holds
                    // no live workspace, which the surface shows as the
                    // needs-a-workspace state (UI half of Plan 1.3).
                    if matches!(
                        result,
                        Ok(koru_api_client::EnsuredWorkspace::Active { .. }
                            | koru_api_client::EnsuredWorkspace::Created { .. }
                            | koru_api_client::EnsuredWorkspace::Empty)
                    ) {
                        self.links.fetch_catalog();
                    }
                    let _ = reply.send(result);
                    return true;
                }
                if let Pending::CreateProfile(reply) = pending {
                    let result = match *answer {
                        Answer::ProfileCreated(result) => result,
                        _ => Err(koru_store_core::Error::new(
                            koru_store_core::Code::StorageFailed,
                            "Profile creation did not receive a Store acknowledgement",
                        )),
                    };
                    if result.is_ok() {
                        self.links.fetch_catalog();
                        self.links.fetch_profiles();
                    }
                    let _ = reply.send(result);
                    return true;
                }
                match *answer {
                    Answer::Refused(notice) => {
                        self.accept_notice(notice);
                        self.notice_owner = None;
                    }
                    answer => {
                        self.receive(answer);
                    }
                }
                true
            }
            #[cfg(any(test, feature = "fixtures"))]
            Answer::Catalog(rows) => self.adopt_rows(&rows),
            Answer::CatalogState { rows, removals } => {
                let changed = !self.removals_received || self.removals != removals;
                self.removals_received = true;
                self.removals = removals;
                self.adopt_rows(&rows) | changed
            }
            Answer::ProfileCreated(_)
            | Answer::Pinned(_)
            | Answer::ListingReconciled(_)
            | Answer::ProfileKept(_)
            | Answer::WorkspaceEnsured { .. } => false,
            Answer::Profiles(profiles) => self.adopt_profiles(&profiles),
            Answer::Authorization(authorization) => {
                let moved = self.authorization != authorization;
                self.authorization = authorization;
                moved | self.run_pending_host_action()
            }
            Answer::MigrationAuthorization(authorization) => {
                let moved = self.migration != authorization;
                self.migration = authorization;
                moved
            }
            // **Acted on only while something is waiting for it.** A
            // consequence that arrives when nothing asked — a late answer to a
            // dialog somebody cancelled, a second reply to one request — is
            // dropped. The alternative is a manager that archives a workspace
            // because a message was slow, which is precisely the thing the
            // acceptance exists to make impossible.
            Answer::Consequence(consequence) => match self.stage {
                // The dialog is built only now, from what the gateway said —
                // never from what this controller assumed it would say.
                Some(Stage::Asking { confirm: true }) => {
                    let dialog = self.dialog_for(&consequence);
                    self.stage = Some(Stage::Confirming(consequence, dialog));
                    true
                }
                // Unarchive and restore: the token is fetched and spent without
                // a question, because there is nothing to accept.
                Some(Stage::Asking { confirm: false }) => {
                    self.links
                        .perform(&consequence, t(self.lang, Key::DefaultWorkspaceName));
                    self.stage = Some(Stage::Working);
                    true
                }
                _ => false,
            },
            // The same rule, for the move: a consequence nobody is waiting for
            // is dropped rather than turned into a dialog.
            Answer::MoveConsequence(consequence) => match self.stage {
                Some(Stage::AskingMove) => {
                    let dialog = self.move_dialog_for(&consequence);
                    self.stage = Some(Stage::ConfirmingMove(consequence, dialog));
                    true
                }
                _ => false,
            },
            Answer::Moved(outcome) => {
                self.stage = None;
                self.notice_owner = self.lifecycle_owner.take();
                // **The one success that still says something.** The workspace
                // moved either way; `projects_rescoped: None` means the session
                // host was not asked, so its terminal projects still name the
                // profile it left and are drawn in the missing-workspace area
                // until somebody repairs them (ADR-268). Moving it again is
                // what does, so the notice offers exactly that.
                self.notice = match outcome.projects_rescoped {
                    Some(_) => None,
                    None => Some(Notice {
                        text: self.refusal_text(NOTICE_MOVE_PROJECTS_PENDING),
                        retry: match (
                            WorkspaceId::parse(&outcome.workspace),
                            ProfileId::parse(&outcome.to),
                        ) {
                            (Some(workspace), Some(to)) => Some(Retry::Migration(workspace, to)),
                            _ => None,
                        },
                    }),
                };
                self.links.invalidate();
                true
            }
            Answer::Completed(outcome) => {
                self.stage = None;
                self.notice = None;
                self.notice_owner = None;
                self.lifecycle_owner = None;
                // **Follow the workspace to where it went.** Somebody who has
                // just archived or trashed a workspace watched a row leave the
                // list; opening the group it left for is the difference between
                // "it moved" and "it vanished", and it is also where Restore
                // is. The opposite is not done: a restored workspace reappears
                // in a group that was already open.
                match outcome.action {
                    WorkspaceLifecycleAction::Archive if outcome.mutation_applied => {
                        self.show_archived = true;
                    }
                    WorkspaceLifecycleAction::SoftDelete if outcome.mutation_applied => {
                        self.show_trash = true;
                    }
                    _ => {}
                }
                // The Store published the committed fact; this asks anyway
                // rather than waiting for the round trip, because the list is
                // on screen and briefly wrong is worse than briefly asking.
                self.links.invalidate();
                true
            }
            Answer::Refused(notice) => {
                self.stage = None;
                self.accept_notice(notice);
                self.notice_owner = self.lifecycle_owner.take();
                true
            }
            Answer::Unavailable => {
                let moved = self.availability != Availability::Down;
                self.availability = Availability::Down;
                self.stage = None;
                moved
            }
        }
    }

    fn accept_notice(&mut self, notice: Notice) {
        self.notice = (!notice.text.is_empty()).then(|| Notice {
            text: self.refusal_text(&notice.text),
            retry: notice.retry,
        });
        self.links.invalidate();
    }

    fn refusal_text(&self, token: &str) -> String {
        t(
            self.lang,
            match token {
                REFUSAL_PARTIAL => Key::WorkspacesPartialFailure,
                crate::REFUSAL_LISTING_PENDING => Key::ProfileWaitListing,
                REFUSAL_DOCUMENTS_OPEN => Key::WorkspacesDocumentsOpen,
                REFUSAL_NOT_PERMITTED => Key::WorkspacesNotPermitted,
                REFUSAL_MOVE_PARTIAL => Key::WorkspacesMovePartialFailure,
                REFUSAL_MOVE_NOT_PERMITTED => Key::WorkspacesMoveNotPermitted,
                REFUSAL_MOVE_NO_SUCH_PROFILE => Key::WorkspacesMoveNoSuchProfile,
                NOTICE_MOVE_PROJECTS_PENDING => Key::WorkspacesMoveProjectsPending,
                _ => Key::WorkspacesUnavailableReadOnly,
            },
        )
        .to_owned()
    }

    // ------------------------------------------------------ the confirmation

    /// **The consequence dialog, built from the gateway's answer.**
    ///
    /// ADR-265 row two: a confirmation the person explicitly invoked, of an
    /// action that is *reversible* — archiving hides, soft-deleting moves to a
    /// trash it comes back out of — so it **declares its primary** and Enter
    /// accepts. Escape cancels, as it does wherever closing is safe.
    ///
    /// The body names the terminals, because that is the consequence and the
    /// token is the proof this is the one that was shown.
    fn dialog_for(&self, consequence: &Consequence) -> koru_ui::Confirm {
        let name = self
            .row(consequence.workspace)
            .map(|row| row.name.clone())
            .unwrap_or_default();
        let (title, body, answer) = match consequence.action {
            WorkspaceLifecycleAction::SoftDelete => (
                Key::WorkspacesDeleteTitle,
                Key::WorkspacesDeleteBody,
                Key::WorkspacesDelete,
            ),
            // `restore` never reaches here: it closes nothing and destroys
            // nothing, so there is no consequence to accept.
            _ => (
                Key::WorkspacesArchiveTitle,
                Key::WorkspacesArchiveBody,
                Key::WorkspacesArchiveAction,
            ),
        };
        let terminals = self.terminals_sentence(&consequence.terminals);
        koru_ui::Confirm::localized(
            fill(t(self.lang, title), &[&name]),
            format!("{} {terminals}", t(self.lang, body)),
            t(self.lang, answer),
            t(self.lang, Key::WorkspacesCancel),
        )
        .defaulting_to_confirm()
    }

    /// **The move's consequence dialog, and its ADR-265 declaration.**
    ///
    /// **Row two, the same row archiving sits on: it declares its primary.**
    /// The two questions ADR-265 asks both answer yes here. *Did the person
    /// invoke this?* — they pressed "Move to profile…", read a list of their
    /// own profiles, and chose one; three deliberate acts, and this dialog
    /// continues the action they expressed rather than interrupting them with
    /// one. *Is it reversible?* — moving it back is the same operation with the
    /// other profile named, and nothing is lost on the way: the id, the
    /// documents and the history are the same afterwards (ADR-268).
    ///
    /// **It is not row four**, which is where SP-15's profile removal sits and
    /// where an operation belongs when it is irreversible. A move destroys
    /// nothing. The only thing it ends is the workspace's terminals, and that
    /// is the consequence the body names — which is also why the dialog exists
    /// at all rather than the row acting on the press.
    ///
    /// The terminals are worded exactly as the lifecycle's are, because they
    /// are the same fact and a person should not have to learn two sentences
    /// for it.
    fn move_dialog_for(&self, consequence: &MoveConsequence) -> koru_ui::Confirm {
        let name = self
            .row(consequence.workspace)
            .map(|row| row.name.clone())
            .unwrap_or_default();
        let terminals = self.terminals_sentence(&consequence.terminals);
        koru_ui::Confirm::localized(
            fill(
                t(self.lang, Key::WorkspacesMoveTitle),
                &[&name, &consequence.to_name],
            ),
            format!("{terminals} {}", t(self.lang, Key::WorkspacesMoveBody)),
            t(self.lang, Key::WorkspacesMove),
            t(self.lang, Key::WorkspacesCancel),
        )
        .defaulting_to_confirm()
    }

    fn terminals_sentence(&self, terminals: &[String]) -> String {
        match terminals.len() {
            0 => t(self.lang, Key::WorkspacesNoTerminals).to_owned(),
            count => {
                let line = fill(
                    t(self.lang, Key::WorkspacesTerminalsWillClose),
                    &[&count.to_string()],
                );
                format!("{line} {}", terminals.join(", "))
            }
        }
    }

    /// **The confirmation waiting for an answer**, lifecycle or move.
    ///
    /// Built from the gateway's answer; `None` while nothing is being asked or
    /// while the answer is still on its way.
    pub fn dialog(&self) -> Option<&koru_ui::Confirm> {
        match &self.stage {
            Some(Stage::Confirming(_, dialog) | Stage::ConfirmingMove(_, dialog)) => Some(dialog),
            _ => None,
        }
    }

    /// The same, for a drawing that forwards pointer and key input to it.
    pub fn dialog_mut(&mut self) -> Option<&mut koru_ui::Confirm> {
        match &mut self.stage {
            Some(Stage::Confirming(_, dialog) | Stage::ConfirmingMove(_, dialog)) => Some(dialog),
            _ => None,
        }
    }

    /// Whether a confirmation is waiting — a host draws nothing over it.
    pub fn is_confirming(&self) -> bool {
        self.dialog().is_some()
    }

    /// Whether the destination picker is open, for a host that wants to know
    /// this flow is mid-way.
    pub fn is_choosing_profile(&self) -> bool {
        self.choosing().is_some()
    }

    /// Which workspace the destination picker is open for.
    pub fn choosing(&self) -> Option<WorkspaceId> {
        match self.stage {
            Some(Stage::Choosing(id)) => Some(id),
            _ => None,
        }
    }

    /// What Enter would do in the confirmation, for a test to assert on rather
    /// than infer.
    pub fn confirmation_default(&self) -> Option<Outcome> {
        self.dialog().and_then(koru_ui::Confirm::default_action)
    }

    /// The terminals the confirmation waiting for an answer names.
    pub fn confirmation_terminals(&self) -> &[String] {
        match &self.stage {
            Some(Stage::Confirming(consequence, _)) => &consequence.terminals,
            Some(Stage::ConfirmingMove(consequence, _)) => &consequence.terminals,
            _ => &[],
        }
    }

    /// **Answer the confirmation.** Confirm spends the token, Cancel leaves it
    /// unspent; answers whether anything changed.
    pub fn answer(&mut self, outcome: Outcome) -> bool {
        match outcome {
            // Two answers here; the third belongs to a dialog that declared
            // one (`Confirm::with_alternative`) and never reaches this.
            Outcome::Keep | Outcome::Alternative => false,
            Outcome::Cancel => {
                // The token is simply left unspent. Nothing has to be told
                // about a confirmation nobody accepted.
                self.stage = None;
                true
            }
            Outcome::Confirm => {
                match self.stage.take() {
                    Some(Stage::Confirming(consequence, _)) => {
                        self.links
                            .perform(&consequence, t(self.lang, Key::DefaultWorkspaceName));
                        self.stage = Some(Stage::Working);
                    }
                    Some(Stage::ConfirmingMove(consequence, _)) => {
                        self.links
                            .perform_move(&consequence, t(self.lang, Key::DefaultWorkspaceName));
                        self.stage = Some(Stage::Working);
                    }
                    other => self.stage = other,
                }
                true
            }
        }
    }

    /// **Where a workspace could go**: every profile that is not the one that
    /// already owns it.
    ///
    /// The current owner is left out rather than drawn and dimmed. A move to
    /// the profile that already owns it succeeds and changes nothing, so
    /// offering it would be offering a control whose whole effect is to close a
    /// dialog — and a picker whose first entry is "where it already is" reads
    /// as a mistake.
    pub fn destinations(&self, workspace: WorkspaceId) -> Vec<ProfileRow> {
        let Some(owner) = self.row(workspace).map(|row| row.owner) else {
            return Vec::new();
        };
        self.profiles
            .iter()
            .filter(|profile| profile.id != owner)
            .cloned()
            .collect()
    }

    // ------------------------------------------------------------------ rows

    /// The rows a list would show, grouped and ordered, across every owner.
    ///
    /// **One filter and one sort, in one place.** Every part of a drawing that
    /// has to know where a workspace sits asks this (or [`Self::rows_of`]), so
    /// no two of them can disagree — which is what let the manager this
    /// replaces put a row under one heading and hit-test it under another.
    /// The archived and trash groups are present only while open.
    pub fn visible(&self) -> Vec<(Group, &Row)> {
        let mut rows: Vec<_> = self
            .rows
            .iter()
            .map(|row| (self.display_group(row), row))
            .filter(|(group, _)| match group {
                Group::Archived => self.show_archived,
                Group::Deleted => self.show_trash,
                _ => true,
            })
            .collect();
        rows.sort_by(|(left_group, left), (right_group, right)| {
            left_group
                .cmp(right_group)
                .then(
                    self.display_position(left)
                        .cmp(&self.display_position(right)),
                )
                .then(left.name.cmp(&right.name))
        });
        rows
    }

    /// **One profile's workspaces**, grouped Pinned, Active, Archived, Deleted
    /// and in that profile's own order within each group.
    ///
    /// The same filter and sort as [`Self::visible`], narrowed to `owner` — so
    /// the archived and trash groups follow the same two toggles, and a step
    /// up or down means the same neighbour here as it does to the reorder.
    pub fn rows_of(&self, owner: ProfileId) -> Vec<(Group, &Row)> {
        self.visible()
            .into_iter()
            .filter(|(_, row)| row.owner == owner)
            .collect()
    }

    /// **How many of `owner`'s workspaces are in `group`**, whether or not
    /// the group is open — what a *Show archived (n)* link counts before it
    /// is pressed.
    pub fn count_of(&self, owner: ProfileId, group: Group) -> usize {
        self.rows
            .iter()
            .filter(|row| row.owner == owner && row.group() == group)
            .count()
    }

    /// Whether a step of `step` places (`-1` up, `1` down) would move
    /// `workspace` within its owner's group — `false` at either end, so a
    /// *Move Up* on the first row is dimmed rather than a press that does
    /// nothing.
    pub fn can_step(&self, workspace: WorkspaceId, step: isize) -> bool {
        self.reordered_owner_ids(workspace, step).is_some()
    }

    fn display_position(&self, row: &Row) -> i64 {
        self.pending_order(row.owner)
            .and_then(|ids| ids.iter().position(|id| *id == row.id))
            .map_or(row.position, |position| position as i64)
    }

    fn display_group(&self, row: &Row) -> Group {
        if row.archived || row.deleted {
            row.group()
        } else if self.effective_pinned(row) {
            Group::Pinned
        } else {
            Group::Active
        }
    }

    /// Move a pin within the complete local permutation, retaining hidden rows.
    /// `None` means immediately after the last other saved pin.
    pub fn reorder_before(&mut self, workspace: WorkspaceId, before: Option<WorkspaceId>) -> bool {
        let Some(row) = self.row(workspace) else {
            return false;
        };
        let owner = row.owner;
        if !row.verified_live()
            || !self.effective_pinned(row)
            || self.pending_order(owner).is_some()
            || self.availability.is_down()
            || self.removals.contains_key(&owner)
        {
            return false;
        }
        if before == Some(workspace) {
            return false;
        }
        if let Some(before) = before {
            if self.row(before).is_none_or(|row| {
                row.owner != owner || !row.verified_live() || !self.effective_pinned(row)
            }) {
                return false;
            }
        }
        let mut rows: Vec<_> = self.rows.iter().filter(|row| row.owner == owner).collect();
        rows.sort_by_key(|row| (row.position, row.id));
        let old: Vec<_> = rows.iter().map(|row| row.id).collect();
        rows.retain(|row| row.id != workspace);
        let at = match before {
            Some(before) => rows.iter().position(|row| row.id == before).unwrap(),
            None => rows
                .iter()
                .rposition(|row| row.verified_live() && self.effective_pinned(row))
                .map_or(rows.len(), |i| i + 1),
        };
        let mut ids: Vec<_> = rows.iter().map(|row| row.id).collect();
        ids.insert(at, workspace);
        if ids == old {
            return false;
        }
        let command = self.start_command(Pending::Order {
            owner,
            ids: ids.clone(),
        });
        self.links.reorder(command, owner, ids);
        true
    }

    // --------------------------------------------------------------- reasons

    /// Why nothing that needs the gateway can be pressed, or `None` while it
    /// is answering.
    pub fn unavailable_reason(&self) -> Option<String> {
        self.availability
            .is_down()
            .then(|| t(self.lang, Key::WorkspacesUnavailableReadOnly).to_owned())
    }

    /// **Why `action` cannot be pressed for `row`, in one sentence — or `None`
    /// when it can.**
    ///
    /// Disabled, never hidden (see the crate's root): a drawing shows every
    /// control for every workspace and dims the ones this answers for, with
    /// this sentence beside them. Which ones are *live* changes with the
    /// workspace's state and this window's authorization; which ones exist
    /// does not.
    ///
    /// The orders below are decisions, outranking each other as listed:
    ///
    /// - **Gateway down outranks everything.** A person told they may not when
    ///   the truth is that nothing is answering would go looking for a
    ///   permission problem that is not there.
    /// - **Ordinary actions** (open, rename, appearance, pin, reorder): then a
    ///   workspace in the trash, which is restored before anything else.
    /// - **Archive, Delete and Restore**: then this window's
    ///   `workspace.destroy`; Archive also refuses a trashed workspace.
    /// - **Move to profile**: then this window's own `workspace.migrate`, its
    ///   own sentence — telling somebody they may not *remove* workspaces when
    ///   what they reached for was moving one is a wrong answer; then a trashed
    ///   workspace; and last the honest arithmetic: with one profile there is
    ///   nowhere to send it.
    /// - **Copy id** needs neither a gateway nor a permission.
    ///
    /// Create and Import take no row and refuse only while the gateway is
    /// down; `row` is not consulted for them.
    pub fn reason_for(&self, action: &Action, row: &Row) -> Option<String> {
        let lang = self.lang;
        let unavailable = self.unavailable_reason();
        let trashed = || {
            row.deleted
                .then(|| t(lang, Key::WorkspacesTrash).to_owned())
        };
        let destructive = || {
            unavailable.clone().or_else(|| {
                (!self.authorization.may_destroy())
                    .then(|| t(lang, Key::WorkspacesNotPermitted).to_owned())
            })
        };
        match action {
            Action::Open(_) | Action::OpenInNewWindow(_) => unavailable
                .or_else(|| {
                    self.removals
                        .contains_key(&row.owner)
                        .then(|| t(lang, Key::ProfileWaitRemoving).to_owned())
                })
                .or_else(trashed)
                .or_else(|| {
                    row.archived
                        .then(|| t(lang, Key::WorkspaceArchivedUnavailable).to_owned())
                })
                .or_else(|| row.authority_reason(lang)),
            Action::Rename(_) | Action::Appearance(_) | Action::MoveUp(_) | Action::MoveDown(_) => {
                unavailable
                    .or_else(trashed)
                    .or_else(|| row.authority_reason(lang))
            }
            Action::TogglePinned(_) => unavailable.or_else(|| self.pin_reason(row)),
            Action::Create | Action::Import | Action::Export(_) => unavailable,
            Action::CopyId(_) => None,
            Action::Migrate(_) => unavailable
                .or_else(|| {
                    (!self.migration.is_granted())
                        .then(|| t(lang, Key::WorkspacesMoveNotPermitted).to_owned())
                })
                .or_else(trashed)
                .or_else(|| {
                    self.destinations(row.id)
                        .is_empty()
                        .then(|| t(lang, Key::WorkspacesMoveNoProfiles).to_owned())
                }),
            Action::Archive(..) => destructive().or_else(trashed),
            Action::SoftDelete(_) | Action::Restore(_) => destructive(),
            Action::Select(_)
            | Action::MigrateTo(..)
            | Action::CancelMigrate
            | Action::ShowArchived
            | Action::ShowTrash
            | Action::Retry
            | Action::DismissNotice => None,
        }
    }

    // --------------------------------------------------------------- perform

    /// **Do what `action` means.** The caller has already decided the press
    /// may run — see the module documentation for why this does not ask
    /// [`Self::reason_for`] itself.
    pub fn perform(&mut self, action: Action) {
        // A menu/accessibility action can outlive the row it was drawn from.
        // Recheck navigation at dispatch against the most recent authority.
        if let Action::Open(id) | Action::OpenInNewWindow(id) = &action
            && (self.unavailable_reason().is_some()
                || !self.row(*id).is_some_and(|row| {
                    row.navigation_ready() && !self.removals.contains_key(&row.owner)
                }))
        {
            return;
        }
        match action {
            Action::Select(id) => self.selected = Some(id),
            Action::Open(id) => self.requests.push(Request::Open(id)),
            Action::OpenInNewWindow(id) => self.requests.push(Request::OpenInNewWindow(id)),
            Action::Create => self.requests.push(Request::Create),
            Action::Rename(id) => self.requests.push(Request::Rename(id)),
            Action::Appearance(id) => self.requests.push(Request::ChangeAppearance(id)),
            Action::Import => self.requests.push(Request::Import),
            Action::Export(id) => self.requests.push(Request::Export(id)),
            Action::CopyId(id) => self.requests.push(Request::CopyId(id)),
            // **Opening the picker is not the operation.** Nothing is asked of
            // the gateway until a destination is chosen: a person who opened
            // this to see which profiles exist has not asked to move anything,
            // and issuing a token for a destination nobody picked would be
            // minting an acceptance of a question that was never put.
            Action::Migrate(id) => {
                self.notice = None;
                self.stage = Some(Stage::Choosing(id));
            }
            Action::CancelMigrate => self.stage = None,
            Action::MigrateTo(workspace, to) => {
                self.lifecycle_owner = self.row(workspace).map(|row| row.owner);
                let name = self
                    .profiles
                    .iter()
                    .find(|profile| profile.id == to)
                    .map(|profile| profile.name.clone())
                    .unwrap_or_default();
                self.notice = None;
                self.stage = Some(Stage::AskingMove);
                self.links.ask_move_consequence(workspace, to, name);
            }
            Action::TogglePinned(id) => {
                if let Some(row) = self.row(id).cloned() {
                    if self.reason_for(&action, &row).is_some() {
                        return;
                    }
                    let pinned = !row.pinned;
                    if self.pending_pin(row.id).is_none() {
                        let command = self.start_command(Pending::Pin {
                            workspace: row.id,
                            pinned,
                        });
                        self.links.pin(command, row.id, pinned);
                    }
                }
            }
            Action::MoveUp(id) => self.reorder(id, -1),
            Action::MoveDown(id) => self.reorder(id, 1),
            // **Both destructive actions ask before they draw.** The
            // consequence is the gateway's answer and the token comes with it;
            // a dialog assembled here would be a dialog about a guess.
            Action::Archive(id, true) => self.begin(id, WorkspaceLifecycleAction::Archive, true),
            Action::SoftDelete(id) => self.begin(id, WorkspaceLifecycleAction::SoftDelete, true),
            // **Unarchiving is an appearance change, not a lifecycle action.**
            // The wire has three verbs and `Archive` is one of them — it always
            // sets the flag, because the operation exists to close terminals
            // first and bringing a workspace *back* closes nothing. So this
            // goes through the appearance route, which since ADR-267 requires
            // `workspace.destroy` of its own accord whenever the archived flag
            // actually moves. Same permission, no confirmation, and the row is
            // dimmed by the same reason as the rest.
            Action::Archive(id, false) => {
                if let Some(row) = self.row(id).cloned() {
                    self.notice = None;
                    let command = self.start_command(Pending::Appearance);
                    self.links.set_appearance(command, &row, row.pinned, false);
                }
            }
            // Restore closes nothing either, but it *is* one of the three
            // verbs, so it takes a token — fetched and spent without a question
            // in between, because there is nothing to accept.
            Action::Restore(id) => self.begin(id, WorkspaceLifecycleAction::Restore, false),
            Action::ShowArchived => self.show_archived = !self.show_archived,
            Action::ShowTrash => self.show_trash = !self.show_trash,
            Action::Retry => match self.notice.take().and_then(|notice| notice.retry) {
                Some(Retry::PinListing(workspace)) => self.retry_pin_listing(workspace),
                // **A fresh confirmation, deliberately.** The first token
                // was spent, and the consequence it named is a workspace
                // whose terminals are already closed — asking again is what
                // makes the second acceptance about the second state, and
                // what makes the second dialog say *no terminals are open
                // in it* rather than repeating the first one's list.
                Some(Retry::Lifecycle(workspace, action)) => {
                    self.begin(workspace, action, true);
                }
                // The same, and it covers two cases at once: a move whose Store
                // write refused after the terminals closed, and a move that
                // committed while the session host was not answering. Both are
                // finished by the same request over current state.
                Some(Retry::Migration(workspace, to)) => {
                    self.perform(Action::MigrateTo(workspace, to))
                }
                None => {}
            },
            Action::DismissNotice => self.notice = None,
        }
    }

    /// **Move one workspace a step within its owner's order.**
    ///
    /// The route takes the owner's complete order every time, so this builds
    /// it: every workspace that profile owns, in the order it is currently
    /// presented, with one row moved by one place. Ordering across profiles is
    /// not a thing the route can express, and a manager showing two profiles'
    /// workspaces must not reorder one profile's list by touching the other's
    /// — so a step that would leave the owner is simply not taken.
    ///
    /// The list is not moved locally first. The catalog is the authority, the
    /// answer is a refetch, and a controller that reordered optimistically
    /// would have to decide what to do when the write refused.
    fn reorder(&mut self, workspace: WorkspaceId, step: isize) {
        let Some((owner, order)) = self.reordered_owner_ids(workspace, step) else {
            return;
        };
        if self.pending_order(owner).is_none() {
            let command = self.start_command(Pending::Order {
                owner,
                ids: order.clone(),
            });
            self.links.reorder(command, owner, order);
        }
    }

    pub(crate) fn reordered_owner_ids(
        &self,
        workspace: WorkspaceId,
        step: isize,
    ) -> Option<(ProfileId, Vec<WorkspaceId>)> {
        let (owner, group) = self
            .row(workspace)
            .map(|row| (row.owner, self.display_group(row)))?;
        let visible_order: Vec<WorkspaceId> = self
            .rows_of(owner)
            .into_iter()
            .filter(|(row_group, _)| *row_group == group)
            .map(|(_, row)| row.id)
            .collect();
        let at = visible_order.iter().position(|id| *id == workspace)?;
        let to = at as isize + step;
        if to < 0 || to as usize >= visible_order.len() {
            return None;
        }
        let other = visible_order[to as usize];
        // The Store requires the owner's complete permutation, including rows
        // hidden in Archive or Trash. Swap the two visible neighbours in that
        // complete order so a hidden row cannot make a valid UI step fail as
        // an incomplete request.
        let mut owned: Vec<&Row> = self.rows.iter().filter(|row| row.owner == owner).collect();
        owned.sort_by(|left, right| {
            left.position
                .cmp(&right.position)
                .then(left.name.cmp(&right.name))
        });
        let mut order: Vec<WorkspaceId> = owned.into_iter().map(|row| row.id).collect();
        let workspace_at = order.iter().position(|id| *id == workspace)?;
        let other_at = order.iter().position(|id| *id == other)?;
        order.swap(workspace_at, other_at);
        Some((owner, order))
    }

    fn begin(&mut self, workspace: WorkspaceId, action: WorkspaceLifecycleAction, confirm: bool) {
        self.notice = None;
        self.lifecycle_owner = self.row(workspace).map(|row| row.owner);
        self.stage = Some(Stage::Asking { confirm });
        self.links.ask_consequence(workspace, action);
    }
}

impl Default for WorkspacesController {
    fn default() -> Self {
        Self::new()
    }
}

/// **The gateway's answers, for a host's suite** (the `fixtures` feature).
///
/// A host that draws this controller — the Manage tab's Profile pages — has
/// to show the confirmations and the notices, and the only way to reach those
/// states is an answer from the gateway, whose types are this crate's own.
/// Each of these is exactly what the link would deliver, through the same
/// `receive`, so a host's test exercises the real state machine rather than a
/// copy of it.
#[cfg(feature = "fixtures")]
impl WorkspacesController {
    /// The lifecycle verb an [`Action`] stands for, and its workspace —
    /// so a host names the fixture in the vocabulary it already speaks.
    fn fixture_verb(action: Action) -> (WorkspaceId, WorkspaceLifecycleAction) {
        match action {
            Action::Archive(workspace, _) => (workspace, WorkspaceLifecycleAction::Archive),
            Action::SoftDelete(workspace) => (workspace, WorkspaceLifecycleAction::SoftDelete),
            Action::Restore(workspace) => (workspace, WorkspaceLifecycleAction::Restore),
            other => panic!("{other:?} is not a lifecycle action"),
        }
    }

    /// The gateway said what `action` (archive or soft-delete) would close.
    pub fn fixture_consequence(&mut self, action: Action, terminals: &[&str]) -> bool {
        let (workspace, action) = Self::fixture_verb(action);
        self.receive(Answer::Consequence(Consequence {
            workspace,
            action,
            confirmation: "fixture-token".into(),
            terminals: terminals.iter().map(|name| (*name).to_owned()).collect(),
        }))
    }

    /// The gateway said what moving `workspace` to `to` would close.
    pub fn fixture_move_consequence(
        &mut self,
        workspace: WorkspaceId,
        to: ProfileId,
        to_name: &str,
        terminals: &[&str],
    ) -> bool {
        self.receive(Answer::MoveConsequence(MoveConsequence {
            workspace,
            to,
            to_name: to_name.to_owned(),
            confirmation: "fixture-token".into(),
            terminals: terminals.iter().map(|name| (*name).to_owned()).collect(),
        }))
    }

    /// The move committed, and the session host repaired its projects.
    pub fn fixture_moved(
        &mut self,
        workspace: WorkspaceId,
        from: ProfileId,
        to: ProfileId,
    ) -> bool {
        self.receive(Answer::Moved(koru_api_wire::WorkspaceMigrationOutcome {
            workspace: workspace.to_storage(),
            from: from.to_storage(),
            to: to.to_storage(),
            terminated_sessions: Vec::new(),
            sessions_closed: true,
            mutation_applied: true,
            projects_rescoped: Some(Vec::new()),
            replacement: None,
        }))
    }

    /// A lifecycle operation refused part-way (S24): the notice, with a retry
    /// of the same action on the same workspace.
    pub fn fixture_partial_failure(&mut self, action: Action) -> bool {
        let (workspace, action) = Self::fixture_verb(action);
        self.receive(Answer::Refused(Notice {
            text: REFUSAL_PARTIAL.to_owned(),
            retry: Some(Retry::Lifecycle(workspace, action)),
        }))
    }
}
