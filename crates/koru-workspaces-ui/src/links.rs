// SPDX-License-Identifier: MPL-2.0

//! **The one thread that talks to the gateway, and the two channels either
//! side of it.**
//!
//! Every call this surface makes is blocking — the catalog is an HTTP round
//! trip, and the lifecycle operation closes terminals and reaches a Store — so
//! none of them may happen inside a frame. A command goes down, an [`Answer`]
//! comes back, and the worker wakes the owning window after enqueueing it.
//! The window drains the inbox during maintenance.
//!
//! **Connecting is the worker's first job, not the constructor's.** Opening a
//! surface must not wait on a gateway; a window that could not connect draws
//! its rows empty and says so (S9), and a gateway that arrives later is picked
//! up by the next command.
//!
//! **The grant is asked for here, from this process.** The Agent vouches for a
//! connection whose transport-read pid is running the `koru` beside it, so the
//! request must come from the window that wants the scope — which this thread
//! is part of. It is not something the host can fetch and hand over, and it is
//! not something a host *has* to do: the surface asks, once, at connect.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};

use koru_api_client::{CatalogChange, CatalogFlags, Client, WorkspaceError};
use koru_api_wire::WorkspaceLifecycleAction;
use koru_store_core::ids::{ProfileId, WorkspaceId};

use crate::state::{
    Answer, Authorization, Consequence, MoveConsequence, Notice, ProfileRow, Retry, Row,
};

#[cfg(any(test, feature = "fixtures"))]
mod fixtures;
#[cfg(test)]
mod tests;
#[cfg(any(test, feature = "fixtures"))]
pub use fixtures::WorkspaceFixture;

/// The scopes a management window asks for.
///
/// **Naming any scope filters rather than adds**, so the ordinary ones this
/// surface still needs — listing the catalog, renaming, recolouring,
/// reordering — have to be named alongside the destructive one. `events.read`
/// is here for the same reason: a manager that could remove a workspace but
/// could not hear that another window removed one would be the stalest surface
/// in the app.
pub const MANAGER_SCOPES: &[&str] = &[
    "store.read_documents",
    "store.manage_workspaces",
    koru_api_client::WORKSPACE_DESTROY_SCOPE,
    // **Named separately, because it is a separate permission.** A vouched
    // window is granted only what it asks for, so a manager that named only the
    // destructive scope would be admitted able to remove a workspace and unable
    // to move one (ADR-268). Both are asked for here; a window the Agent will
    // not vouch for gets neither and draws both rows dimmed with their own
    // reasons.
    koru_api_client::WORKSPACE_MIGRATE_SCOPE,
    koru_api_client::EVENTS_READ_SCOPE,
];

/// What the surface asks the worker to do.
enum Command {
    Keyed(crate::CommandId, Box<Command>),
    /// Read the including-deleted catalog.
    Catalog,
    /// Read the profiles, which is where a move can send a workspace.
    Profiles,
    CreateProfile(koru_api_client::CreateProfileWithWorkspace),
    KeepProfile(ProfileId, koru_store_core::ids::RemovalAttemptId),
    EnsureLive {
        owner: ProfileId,
        name: String,
        attempt: u64,
    },
    /// Ask whether this session may remove a workspace at all.
    Probe,
    /// The same question about moving one. Two probes, because a window can
    /// hold either scope without the other.
    ProbeMigration,
    /// Compute what an action would do, and issue the token for it.
    Consequence(WorkspaceId, WorkspaceLifecycleAction),
    /// Perform it, with the token that was accepted.
    Lifecycle(WorkspaceId, WorkspaceLifecycleAction, String, String),
    /// Compute what moving it to a profile would close, and issue the token.
    MoveConsequence(WorkspaceId, ProfileId, String),
    /// Perform the move, with the token that was accepted.
    Migrate(WorkspaceId, ProfileId, String, String),
    /// The whole appearance set, which is how the route takes it.
    Appearance {
        workspace: WorkspaceId,
        icon: Option<String>,
        color: Option<String>,
        pinned: bool,
        archived: bool,
    },
    Reorder(ProfileId, Vec<WorkspaceId>),
    Pin(WorkspaceId, bool),
    ReconcileListing(WorkspaceId),
}

