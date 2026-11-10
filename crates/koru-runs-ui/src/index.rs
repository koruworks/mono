// SPDX-License-Identifier: MPL-2.0

//! **The workspace's runs, as a list somebody can pick from.**
//!
//! The run views show one run; something has to say which runs there are.
//! This is that: `GET /v1/runs?workspace=…` on a thread of its own, folded
//! into rows a navigator can draw — a run id, the two-word name derived from
//! it, the objective, where it got to, whether it wants a look, and the
//! session this device is driving it in.
//!
//! It is deliberately not a surface. It draws nothing and lays nothing out,
//! because the list it feeds is a group in a tree the window already has,
//! and a second list beside that one would be two places to look.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::time::Duration;

use koru_api_client::Client;
use koru_delegate_api::wire::ListRunsResponse;
use koru_delegate_core::{
    ArtifactDisposition, DocumentId, FolderOutcome, RunHeader, RunId, RunName, SessionId,
    WorkspaceId,
};
use koru_i18n::Lang;

use crate::links::RUN_SCOPES;
use crate::words;

/// One run, as a row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunRow {
    pub run: RunId,
    /// **What a person calls this run** — two words derived from its id
    /// (ADR-360), the same on every device and stored nowhere. The objective
    /// is what was *asked for* and stays on the run's own Overview: a
    /// sentence is not a name you can point at in a list, or say out loud.
    pub name: String,
    /// The objective, as given. Carried beside the name so a surface with
    /// room for both can show what the run was asked to do.
    pub objective: String,
    /// Where it got to, in the person's own words.
    pub status: String,
    /// Whether it is asking for something (§7.5's markers, D29's included).
    pub needs_attention: bool,
    /// **The session this device is driving the run in**, when it is driving
    /// one. What lets a navigator draw a run's session underneath its run
    /// rather than in the bucket of sessions nothing has claimed.
    pub session: Option<SessionId>,
    pub created_at_ms: i64,
    /// **What is happening to this run**, by exactly the answer the worker's
    /// own poll is built on ([`motion`]). A navigator draws each of the three
    /// differently; anything but [`Motion::Still`] keeps the folder.
    pub motion: Motion,
    /// **How many results it registered.** A count, never the results
    /// themselves: what a surface does with it is ask a question — deleting
    /// a run that left something behind has to ask what happens to it, and
    /// deleting one that left nothing must not ask at all.
    pub results: u32,
}

impl RunRow {
    pub fn of(lang: Lang, header: &RunHeader, session: Option<SessionId>) -> Self {
        Self {
            run: header.run,
            name: RunName::of(header.run).to_string(),
            objective: header.objective.as_str().to_owned(),
            status: words::status(lang, &header.status),
            needs_attention: header
                .status
                .attention()
                .is_some_and(|attention| !attention.is_empty()),
            session,
            created_at_ms: header.created_at_ms,
            motion: motion(&header.status),
            results: u32::try_from(header.artifacts.len()).unwrap_or(u32::MAX),
        }
    }
}

/// **What a run is doing**, in the three answers a row can draw.
///
/// One predicate with three answers rather than two predicates, because the
/// two questions a surface asks — *which mark does this row wear* and *is
/// this list still worth watching* — are answered from one place and can
/// therefore never disagree. Two independent predicates would be a ring
/// spinning on a list nothing is refreshing, or a folder on a run that is
/// still going.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Motion {
    /// Something is moving this run without anybody's help — `Created`,
    /// `Starting`, `Running`. A navigator draws it turning.
    Working,
    /// Unfinished and **stopped**, waiting for a person to do something.
    ///
    /// `Interrupted` is the case that matters in M1: a restart ended the
    /// attempt and the run sits still until somebody presses Resume. It used
    /// to turn, because the rule was *every non-terminal state turns*, and a
    /// ring that goes round says work is happening when nothing is.
    ///
    /// A **pause** mark rather than a stop mark: `Interrupted` is the one
    /// ending that offers Resume, and a stop sign beside a Resume button says
    /// two different things about the same run.
    Paused,
    /// Finished, failed or cancelled. The folder, and nothing to watch.
    Still,
}