/// How the surface reaches the gateway, or the fact that it does not.
///
/// Four tiers, the same four `SettingsLinks` has and for the same reasons:
/// [`Self::disabled`] for a surface nothing feeds and nothing dials,
/// [`Self::inert`] for a host's own `cfg(test)`, [`Self::open`] for the real
/// thing, and [`Self::first_party`] as the named arrangement every shipping
/// host uses.
pub struct WorkspaceLinks {
    live: Option<Live>,
}

struct Live {
    commands: Sender<Command>,
    answers: Receiver<Answer>,
    /// **Whether the worker's end has already been mourned.**
    ///
    /// A dropped sender makes `try_recv` answer `Disconnected` *for ever*, and
    /// the surface drains answers in a loop until there are none — so without
    /// this, a worker that failed to connect turns the next `poll` into an
    /// infinite one. Found by a Koru test hanging rather than failing, which is
    /// how this class of bug announces itself.
    ended: std::cell::Cell<bool>,
    /// Set by the catalog subscription's thread and taken when the surface
    /// draws. Sticky until taken, so a fact is never missed because nobody was
    /// looking (the `CatalogFlags` contract).
    flags: Arc<CatalogFlags>,
    /// Transport receipts still owed, including if the worker exits early.
    awaiting: RefCell<VecDeque<crate::CommandId>>,
    _inert_orders: Option<Receiver<Command>>,
    _inert_answers: Option<Sender<Answer>>,
}

impl WorkspaceLinks {
    /// No link. The host feeds every row and nothing dials anything.
    pub fn disabled() -> Self {
        Self { live: None }
    }

    /// **A link that answers nothing and never connects.**
    ///
    /// What a host constructs under `cfg(test)`: a guard inside this crate
    /// cannot cover `apps/koru`, because a dependency is never compiled with
    /// `cfg(test)`. It reports itself as held, so a host's conformance test can
    /// assert the surface was handed one without any test dialling a gateway
    /// somebody has running.
    pub fn inert() -> Self {
        let (commands, sunk) = channel();
        let (never, answers) = channel();
        // **Both ends are kept alive and neither is used.** A command send
        // succeeds and goes nowhere; an answer never arrives and the channel
        // never reports itself closed. Dropping either would make this link
        // *dead* rather than *quiet*, which is a different state with different
        // behaviour — and the one a host's suite must not be arranging by
        // accident.
        Self {
            live: Some(Live {
                commands,
                answers,
                ended: std::cell::Cell::new(false),
                flags: Arc::new(CatalogFlags::default()),
                awaiting: RefCell::default(),
                _inert_orders: Some(sunk),
                _inert_answers: Some(never),
            }),
        }
    }

    /// Open a link, and return at once.
    ///
    /// `segment` is the release channel's segment; `root` is an alternate
    /// installation root. Both are the client's own arguments — nothing here
    /// resolves a path.
    pub fn open(
        segment: &str,
        root: Option<std::path::PathBuf>,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        let (commands, orders) = channel();
        let (answers_out, answers) = channel();
        let flags = Arc::new(CatalogFlags::default());
        let theirs = Arc::clone(&flags);
        let segment = segment.to_owned();
        let _ = std::thread::Builder::new()
            .name("koru-workspaces-link".into())
            .spawn(move || serve(&segment, root, &orders, &answers_out, &theirs, wake));
        Self {
            live: Some(Live {
                commands,
                answers,
                ended: std::cell::Cell::new(false),
                flags,
                awaiting: RefCell::default(),
                _inert_orders: None,
                _inert_answers: None,
            }),
        }
    }

    /// **The arrangement every shipping host uses**: this channel's gateway,
    /// asking the Agent to vouch for this window.
    pub fn first_party(segment: &str, wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self::open(segment, None, wake)
    }

    /// A link whose worker has already gone — the state [`Self::open`] reaches
    /// when it cannot connect. Test-only, because the failure it stands for is
    /// the one that used to hang the surface rather than report itself.
    #[cfg(test)]
    fn open_ended() -> Self {
        let (commands, sunk) = channel();
        let (_gone, answers) = channel::<Answer>();
        Self {
            live: Some(Live {
                commands,
                answers,
                ended: std::cell::Cell::new(false),
                flags: Arc::new(CatalogFlags::default()),
                awaiting: RefCell::default(),
                _inert_orders: Some(sunk),
                _inert_answers: None,
            }),
        }
    }

    /// Whether this surface holds a link at all — so a host that feeds the rows
    /// itself is not also handed one.
    pub fn is_held(&self) -> bool {
        self.live.is_some()
    }

    // ---------------------------------------------------------- the surface

    /// The next answer, or nothing.
    ///
    /// **A closed channel is reported once and then falls silent.** The caller
    /// drains in a loop, and `try_recv` answers `Disconnected` every time it is
    /// asked — so a worker that failed to connect would otherwise turn the
    /// surface's next `poll` into a loop that never ends. The worker being gone
    /// is one fact, not an unlimited supply of them.
    pub(crate) fn take(&self) -> Option<Answer> {
        let live = self.live.as_ref()?;
        match live.answers.try_recv() {
            Ok(answer) => {
                if let Answer::Settled(id, _) = &answer {
                    live.awaiting.borrow_mut().retain(|awaiting| awaiting != id);
                }
                Some(answer)
            }
            Err(TryRecvError::Empty) => None,
            // The worker is gone, which is the one failure this cannot recover
            // from by asking again — so it is said once.
            Err(TryRecvError::Disconnected) => {
                if let Some(id) = live.awaiting.borrow_mut().pop_front() {
                    return Some(Answer::Settled(id, Box::new(Answer::Unavailable)));
                }
                match live.ended.replace(true) {
                    true => None,
                    false => Some(Answer::Unavailable),
                }
            }
        }
    }

    /// Whether a committed catalog fact has arrived since this was last asked.
    ///
    /// Clears the flag: a surface opening twice with nothing in between fetches
    /// once, which is the whole of invalidate-on-event/fetch-on-open.
    pub(crate) fn take_stale(&self) -> bool {
        self.live
            .as_ref()
            .is_some_and(|live| live.flags.take(CatalogChange::Workspaces))
    }

    pub(crate) fn take_profiles_stale(&self) -> bool {
        self.live
            .as_ref()
            .is_some_and(|live| live.flags.take(CatalogChange::Profiles))
    }

    /// Mark the catalog stale without waiting for the round trip — what a
    /// surface does after its own mutation commits.
    pub(crate) fn invalidate(&self) {
        if let Some(live) = &self.live {
            live.flags.invalidate(CatalogChange::Workspaces);
        }
    }

    pub(crate) fn fetch_catalog(&self) {
        self.send(Command::Catalog);
    }

    pub(crate) fn fetch_profiles(&self) {
        self.send(Command::Profiles);
    }

    pub(crate) fn probe_authorization(&self) {
        self.send(Command::Probe);
        self.send(Command::ProbeMigration);
    }

    pub(crate) fn ask_move_consequence(
        &self,
        workspace: WorkspaceId,
        to: ProfileId,
        to_name: String,
    ) {
        self.send(Command::MoveConsequence(workspace, to, to_name));
    }

    pub(crate) fn perform_move(&self, consequence: &MoveConsequence, name: &str) {
        self.send(Command::Migrate(
            consequence.workspace,
            consequence.to,
            consequence.confirmation.clone(),
            name.to_owned(),
        ));
    }

    pub(crate) fn ask_consequence(&self, workspace: WorkspaceId, action: WorkspaceLifecycleAction) {
        self.send(Command::Consequence(workspace, action));
    }

    pub(crate) fn perform(&self, consequence: &Consequence, name: &str) {
        self.send(Command::Lifecycle(
            consequence.workspace,
            consequence.action,
            consequence.confirmation.clone(),
            name.to_owned(),
        ));
    }

    pub(crate) fn set_appearance(
        &self,
        id: crate::CommandId,
        row: &Row,
        pinned: bool,
        archived: bool,
    ) {
        self.send(Command::Keyed(
            id,
            Box::new(Command::Appearance {
                workspace: row.id,
                icon: row.icon.clone(),
                color: row.color.clone(),
                pinned,
                archived,
            }),
        ));
    }