impl Motion {
    /// **Whether this run can still change**, which is what the worker's own
    /// poll ([`still_moving`]) and a trash refusal both ask.
    ///
    /// [`Self::Paused`] counts as unfinished, deliberately. Nothing is moving
    /// an interrupted run *by itself*, but a person resumes one — from this
    /// window's run tab, which does not touch the index, or on another
    /// device — and a list that stopped looking would keep saying
    /// *Interrupted* long after it stopped being true. Which mark to draw and
    /// how long to keep watching are two questions, and this is the one place
    /// they are answered apart.
    pub fn unfinished(self) -> bool {
        !matches!(self, Self::Still)
    }
}

/// The mark a run's row wears.
///
/// `AwaitingReview` and `WaitingForParticipant` are M5's and no transition
/// reaches them in M1. They answer [`Motion::Paused`] because that is what
/// their names say — a run waiting on a review or on a participant is a run
/// waiting on a person, and nothing is executing in it. It is also the
/// conservative answer *for a mark*: a spinner that lies about work being
/// done is the failure this enum exists to fix, whereas a pause on a state
/// nobody has defined yet costs nothing.
pub fn motion(status: &koru_delegate_core::RunStatus) -> Motion {
    use koru_delegate_core::RunStatus;
    match status {
        RunStatus::Created | RunStatus::Starting | RunStatus::Running { .. } => Motion::Working,
        RunStatus::Interrupted { .. }
        | RunStatus::AwaitingReview
        | RunStatus::WaitingForParticipant => Motion::Paused,
        RunStatus::Completed | RunStatus::Failed { .. } | RunStatus::Cancelled => Motion::Still,
    }
}

/// One run in the trash, as a row.
///
/// **The name, not the objective**, for the reason [`RunRow`] gives: a
/// sentence is not a name you can point at in a list, and the trash is a
/// list you point at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrashedRunRow {
    pub run: RunId,
    pub name: String,
    pub objective: String,
    pub status: String,
    /// The Run document, which is what the window moves in and out of the
    /// workspace's bin.
    pub document: DocumentId,
    pub trashed_at_ms: Option<i64>,
    /// What the person said should happen to its results when the trash is
    /// emptied.
    pub artifacts: ArtifactDisposition,
    /// Whether Koru made the folder this run worked in, and will therefore
    /// remove it. `false` is a run that worked inside the person's own
    /// project, whose folder emptying the trash leaves exactly alone --
    /// which the trash has to be able to say **before** it is emptied, not
    /// only afterwards.
    pub folder_is_ours: bool,
}

/// Which verb a refusal was about, so a surface can say what did not
/// happen without matching on a string.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DeletionVerb {
    Trash,
    Restore,
    Purge,
}

/// What the worker did about a run's deletion, for the window to finish.
///
/// **Every arm names the document**, because the other half of each verb is
/// a manifest edit only the window can make: the worker records and deletes
/// what is device-local, the window moves the Run document in or out of the
/// workspace's bin.
#[derive(Clone, Debug, PartialEq)]
pub enum DeletionAnswer {
    /// Recorded. Move the document into the bin.
    Trashed { run: RunId, document: DocumentId },
    /// Forgotten. Move the document back out.
    Restored { run: RunId, document: DocumentId },
    /// Deleted here. Remove the document from the manifest.
    Purged {
        run: RunId,
        document: DocumentId,
        folder: FolderOutcome,
        artifacts_retired: u32,
    },
    /// The worker refused, by its stable code. Nothing happened, and the
    /// window changes nothing.
    Refused {
        run: RunId,
        verb: DeletionVerb,
        code: String,
    },
}

enum Ask {
    Workspace(WorkspaceId),
    Refresh,
    Waker(Waker),
    Stop,
    Trash {
        run: RunId,
        artifacts: ArtifactDisposition,
    },
    Restore {
        run: RunId,
    },
    Purge {
        run: RunId,
    },
}

type Waker = Arc<dyn Fn() + Send + Sync + 'static>;

/// What the worker sends back.
enum Answered {
    Listing(Box<ListRunsResponse>),
    Deletion(DeletionAnswer),
}

/// The runs of one workspace, kept up to date off the window thread.
pub struct RunIndex {
    asks: Option<Sender<Ask>>,
    answers: Receiver<Answered>,
    listing: ListRunsResponse,
    workspace: Option<WorkspaceId>,
    lang: Lang,
    rows: Vec<RunRow>,
    trashed: Vec<TrashedRunRow>,
    /// What the worker did about a deletion, waiting for the window to
    /// finish it. Drained, never read twice.
    deletions: Vec<DeletionAnswer>,
    /// **Deletion verbs sent and not yet answered.**
    ///
    /// A window redraws when something asks it to, and a deletion's answer
    /// arrives on a thread that cannot. While this is above zero the host
    /// keeps asking for frames, so the receipt appears when the worker
    /// answers rather than when the pointer next moves — and the moment it
    /// reaches zero the asking stops, which is what keeps an idle surface
    /// idle (standing requirement 3).
    in_flight: usize,
}

impl Default for RunIndex {
    fn default() -> Self {
        Self::inert()
    }
}

impl RunIndex {
    /// An index with no worker behind it: it asks nothing and answers with
    /// no runs. What a test and a headless host hold.
    pub fn inert() -> Self {
        let (_, answers) = channel();
        Self::base(None, answers)
    }

    pub fn open(segment: &str, root: Option<std::path::PathBuf>) -> Self {
        let (asks, inbox) = channel::<Ask>();
        let (outbox, answers) = channel::<Answered>();
        let segment = segment.to_owned();
        let spawned = std::thread::Builder::new()
            .name("koru-run-index".into())
            .stack_size(256 * 1024)
            .spawn(move || list_runs(segment, root, inbox, outbox));
        match spawned {
            Ok(_) => Self::base(Some(asks), answers),
            Err(_) => Self::inert(),
        }
    }

    fn base(asks: Option<Sender<Ask>>, answers: Receiver<Answered>) -> Self {
        Self {
            asks,
            answers,
            listing: empty_listing(),
            workspace: None,
            lang: Lang::En,
            rows: Vec::new(),
            trashed: Vec::new(),
            deletions: Vec::new(),
            in_flight: 0,
        }
    }

    pub fn set_lang(&mut self, lang: Lang) -> bool {
        if self.lang == lang {
            return false;
        }
        self.lang = lang;
        self.rebuild();
        true
    }

    /// Which workspace's runs to list. Asking for the one already showing
    /// costs nothing.
    pub fn set_workspace(&mut self, workspace: Option<WorkspaceId>) {
        if self.workspace == workspace {
            return;
        }
        self.workspace = workspace;
        self.listing = empty_listing();
        self.rebuild();
        if let (Some(asks), Some(workspace)) = (&self.asks, workspace) {
            let _ = asks.send(Ask::Workspace(workspace));
        }
    }

    /// Ask again. Called when a run this window is showing changed, which
    /// is the only thing that moves the list without somebody looking.
    pub fn refresh(&self) {
        if let Some(asks) = &self.asks {
            let _ = asks.send(Ask::Refresh);
        }
    }

    /// Install the content-free event-loop wake used when the worker publishes
    /// a listing. The answer itself remains on this index's channel.
    pub fn set_waker(&self, wake: Waker) {
        if let Some(asks) = &self.asks {
            let _ = asks.send(Ask::Waker(wake));
        }
    }

    /// **Put a run in the trash**, with what to do about its results. The
    /// worker refuses one that is still working; the answer arrives through
    /// [`Self::take_deletions`].
    pub fn trash(&mut self, run: RunId, artifacts: ArtifactDisposition) {
        self.ask(Ask::Trash { run, artifacts });
    }

    /// Take a run back out of the trash.
    pub fn restore_run(&mut self, run: RunId) {
        self.ask(Ask::Restore { run });
    }

    /// **Empty the trash of this run**: everything device-local it left
    /// behind, and its working folder when Koru made it.
    pub fn purge(&mut self, run: RunId) {
        self.ask(Ask::Purge { run });
    }

    /// Whether a deletion verb is still waiting for its answer. What a host
    /// keeps asking for frames on; see [`Self::in_flight`].
    pub fn awaiting_deletion(&self) -> bool {
        self.in_flight > 0
    }

    fn ask(&mut self, ask: Ask) {
        // An inert index answers nothing, so nothing is in flight: a host
        // that kept redrawing for an answer that can never come would be a
        // window that never went idle.
        let Some(asks) = &self.asks else { return };
        if asks.send(ask).is_ok() {
            self.in_flight += 1;
        }
    }