    pub(crate) fn reorder(&self, id: crate::CommandId, owner: ProfileId, order: Vec<WorkspaceId>) {
        self.send(Command::Keyed(id, Box::new(Command::Reorder(owner, order))));
    }

    pub(crate) fn pin(&self, id: crate::CommandId, workspace: WorkspaceId, pinned: bool) {
        self.send(Command::Keyed(
            id,
            Box::new(Command::Pin(workspace, pinned)),
        ));
    }

    pub(crate) fn reconcile_listing(&self, id: crate::CommandId, workspace: WorkspaceId) {
        self.send(Command::Keyed(
            id,
            Box::new(Command::ReconcileListing(workspace)),
        ));
    }

    pub(crate) fn create_profile(
        &self,
        id: crate::CommandId,
        request: koru_api_client::CreateProfileWithWorkspace,
    ) {
        self.send(Command::Keyed(
            id,
            Box::new(Command::CreateProfile(request)),
        ));
    }

    pub(crate) fn ensure_live(
        &self,
        id: crate::CommandId,
        owner: ProfileId,
        name: String,
        attempt: u64,
    ) {
        self.send(Command::Keyed(
            id,
            Box::new(Command::EnsureLive {
                owner,
                name,
                attempt,
            }),
        ));
    }

    pub(crate) fn keep_profile(
        &self,
        id: crate::CommandId,
        owner: ProfileId,
        attempt: koru_store_core::ids::RemovalAttemptId,
    ) {
        self.send(Command::Keyed(
            id,
            Box::new(Command::KeepProfile(owner, attempt)),
        ));
    }

    fn send(&self, command: Command) {
        if let Some(live) = &self.live {
            if let Command::Keyed(id, _) = &command {
                live.awaiting.borrow_mut().push_back(*id);
            }
            // A dead worker is reported by the answer channel, which the
            // surface is already polling. Failing here as well would be two
            // places to notice the same thing.
            let _ = live.commands.send(command);
        }
    }
}

impl std::fmt::Debug for WorkspaceLinks {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkspaceLinks")
            .field("held", &self.is_held())
            .finish()
    }
}

// ------------------------------------------------------------------- the worker

fn serve(
    segment: &str,
    root: Option<std::path::PathBuf>,
    orders: &Receiver<Command>,
    answers: &Sender<Answer>,
    flags: &Arc<CatalogFlags>,
    wake: Arc<dyn Fn() + Send + Sync>,
) {
    let client = match connect(segment, root.as_deref()) {
        Some(client) => client,
        None => {
            publish_answer(answers, &*wake, Answer::Unavailable);
            return;
        }
    };

    // **Watch before the first fetch.** Starting the subscription second would
    // leave a window in which a workspace was removed, the fetch answered with
    // it still present, and nothing ever said otherwise.
    let stale = Arc::clone(flags);
    let catalog_wake = Arc::clone(&wake);
    let _watch = client
        .events()
        .watch_catalog(move |change| {
            invalidate_and_wake(&stale, &*catalog_wake, change);
        })
        .ok();

    while let Ok(command) = orders.recv() {
        let answer = run(&client, command);
        if !publish_answer(answers, &*wake, answer) {
            return; // the surface is gone
        }
    }
}

fn publish_answer(answers: &Sender<Answer>, wake: &dyn Fn(), answer: Answer) -> bool {
    let delivered = answers.send(answer).is_ok();
    wake();
    delivered
}

fn invalidate_and_wake(flags: &CatalogFlags, wake: &dyn Fn(), change: CatalogChange) {
    flags.invalidate(change);
    wake();
}

/// **Ask the supervisor, then connect.**
///
/// One attempt and no retry loop: a window that opens its manager before the
/// gateway is up draws S9 and refetches when a command is next sent, which is
/// what invalidate-on-event/fetch-on-open already does for every other reason
/// the catalog moves.
fn connect(segment: &str, root: Option<&std::path::Path>) -> Option<Client> {
    match root {
        Some(root) => Client::connect_at_first_party(segment, root, MANAGER_SCOPES),
        None => Client::connect_first_party(segment, MANAGER_SCOPES),
    }
    .ok()
}