    /// Take whatever the worker has said. Answers whether the rows moved.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(answered) = self.answers.try_recv() {
            match answered {
                Answered::Listing(listing) => {
                    self.listing = *listing;
                    changed = true;
                }
                Answered::Deletion(answer) => {
                    self.deletions.push(answer);
                    changed = true;
                }
            }
        }
        if changed {
            self.rebuild();
        }
        changed
    }

    pub fn rows(&self) -> &[RunRow] {
        &self.rows
    }

    /// The runs in the workspace's trash, newest deletion first.
    pub fn trashed(&self) -> &[TrashedRunRow] {
        &self.trashed
    }

    /// The Run document a run is registered as, when the listing named one.
    /// What the window moves in and out of the bin.
    pub fn document_of(&self, run: RunId) -> Option<DocumentId> {
        self.listing.documents.get(&run).copied()
    }

    /// What the worker did about a deletion, for the window to finish.
    /// Drained: each answer is acted on once.
    pub fn take_deletions(&mut self) -> Vec<DeletionAnswer> {
        let answers = std::mem::take(&mut self.deletions);
        self.in_flight = self.in_flight.saturating_sub(answers.len());
        answers
    }

    /// **Accept a listing this index did not read**, and rebuild from it.
    ///
    /// The seam an *inert* index is driven through — the same one
    /// `koru_artifacts_ui::ArtifactsSurface::take_catalog` gives the
    /// artifacts column and `RunLinks::scripted` gives a run view: a host
    /// that reaches runs some other way, and a suite that must reach none,
    /// both need a way to say *these are the runs* without a gateway
    /// answering.
    pub fn take_listing(&mut self, listing: ListRunsResponse) {
        self.listing = listing;
        self.rebuild();
    }

    /// **Accept an answer this index did not ask for**, on the same terms.
    /// It joins the queue [`Self::take_deletions`] drains, so the window's
    /// half of a deletion is exercised by exactly the path the worker's
    /// answer takes.
    pub fn take_deletion(&mut self, answer: DeletionAnswer) {
        self.deletions.push(answer);
    }

    fn rebuild(&mut self) {
        // Newest first: the run you are waiting on is the one you just
        // asked for.
        let mut rows: Vec<RunRow> = self
            .listing
            .runs
            .iter()
            .map(|header| {
                RunRow::of(
                    self.lang,
                    header,
                    self.listing.sessions.get(&header.run).copied(),
                )
            })
            .collect();
        rows.sort_by(|a, b| {
            b.created_at_ms
                .cmp(&a.created_at_ms)
                .then_with(|| a.run.cmp(&b.run))
        });
        self.rows = rows;
        // The worker already ordered the trash newest deletion first; this
        // only turns each one into words.
        self.trashed = self
            .listing
            .trashed
            .iter()
            .map(|trashed| TrashedRunRow {
                run: trashed.header.run,
                name: RunName::of(trashed.header.run).to_string(),
                objective: trashed.header.objective.as_str().to_owned(),
                status: words::status(self.lang, &trashed.header.status),
                document: trashed.document,
                trashed_at_ms: trashed.trashed_at_ms,
                artifacts: trashed.artifacts,
                folder_is_ours: trashed.header.scope.is_koru_owned(),
            })
            .collect();
    }
}

impl Drop for RunIndex {
    fn drop(&mut self) {
        if let Some(asks) = &self.asks {
            let _ = asks.send(Ask::Stop);
        }
    }
}

/// A listing with nothing in it: what an index holds before its first
/// answer, and what a workspace with no runs answers with.
fn empty_listing() -> ListRunsResponse {
    ListRunsResponse {
        runs: Vec::new(),
        sessions: Default::default(),
        documents: Default::default(),
        trashed: Vec::new(),
    }
}

/// How long the worker waits before checking whether its channel is gone.
const IDLE: Duration = Duration::from_millis(500);

/// How often the list is asked for again **while a run on it is still
/// working**. A run moves without anybody touching this window — it is being
/// driven by another process, and on another device it may be driven by
/// another machine — so a list that only answered when it was asked would
/// show *Working…* for a run that finished an hour ago.
///
/// It stops on its own: once every run in the answer is terminal, nothing is
/// going to change it except a new delegation, and the window says so
/// directly (`refresh`). An idle workspace therefore costs one channel wait
/// and no requests at all.
const WHILE_WORKING: Duration = Duration::from_secs(3);

/// A refresh explicitly triggered by a new schedule firing gets two delayed
/// recovery attempts. This is bounded work only after a failed request: a
/// healthy or persistently idle workspace still has no timer traffic.
const RETRY_AFTER: Duration = Duration::from_millis(200);
const REFRESH_RETRIES: u8 = 2;