fn run(client: &Client, command: Command) -> Answer {
    match command {
        Command::Keyed(id, command) => Answer::Settled(id, Box::new(run(client, *command))),
        Command::Pin(workspace, pinned) => {
            Answer::Pinned(client.store().set_workspace_pinned(workspace, pinned))
        }
        Command::ReconcileListing(workspace) => {
            Answer::ListingReconciled(client.store().reconcile_workspace_listing(workspace))
        }
        Command::CreateProfile(request) => {
            Answer::ProfileCreated(client.store().create_profile_with_workspace(&request))
        }
        Command::EnsureLive {
            owner,
            name,
            attempt,
        } => Answer::WorkspaceEnsured {
            attempt,
            result: client.store().ensure_live_workspace(owner, &name),
        },
        Command::Catalog => catalog_answer(client),
        Command::KeepProfile(owner, attempt) => {
            Answer::ProfileKept(client.catalog().keep_profile(owner, attempt))
        }
        Command::Profiles => match client.store().profiles() {
            Ok(profiles) => Answer::Profiles(
                profiles
                    .into_iter()
                    .map(|profile| ProfileRow {
                        id: profile.id,
                        name: profile.name,
                    })
                    .collect(),
            ),
            // **Not `Unavailable`.** The list of destinations failing is not
            // the catalog failing; the rows on screen are still true, and the
            // move row simply has nowhere to send anything until the next
            // fetch. Reporting the whole surface as down would grey out
            // everything because one extra call did not answer.
            Err(_) => Answer::Profiles(Vec::new()),
        },
        Command::Probe => Answer::Authorization(match client.catalog().may_destroy() {
            true => Authorization::Vouched,
            false => Authorization::Refused,
        }),
        Command::ProbeMigration => {
            Answer::MigrationAuthorization(match client.catalog().may_migrate() {
                true => Authorization::Vouched,
                false => Authorization::Refused,
            })
        }
        Command::Consequence(workspace, action) => {
            match client
                .catalog()
                .confirmation(&workspace.to_storage(), action)
            {
                Ok(issued) => Answer::Consequence(Consequence {
                    workspace,
                    action,
                    confirmation: issued.confirmation,
                    terminals: issued
                        .sessions
                        .into_iter()
                        .map(|session| session.display_name)
                        .collect(),
                }),
                Err(error) => Answer::Refused(refusal(workspace, action, &error, false)),
            }
        }
        Command::Lifecycle(workspace, action, confirmation, name) => {
            match client.catalog().lifecycle_replacing(
                &workspace.to_storage(),
                action,
                &confirmation,
                &name,
            ) {
                Ok(outcome) => Answer::Completed(outcome),
                Err(error) => Answer::Refused(refusal(workspace, action, &error, true)),
            }
        }
        Command::MoveConsequence(workspace, to, to_name) => {
            match client
                .catalog()
                .migration_confirmation(&workspace.to_storage(), &to.to_storage())
            {
                Ok(issued) => Answer::MoveConsequence(MoveConsequence {
                    workspace,
                    to,
                    to_name,
                    confirmation: issued.confirmation,
                    terminals: issued
                        .sessions
                        .into_iter()
                        .map(|session| session.display_name)
                        .collect(),
                }),
                Err(error) => Answer::Refused(move_refusal(workspace, to, &error, false)),
            }
        }
        Command::Migrate(workspace, to, confirmation, name) => {
            match client.catalog().migrate_replacing(
                &workspace.to_storage(),
                &to.to_storage(),
                &confirmation,
                &name,
            ) {
                Ok(outcome) => Answer::Moved(outcome),
                Err(error) => Answer::Refused(move_refusal(workspace, to, &error, true)),
            }
        }
        Command::Appearance {
            workspace,
            icon,
            color,
            pinned,
            archived,
        } => match client.store().set_workspace_appearance(
            workspace,
            icon.as_deref(),
            color.as_deref(),
            pinned,
            archived,
        ) {
            Ok(_) => catalog_answer(client),
            Err(_) => Answer::Refused(Notice {
                text: crate::REFUSAL_UNAVAILABLE.into(),
                retry: None,
            }),
        },
        Command::Reorder(owner, order) => match client.store().reorder_workspaces(owner, &order) {
            Ok(_) => catalog_answer(client),
            Err(_) => Answer::Refused(Notice {
                text: crate::REFUSAL_UNAVAILABLE.into(),
                retry: None,
            }),
        },
    }
}

mod authority;
use authority::catalog_answer;

/// **The refusal, in the one vocabulary the surface localizes from.**
///
/// The worker classifies; the surface words it. The text is left empty here on
/// purpose — a worker thread does not know which language the window is drawing
/// in, and it must not: `lib.rs` fills every one of these from a key.
fn refusal(
    workspace: WorkspaceId,
    action: WorkspaceLifecycleAction,
    error: &WorkspaceError,
    performed: bool,
) -> Notice {
    Notice {
        text: classify(error).into(),
        // **Only S24 offers a retry.** Everything else either cannot be helped
        // by pressing again (no permission, open documents, no such workspace)
        // or never got as far as doing anything, in which case the affordance
        // is the row itself.
        retry: (performed && error.partial_outcome().is_some() && !error.documents_connected())
            .then_some(Retry::Lifecycle(workspace, action)),
    }
}

/// The same, for a move.
fn move_refusal(
    workspace: WorkspaceId,
    to: ProfileId,
    error: &WorkspaceError,
    performed: bool,
) -> Notice {
    Notice {
        text: classify_move(error).into(),
        retry: (performed && error.partial_migration().is_some())
            .then_some(Retry::Migration(workspace, to)),
    }
}

/// The move's classifications.
///
/// **`documents_connected` is not among them, and cannot be.** A move touches
/// no room, so the Store never answers `workspace_in_use` for one (ADR-268). A
/// branch for it here would be a branch that can never be taken, and a sentence
/// telling somebody to close their tabs before moving a workspace would be
/// advice about a refusal that does not exist.
pub(crate) fn classify_move(error: &WorkspaceError) -> &'static str {
    if error.partial_migration().is_some() {
        return crate::REFUSAL_MOVE_PARTIAL;
    }
    if error.is_permission_denied() {
        return crate::REFUSAL_MOVE_NOT_PERMITTED;
    }
    // The destination went away between the picker and the commit — the one
    // refusal a person can fix by choosing again.
    if matches!(error, WorkspaceError::Api(failure) if failure.status == 404) {
        return crate::REFUSAL_MOVE_NO_SUCH_PROFILE;
    }
    crate::REFUSAL_UNAVAILABLE
}

/// The classification the surface turns into a sentence. A stable token rather
/// than a message, so the words live in the i18n table and this thread stays
/// language-blind.
pub(crate) fn classify(error: &WorkspaceError) -> &'static str {
    // A partial response keeps the Store's nested cause. Prefer that
    // actionable fact to the generic partial-outcome sentence: retrying while
    // a Koru document is still connected can only produce the same refusal.
    if error.documents_connected() {
        return crate::REFUSAL_DOCUMENTS_OPEN;
    }
    if let Some(outcome) = error.partial_outcome() {
        return if outcome.authority_committed == Some(true) {
            crate::REFUSAL_LISTING_PENDING
        } else {
            crate::REFUSAL_PARTIAL
        };
    }
    if error.is_permission_denied() {
        return crate::REFUSAL_NOT_PERMITTED;
    }
    crate::REFUSAL_UNAVAILABLE
}

fn row_of(workspace: koru_store_core::model::Workspace) -> Row {
    Row {
        id: workspace.id,
        owner: workspace.owner,
        name: workspace.name,
        icon: workspace.icon,
        color: workspace.color,
        pinned: workspace.pinned,
        archived: workspace.archived,
        deleted: workspace.deleted_at.is_some(),
        position: workspace.position,
        last_opened_at: workspace.last_opened_at.map(|at| at.millis()),
        authority: crate::Authority::Unknown,
        listing_differs: false,
    }
}