/// Whether this list can change without anybody touching this window.
///
/// A run is driven by another process, and on a second device by another
/// machine, so *Working…* on a list that only answers when it is asked is a
/// word that can go stale for as long as the window stays open. While one run
/// is unfinished the worker looks again on its own; once every one of them has
/// stopped, only a new delegation can move the list, and the window says so
/// directly rather than paying for a timer that finds nothing.
fn still_moving(headers: &[RunHeader]) -> bool {
    headers
        .iter()
        .any(|header| motion(&header.status).unfinished())
}

fn list_runs(
    segment: String,
    root: Option<std::path::PathBuf>,
    inbox: Receiver<Ask>,
    outbox: Sender<Answered>,
) {
    let mut source = ApiRunSource {
        client: None,
        segment,
        root,
    };
    run_index_worker(inbox, outbox, &mut source);
}

trait RunSource {
    fn list(&mut self, workspace: WorkspaceId) -> Result<ListRunsResponse, ()>;
    fn perform(&mut self, verb: Ask) -> Option<DeletionAnswer>;
}

struct ApiRunSource {
    client: Option<Client>,
    segment: String,
    root: Option<std::path::PathBuf>,
}

impl RunSource for ApiRunSource {
    fn list(&mut self, workspace: WorkspaceId) -> Result<ListRunsResponse, ()> {
        let Some(open) = connected(&mut self.client, &self.segment, &self.root) else {
            return Err(());
        };
        match open.runs().list(workspace) {
            Ok(listed) => Ok(listed),
            Err(failure) => {
                if failure.retryable {
                    self.client = None;
                }
                Err(())
            }
        }
    }

    fn perform(&mut self, verb: Ask) -> Option<DeletionAnswer> {
        let open = connected(&mut self.client, &self.segment, &self.root)?;
        perform(open, verb)
    }
}

fn run_index_worker(inbox: Receiver<Ask>, outbox: Sender<Answered>, source: &mut dyn RunSource) {
    let mut workspace: Option<WorkspaceId> = None;
    let mut waker: Option<Waker> = None;
    // Whether the last answer held a run that could still move. It is what
    // decides between waiting to be asked and looking again.
    let mut working = false;
    let mut retries = 0;
    loop {
        let wait = match (retries > 0, working) {
            (true, _) => RETRY_AFTER,
            (false, true) => WHILE_WORKING,
            (false, false) => IDLE,
        };
        let asked = match inbox.recv_timeout(wait) {
            Ok(first) => {
                let mut batch = vec![first];
                loop {
                    match inbox.try_recv() {
                        Ok(ask) => batch.push(ask),
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => return,
                    }
                }
                let mut verbs = Vec::new();
                let mut requested = false;
                for ask in batch {
                    match ask {
                        Ask::Workspace(next) => {
                            workspace = Some(next);
                            requested = true;
                            retries = REFRESH_RETRIES;
                        }
                        Ask::Refresh => {
                            requested = true;
                            retries = REFRESH_RETRIES;
                        }
                        Ask::Waker(next) => waker = Some(next),
                        Ask::Stop => return,
                        verb => {
                            verbs.push(verb);
                            requested = true;
                        }
                    }
                }
                // **The verbs first, then the listing** -- one round trip
                // shows the answer rather than two, and a run that has just
                // been trashed must not come back in a listing taken before
                // it went.
                for verb in verbs {
                    let Some(answer) = source.perform(verb) else {
                        continue;
                    };
                    if !publish(&outbox, &waker, Answered::Deletion(answer)) {
                        return;
                    }
                }
                requested
            }
            Err(_) => match inbox.try_recv() {
                Err(TryRecvError::Disconnected) => return,
                _ if retries > 0 => {
                    retries -= 1;
                    true
                }
                // Nobody asked. Look anyway only while something is still
                // working, which is the whole of the timer's licence.
                _ => working,
            },
        };
        if !asked {
            continue;
        }
        let Some(workspace) = workspace else { continue };
        if let Ok(listed) = source.list(workspace) {
            retries = 0;
            working = still_moving(&listed.runs);
            if !publish(&outbox, &waker, Answered::Listing(Box::new(listed))) {
                return;
            }
        }
    }
}

fn publish(outbox: &Sender<Answered>, waker: &Option<Waker>, answer: Answered) -> bool {
    if outbox.send(answer).is_err() {
        return false;
    }
    if let Some(wake) = waker {
        wake();
    }
    true
}

/// The gateway session, opened on first need. A worker that could not
/// connect answers nothing and tries again on the next ask.
fn connected<'a>(
    client: &'a mut Option<Client>,
    segment: &str,
    root: &Option<std::path::PathBuf>,
) -> Option<&'a Client> {
    if client.is_none() {
        *client = match root {
            Some(root) => Client::connect_at_with_scopes(segment, root, RUN_SCOPES).ok(),
            None => Client::connect_with_scopes(segment, RUN_SCOPES).ok(),
        };
    }
    client.as_ref()
}

/// One deletion verb, and the answer the window finishes it from. `None`
/// for the two asks that are not verbs, which the loop has already taken
/// out of the batch.
fn perform(client: &Client, verb: Ask) -> Option<DeletionAnswer> {
    let runs = client.runs();
    match verb {
        Ask::Trash { run, artifacts } => Some(match runs.trash(run, artifacts) {
            Ok(answer) => DeletionAnswer::Trashed {
                run,
                document: answer.document,
            },
            Err(failure) => DeletionAnswer::Refused {
                run,
                verb: DeletionVerb::Trash,
                code: failure.code,
            },
        }),
        Ask::Restore { run } => Some(match runs.restore(run) {
            Ok(answer) => DeletionAnswer::Restored {
                run,
                document: answer.document,
            },
            Err(failure) => DeletionAnswer::Refused {
                run,
                verb: DeletionVerb::Restore,
                code: failure.code,
            },
        }),
        Ask::Purge { run } => Some(match runs.purge(run) {
            Ok(answer) => DeletionAnswer::Purged {
                run,
                document: answer.document,
                folder: answer.folder,
                artifacts_retired: answer.artifacts_retired,
            },
            Err(failure) => DeletionAnswer::Refused {
                run,
                verb: DeletionVerb::Purge,
                code: failure.code,
            },
        }),
        Ask::Workspace(_) | Ask::Refresh | Ask::Waker(_) | Ask::Stop => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use super::*;
    use koru_delegate_core::{
        ActorRef, AttentionSet, ControlState, EconomicsSummary, ExecutionEnvironment, FailureStage,
        Objective, ProfileId, RunStatus, SessionProfileKey, WorkspaceScope,
    };

    fn header(status: RunStatus) -> RunHeader {
        RunHeader {
            bot: None,
            native_results: Vec::new(),
            chat_delivery: None,
            run: RunId::from_uuid(uuid::Uuid::from_u128(0x51)),
            created_at_ms: 10,
            requested_by: ActorRef::Person {
                profile: ProfileId::from_uuid(uuid::Uuid::from_u128(1)),
            },
            objective: Objective::new("Tidy the release notes").unwrap(),
            agent: SessionProfileKey::BuiltIn("codex".parse().unwrap()),
            requested_model: None,
            origin: Default::default(),
            scope: WorkspaceScope::Temporary,
            context: Vec::new(),
            grants: Default::default(),
            status,
            failure_reason: None,
            environment: ExecutionEnvironment::local(koru_delegate_core::DeviceId::from_uuid(
                uuid::Uuid::from_u128(3),
            )),
            artifacts: Vec::new(),
            economics: EconomicsSummary::default(),
            rating: None,
        }
    }

    struct ScriptedSource {
        answers: Mutex<VecDeque<Result<ListRunsResponse, ()>>>,
        attempts: Sender<()>,
    }

    impl RunSource for ScriptedSource {
        fn list(&mut self, _workspace: WorkspaceId) -> Result<ListRunsResponse, ()> {
            self.attempts.send(()).unwrap();
            self.answers.lock().unwrap().pop_front().unwrap_or(Err(()))
        }

        fn perform(&mut self, _verb: Ask) -> Option<DeletionAnswer> {
            None
        }
    }

    /// **A row is named by the run, not by the objective** (ADR-360), and
    /// keeps the objective beside it.
    ///
    /// The navigator drew *"Need to research most of the…"* under *Runs* —
    /// a truncated sentence, which is not a name anybody can say out loud or
    /// point at. The name is derived from the id, so the same run reads the
    /// same on every device with nothing stored.
    #[test]
    fn a_row_is_named_after_the_run_and_keeps_the_objective() {
        let header = header(RunStatus::Created);
        let row = RunRow::of(Lang::En, &header, None);
        assert_eq!(row.name, RunName::of(header.run).to_string());
        assert_eq!(row.objective, "Tidy the release notes");
        assert_ne!(row.name, row.objective, "the name is not the objective");
        assert_eq!(row.session, None, "no session was named for it");
    }

    /// **The session a run is being driven in reaches the row** (ADR-361),
    /// which is what lets the navigator draw it under its run rather than in
    /// the bucket of sessions nothing has claimed.
    #[test]
    fn the_listing_puts_each_runs_session_on_its_row() {
        let driving = SessionId::mint();
        let mut index = RunIndex::inert();
        let one = header(RunStatus::running());
        let mut two = header(RunStatus::Completed);
        two.run = RunId::from_uuid(uuid::Uuid::from_u128(0x52));
        two.created_at_ms = 5;
        index.listing = ListRunsResponse {
            documents: Default::default(),
            trashed: Vec::new(),
            runs: vec![one.clone(), two.clone()],
            sessions: [(one.run, driving)].into_iter().collect(),
        };
        index.rebuild();

        let rows = index.rows();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].run, one.run, "newest first");
        assert_eq!(rows[0].session, Some(driving));
        assert_eq!(
            rows[1].session, None,
            "a run this device is not driving names no session"
        );
    }

    /// **A run the window is showing can move while nobody touches it**, so a
    /// list that answered once is a list that goes stale. The maintainers found this
    /// in acceptance from the other end: a run delegated a moment ago did not
    /// appear at all, because nothing asked again.
    #[test]
    fn the_list_keeps_looking_while_a_run_is_still_working() {
        assert!(
            still_moving(&[header(RunStatus::Running {
                attention: AttentionSet::default(),
                control: ControlState::Orchestrator,
            })]),
            "a working run moves without anybody asking"
        );
        assert!(
            still_moving(&[header(RunStatus::Created), header(RunStatus::Completed)]),
            "one unfinished run is enough"
        );
    }

    /// And stops when there is nothing left to see. A finished workspace costs
    /// no requests at all; a new delegation is announced by the window.
    #[test]
    fn a_list_of_finished_runs_asks_for_nothing() {
        assert!(!still_moving(&[]), "an empty workspace asks nothing");
        assert!(
            !still_moving(&[
                header(RunStatus::Completed),
                header(RunStatus::Cancelled),
                header(RunStatus::Failed {
                    stage: FailureStage::Turn
                }),
            ]),
            "every run has stopped, so only a new one can move the list"
        );
    }

    /// **Every state answers, and a ring never turns on a list nothing is
    /// watching.**
    ///
    /// Two questions from one predicate: which mark a row wears, and whether
    /// the worker looks again. They are not the same answer any more — an
    /// interrupted run is watched and does *not* turn — so what is asserted
    /// is the invariant that actually matters: everything that turns is
    /// watched. Over every status rather than the three a screenshot happened
    /// to show, so a state minted later cannot end up spinning on a list
    /// nothing refreshes.
    #[test]
    fn every_state_answers_and_anything_that_turns_is_still_being_watched() {
        let every = [
            RunStatus::Created,
            RunStatus::Starting,
            RunStatus::running(),
            RunStatus::Interrupted {
                prior_attempt: koru_delegate_core::PriorAttempt::Ended,
            },
            RunStatus::AwaitingReview,
            RunStatus::WaitingForParticipant,
            RunStatus::Completed,
            RunStatus::Cancelled,
            RunStatus::Failed {
                stage: FailureStage::Turn,
            },
        ];
        for status in every {
            let header = header(status.clone());
            let row = RunRow::of(Lang::En, &header, None);
            assert_eq!(row.motion, motion(&status));
            assert_eq!(
                row.motion.unfinished(),
                still_moving(std::slice::from_ref(&header)),
                "{status:?}: the row and the poll disagreed"
            );
            if row.motion == Motion::Working {
                assert!(
                    row.motion.unfinished(),
                    "{status:?}: a turning ring on a list nothing is refreshing"
                );
            }
        }

        // **The interruption is the finding.** A restart ended the attempt;
        // nothing is running, and the run waits for a person to press Resume.
        // It is watched, because that Resume can come from the run's own tab
        // or from another device, and it does not turn.
        let interrupted = motion(&RunStatus::Interrupted {
            prior_attempt: koru_delegate_core::PriorAttempt::Ended,
        });
        assert_eq!(interrupted, Motion::Paused);
        assert!(interrupted.unfinished());

        // And the three the report named keep the folder.
        for terminal in [
            RunStatus::Completed,
            RunStatus::Cancelled,
            RunStatus::Failed {
                stage: FailureStage::Turn,
            },
        ] {
            assert_eq!(motion(&terminal), Motion::Still, "{terminal:?} is finished");
            assert!(!motion(&terminal).unfinished());
        }
    }

    /// **An inert index never says it is waiting.** A host keeps asking for
    /// frames while a deletion verb is in flight; an index with no worker
    /// behind it can never be answered, and a window that redrew for an
    /// answer that cannot come is a window that never goes idle.
    #[test]
    fn an_inert_index_is_never_awaiting_an_answer() {
        let mut index = RunIndex::inert();
        assert!(!index.awaiting_deletion());
        index.trash(
            RunId::from_uuid(uuid::Uuid::from_u128(1)),
            ArtifactDisposition::Keep,
        );
        index.purge(RunId::from_uuid(uuid::Uuid::from_u128(1)));
        assert!(!index.awaiting_deletion());
    }

    /// **Draining the answer stops the asking.** The count is what a host
    /// reads; an answer taken is a verb no longer in flight.
    #[test]
    fn an_answer_taken_ends_the_wait() {
        let mut index = RunIndex::inert();
        let run = RunId::from_uuid(uuid::Uuid::from_u128(2));
        // Pushed the way a worker's answer arrives, since an inert index has
        // no worker to send one.
        index.in_flight = 1;
        assert!(index.awaiting_deletion());
        index.take_deletion(DeletionAnswer::Restored {
            run,
            document: DocumentId::from_uuid(uuid::Uuid::from_u128(3)),
        });
        assert_eq!(index.take_deletions().len(), 1);
        assert!(!index.awaiting_deletion());
    }

    #[test]
    fn publishing_a_worker_listing_wakes_one_maintenance_pass() {
        let (outbox, answers) = channel();
        let wakes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let wake: Waker = Arc::new({
            let wakes = Arc::clone(&wakes);
            move || {
                wakes.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        });

        assert!(publish(
            &outbox,
            &Some(wake),
            Answered::Listing(Box::new(empty_listing())),
        ));
        assert!(matches!(answers.try_recv(), Ok(Answered::Listing(_))));
        assert_eq!(wakes.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert!(
            answers.try_recv().is_err(),
            "idle publishes no second answer"
        );
    }

    #[test]
    fn failed_refresh_retries_then_wakes_and_settles_without_polling() {
        let listed = ListRunsResponse {
            runs: vec![header(RunStatus::Completed)],
            sessions: Default::default(),
            documents: Default::default(),
            trashed: Vec::new(),
        };
        let (asks, inbox) = channel();
        let (outbox, answers) = channel();
        let (attempted, attempts) = channel();
        let mut source = ScriptedSource {
            answers: Mutex::new(VecDeque::from([Err(()), Ok(listed)])),
            attempts: attempted,
        };
        let worker = std::thread::spawn(move || run_index_worker(inbox, outbox, &mut source));
        let mut index = RunIndex::base(Some(asks), answers);
        let (woke, wakes) = channel();
        index.set_waker(Arc::new(move || {
            let _ = woke.send(());
        }));
        index.set_workspace(Some(WorkspaceId::new()));

        wakes.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(index.poll());
        assert_eq!(index.rows().len(), 1);
        attempts.recv_timeout(Duration::from_secs(1)).unwrap();
        attempts.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(
            attempts.recv_timeout(RETRY_AFTER * 2).is_err(),
            "success clears the unhealthy-only retry state"
        );
        drop(index);
        worker.join().unwrap();
    }

    #[test]
    fn persistent_refresh_failure_is_bounded_and_never_wakes_an_empty_answer() {
        let (asks, inbox) = channel();
        let (outbox, _answers) = channel();
        let (attempted, attempts) = channel();
        let mut source = ScriptedSource {
            answers: Mutex::new(VecDeque::new()),
            attempts: attempted,
        };
        let worker = std::thread::spawn(move || run_index_worker(inbox, outbox, &mut source));
        let (woke, wakes) = channel();
        asks.send(Ask::Waker(Arc::new(move || {
            let _ = woke.send(());
        })))
        .unwrap();
        asks.send(Ask::Workspace(WorkspaceId::new())).unwrap();

        for _ in 0..=REFRESH_RETRIES {
            attempts.recv_timeout(Duration::from_secs(1)).unwrap();
        }
        assert!(
            attempts.recv_timeout(RETRY_AFTER * 2).is_err(),
            "persistent failure stops after the bounded recovery attempts"
        );
        assert!(wakes.try_recv().is_err(), "no answer means no false wake");
        asks.send(Ask::Stop).unwrap();
        worker.join().unwrap();
    }
}
