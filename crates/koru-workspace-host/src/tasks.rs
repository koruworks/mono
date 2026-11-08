// SPDX-License-Identifier: MPL-2.0

//! **The desktop task wrapper** ([ADR-286]).
//!
//! A wrapper adds I/O scheduling and attribution to a canonical command and
//! *nothing else*. It never validates, never decides a lifecycle, never reads a
//! projection row, and never invents an event the outcome did not name. That is
//! why it is shaped as a closure taker — [`TaskHost::run`] — rather than a
//! method per command: "a host runs a command" is then structural rather than a
//! convention somebody has to keep.
//!
//! What this host owes a task gesture, and where each half runs:
//!
//! - **Synchronous, on the event thread**: the command (a pure in-memory CRDT
//!   edit), the [`TaskOutcome`], `Workspace::touched()` and the view refresh.
//!   That is all. Success shows at the in-memory edit, before anything is
//!   acknowledged — the app's existing optimistic model, and why a task ticks
//!   the moment you click it.
//! - **Off the event thread**: *nothing new*. Persistence is the host's
//!   existing path — the dispatch loop sees the document version move, calls
//!   `Workspace::touched()`, and `Workspace::tick` publishes; the snapshot
//!   worker gets a `fork_for_read` over a channel. This module adds no second
//!   path and no durability wait, ever. Activity leaves through
//!   [`crate::events_host::emit`], which is documented as never blocking and
//!   never failing: one `mpsc` send onto the events thread's channel.
//! - **A persistence failure** surfaces exactly as every other edit's does,
//!   through `Workspace::owed()` / `Workspace::store_trouble()` and the banner
//!   the host already polls for. **Tasks add no second failure channel** — an
//!   unsaved task is the same problem as an unsaved paragraph.
//!
//! **A desktop task gesture therefore performs no disk or network I/O on the
//! event thread**, and that is structural rather than reviewed: [`run`] is
//! handed a `&mut WorkspaceDoc` and never a `Workspace`, a `Session` or a
//! client, so it *cannot* reach a file or a socket even by mistake. The suite
//! at the bottom of this file pins both halves.
//!
//! **Attribution is `Actor::user()`**, and with the causation id the command
//! minted it is the whole of what this wrapper adds to an intent. MCP's wrapper
//! ([`koru-mcp-server`], F-8) differs in exactly two places — a durability wait
//! and `Actor::agent("mcp")` — and in nothing else, which is what the golden
//! cases compare.
//!
//! [ADR-286]: ../../../docs/decisions/ADR-276-300.md#adr-286-taskboundary--six-canonical-commands-own-task-state-and-every-host-is-a-thin-wrapper-around-them

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use koru_core::EntityRef;
use koru_core::attributes::{Priority, TaskReason};
use koru_core::node::Kind;
use koru_core::schema::{Result, WorkspaceDoc};
use koru_core::tasks::{
    ActivityIntent, AttrChange, CausationId, ChildClass, LifecycleReason, TaskOutcome,
};
use koru_core::time::TemporalKind;
use koru_events::{Actor, Draft, EventId};
pub use koru_workspace::tasks::Landed;

#[cfg(test)]
mod tests;

// ------------------------------------------------------------------ counters

/// What the wrapper has done since the process started.
///
/// **Coalesced counts, never a log line per event** (SPEC §9): a failed or
/// dropped activity attempt keeps the task change and increments a number.
/// There is deliberately no environment read here — the debug row belongs to
/// whoever owns `KORU_TASKS_DEBUG`, and this is the plain function it gates.
#[derive(Copy, Clone, PartialEq, Eq, Default, Debug)]
pub struct Counters {
    /// Commands run through [`TaskHost::run`].
    pub commands: u64,
    /// Intents this wrapper turned into a draft and handed to the sink.
    pub attempted: u64,
    /// Intents it could not map to a registered payload. Never retried and
    /// never fabricated later — the task change stands either way.
    pub dropped: u64,
}

impl Counters {
    /// One line naming the counts. Tokens and numbers; **no content**, for the
    /// same reason the intents carry none.
    pub fn describe(&self) -> String {
        format!(
            "tasks commands={} activity_attempted={} activity_dropped={}",
            self.commands, self.attempted, self.dropped
        )
    }
}

static COMMANDS: AtomicU64 = AtomicU64::new(0);
static ATTEMPTED: AtomicU64 = AtomicU64::new(0);
static DROPPED: AtomicU64 = AtomicU64::new(0);

/// The wrapper's counters, for a diagnostic that wants them.
pub fn counters() -> Counters {
    Counters {
        commands: COMMANDS.load(Ordering::Relaxed),
        attempted: ATTEMPTED.load(Ordering::Relaxed),
        dropped: DROPPED.load(Ordering::Relaxed),
    }
}

// ------------------------------------------------------------------ the host

/// The desktop wrapper.
///
/// Zero-sized in production, so a caller may keep one on its `App` or build one
/// where it stands. Its only state is the sink, which exists so a test can
/// watch what left without a store, a gateway or a thread.
pub struct TaskHost {
    sink: Box<dyn FnMut(Draft) + Send>,
}

impl Default for TaskHost {
    fn default() -> Self {
        Self::new()
    }
}

impl TaskHost {
    /// The shipping wrapper: every draft goes to [`crate::events_host::emit`].
    pub fn new() -> Self {
        Self {
            sink: Box::new(crate::events_host::emit),
        }
    }

    /// **Run one canonical command.**
    ///
    /// The outcome comes back synchronously and is the caller's answer; the
    /// activity attempt follows the edit, in outcome order, and is never waited
    /// on. A command that refuses leaves nothing behind — no counter moves and
    /// no draft is built, because there is no outcome to name one.
    pub fn run(
        &mut self,
        ws: &mut WorkspaceDoc,
        command: impl FnOnce(&mut WorkspaceDoc) -> Result<TaskOutcome>,
    ) -> Result<TaskOutcome> {
        let outcome = command(ws)?;
        COMMANDS.fetch_add(1, Ordering::Relaxed);
        self.attempt(outcome.causation, &outcome.activity);
        Ok(outcome)
    }

    /// The same attempt, for an outcome this host did not run itself.
    ///
    /// A checkbox click, a `/due` and a chip's `×` all happen *inside a view*,
    /// which holds the document for the length of the call and cannot name this
    /// crate — so the view runs the command, keeps the causation id and the
    /// intents, and the host drains them after dispatch and hands them here.
    /// The scheduling and the attribution are still the wrapper's, which is the
    /// only thing the contract asks.
    pub fn attempt(&mut self, causation: CausationId, intents: &[ActivityIntent]) {
        for intent in intents {
            match draft_for(causation, intent) {
                Some(draft) => {
                    ATTEMPTED.fetch_add(1, Ordering::Relaxed);
                    (self.sink)(draft);
                }
                None => {
                    DROPPED.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }

    /// **Emit the coalesced `task.content.changed` for a flushed burst.**
    ///
    /// The list comes from [`ContentBursts`], which is the only thing allowed
    /// to decide *when*; this function decides nothing at all beyond the
    /// attribution. It is a separate entry point from [`attempt`](Self::attempt)
    /// because this event belongs to no command: there is no
    /// [`ActivityIntent`] for it, no [`TaskOutcome`] to take a causation id
    /// from, and therefore neither `in_chain` nor `caused_by` is called on the
    /// draft — **it is its own chain root** (ADR-273).
    ///
    /// A reference that is not the `task:` spelling is counted and dropped, for
    /// the same reason [`draft_for`] drops one: publishing a subject nobody can
    /// query is worse than publishing nothing.
    pub fn content_changed(&mut self, tasks: &[EntityRef]) {
        for task in tasks {
            match task_subject(task) {
                Some(task) => {
                    ATTEMPTED.fetch_add(1, Ordering::Relaxed);
                    // No attrs by construction — the text is the one thing this
                    // event must never carry, and the payload has nowhere to
                    // put it.
                    (self.sink)(
                        Draft::from(koru_events::TaskContentChanged { task }).actor(Actor::user()),
                    );
                }
                None => {
                    DROPPED.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }

    /// A wrapper whose attempts are collected rather than emitted — what the
    /// tests below watch, and the only other sink there is.
    #[cfg(test)]
    fn recording(into: std::sync::Arc<std::sync::Mutex<Vec<Draft>>>) -> Self {
        Self {
            sink: Box::new(move |draft| into.lock().unwrap().push(draft)),
        }
    }
}

// -------------------------------------------------------------- the burst set
//
// ADR-273 gives `task.content.changed` to *this* host and to nothing below it:
// the domain produces no such intent, because the domain does not know what a
// burst is.

/// How long a task's text must sit untouched before the host's existing
/// maintenance tick calls its burst over.
///
/// Five seconds, from ADR-273. It is a *quiet* period rather than a period of
/// its own — see [`ContentBursts::quiet`] for why that distinction is the whole
/// of the "no timer" rule.
pub const BURST_QUIET: Duration = Duration::from_secs(5);

/// How many bursts may be open before the oldest is let go.
///
/// A host that never flushes must not cost anything worse than the feed it is
/// not draining, which is the rule `koru_editor::tasks::SINK_LIMIT` already
/// follows one level down. The number is generous because a key is a *task*,
/// not a keystroke: reaching it means five hundred distinct tasks were typed
/// into without one boundary passing.
pub const OPEN_LIMIT: usize = 512;

/// **The open editing bursts, keyed by task.**
///
/// The whole of ADR-273's coalescing, and the four constraints it names are
/// each structural here rather than reviewed:
///
/// - **Never per keystroke.** [`note`](Self::note) writes a map entry and emits
///   nothing. It cannot emit: it is handed no sink.
/// - **No timer of its own.** Nothing in this type reads a clock. Every method
///   that needs the time is *given* it, so this rides the host's existing
///   throttled maintenance tick and can never put a second clock on the idle
///   path.
/// - **No document walk.** The key *is* the task; nothing here has a
///   `WorkspaceDoc` to scan even if it wanted one.
/// - **Never the text.** An [`EntityRef`] is three ids.
///
/// A burst on task *T* ends at the first of — the caret leaving *T*'s row
/// ([`caret_left`](Self::caret_left)), the maintenance tick seeing
/// [`BURST_QUIET`] with no keystroke on *T* ([`quiet`](Self::quiet)), or *T*'s
/// tab closing ([`closed`](Self::closed)). Each answers the tasks whose burst
/// just ended; the caller hands them to [`TaskHost::content_changed`].
///
/// **Every flush is a drain**, which is what makes "one event per task per
/// burst" true rather than approximately true: a task leaves the map when its
/// burst ends, so the second boundary to arrive for one burst finds nothing.
#[derive(Default, Debug)]
pub struct ContentBursts {
    /// Task → when the last keystroke landed on it. The instant is what
    /// [`quiet`](Self::quiet) compares; it is never used to schedule anything.
    open: HashMap<EntityRef, Instant>,
}

impl ContentBursts {
    pub fn new() -> Self {
        Self::default()
    }

    /// **One keystroke landed on a task's own text.** Emits nothing.
    ///
    /// Re-noting an open burst refreshes its quiet deadline and adds no second
    /// event — the map is keyed by task, so coalescing is the data structure
    /// rather than a rule somebody applies.
    pub fn note(&mut self, task: EntityRef, now: Instant) {
        if self.open.len() >= OPEN_LIMIT && !self.open.contains_key(&task) {
            self.drop_oldest();
        }
        self.open.insert(task, now);
    }

    /// **The caret left this task's row** — a click, an arrow key, Enter, a tab
    /// switch, a window blur. The burst ends now, whatever the clock says.
    pub fn caret_left(&mut self, task: &EntityRef) -> Vec<EntityRef> {
        match self.open.remove(task) {
            Some(_) => vec![*task],
            None => Vec::new(),
        }
    }

    /// **The maintenance tick.** Every burst with no keystroke for
    /// [`BURST_QUIET`] ends.
    ///
    /// `now` is a parameter and not a `Instant::now()` on purpose: this is the
    /// difference between riding the host's existing tick and owning a clock,
    /// and it is also what lets the tests below step time without sleeping.
    pub fn quiet(&mut self, now: Instant) -> Vec<EntityRef> {
        let done: Vec<EntityRef> = self
            .open
            .iter()
            .filter(|(_, last)| now.duration_since(**last) >= BURST_QUIET)
            .map(|(task, _)| *task)
            .collect();
        for task in &done {
            self.open.remove(task);
        }
        ordered(done)
    }

    /// **A tab closed**, and these are the tasks it held. Every burst open on
    /// one of them ends: there is no row left for a caret to leave and no tick
    /// will ever see it again.
    ///
    /// It takes the tasks rather than a document id. That used to be forced:
    /// a reference's document segment was derived per node, so two rows of one
    /// page carried two different segments and filtering on one would have
    /// closed the burst on the title row while leaving the paragraph under it
    /// open forever (TECHDEBT 72). The segment is the containing document now —
    /// `koru_core::tasks::refs::document_segment` — so a document-keyed close
    /// would be correct, and this stays a list anyway: the closing tab knows
    /// exactly what it held, which is cheaper than a scan and does not assume a
    /// tab is the whole of a document.
    ///
    /// A task in the list with no burst open is not an error and emits nothing —
    /// closing a tab nobody typed in owes no event.
    pub fn closed(&mut self, tasks: &[EntityRef]) -> Vec<EntityRef> {
        let done: Vec<EntityRef> = tasks
            .iter()
            .filter(|task| self.open.remove(task).is_some())
            .copied()
            .collect();
        ordered(done)
    }

    /// Every open burst, ended. The window is going away.
    pub fn drain(&mut self) -> Vec<EntityRef> {
        ordered(self.open.drain().map(|(task, _)| task).collect())
    }

    pub fn is_empty(&self) -> bool {
        self.open.is_empty()
    }

    pub fn len(&self) -> usize {
        self.open.len()
    }

    /// The bound. Activity is best effort and never retried or fabricated later
    /// (SPEC §9), so the burst that has waited longest is the right one to lose.
    fn drop_oldest(&mut self) {
        let oldest = self
            .open
            .iter()
            .min_by_key(|(task, last)| (**last, task.to_storage()))
            .map(|(task, _)| *task);
        if let Some(task) = oldest {
            self.open.remove(&task);
            DROPPED.fetch_add(1, Ordering::Relaxed);
        }
    }
}

// ------------------------------------------------------- the one open set
//
// T-0b's one addition to this file, and it is a *location* rather than a
// mechanism: everything below delegates to the type above and decides nothing.
//
// The burst map has to be reachable from two places that cannot see each
// other's state — the container's post-dispatch drain, which lives up in
// `apps/koru` and is handed a view rather than an `App`, and the maintenance
// tick in `crate::app`, which is handed an `App` and never a view. A field on
// either one is invisible to the other, and threading it through the `View`
// trait would put a task concept in the shell's vocabulary.
//
// One map per *process* is also the coalescing domain the event wants: the key
// is a task, two windows can be open on one document, and a burst is a thing a
// person is doing rather than a thing a window is doing.

/// The open bursts, for the whole process.
static BURSTS: std::sync::Mutex<Option<ContentBursts>> = std::sync::Mutex::new(None);

fn with_bursts<T>(f: impl FnOnce(&mut ContentBursts) -> T) -> T {
    let mut open = BURSTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    f(open.get_or_insert_with(ContentBursts::new))
}

/// **One keystroke landed on a task's own text.** Emits nothing.
pub fn note_content_edit(task: EntityRef, now: Instant) {
    with_bursts(|bursts| bursts.note(task, now));
}

/// **The caret left this task's row.** The tasks whose burst just ended.
pub fn content_caret_left(task: &EntityRef) -> Vec<EntityRef> {
    with_bursts(|bursts| bursts.caret_left(task))
}

/// **The maintenance tick**, given the time it is riding on rather than reading
/// one — see [`ContentBursts::quiet`], which is where that rule lives.
pub fn content_quiet(now: Instant) -> Vec<EntityRef> {
    with_bursts(|bursts| bursts.quiet(now))
}

/// **A tab closed**, and these are the tasks it held.
pub fn content_closed(tasks: &[EntityRef]) -> Vec<EntityRef> {
    with_bursts(|bursts| bursts.closed(tasks))
}

/// Whether any burst is open at all — the guard in front of the work a
/// boundary would otherwise do on every event and every tick.
pub fn content_bursts_open() -> bool {
    with_bursts(|bursts| !bursts.is_empty())
}

/// A flush in a stable order.
///
/// `HashMap` iteration is arbitrary, and two events published in an arbitrary
/// order are two events a reader cannot compare runs of. The storage spelling
/// is already the canonical total order on a reference.
fn ordered(mut tasks: Vec<EntityRef>) -> Vec<EntityRef> {
    tasks.sort_by_key(|task| task.to_storage());
    tasks
}

// ------------------------------------------------------------- the mapping
//
// One arm per `ActivityIntent`, so the day the domain adds a row this stops
// compiling rather than quietly emitting eleven of twelve. Nothing here decides
// anything: it renames closed tokens into the ones `koru-events` declares.

/// The intent as the draft `koru-events` declares for it, attributed and joined
/// to the command's chain.
///
/// `None` when the intent cannot be named — a subject that is not a task
/// reference, or a temporal shape the event vocabulary has no token for. Both
/// are counted and dropped rather than guessed at: a wrapper may not invent an
/// event the outcome did not name, and that includes inventing a token for it.
fn draft_for(causation: CausationId, intent: &ActivityIntent) -> Option<Draft> {
    let task = subject(intent)?;
    let draft: Draft = match intent {
        ActivityIntent::Created {
            reason, node_kind, ..
        } => koru_events::TaskCreated {
            task,
            reason: created_reason(*reason),
            node_kind: node_kind_of(*node_kind)?,
        }
        .into(),
        ActivityIntent::Removed { .. } => koru_events::TaskRemoved { task }.into(),
        ActivityIntent::Completed { reason, .. } => koru_events::TaskCompleted {
            task,
            reason: completed_reason(*reason)?,
        }
        .into(),
        ActivityIntent::Reopened { reason, .. } => koru_events::TaskReopened {
            task,
            reason: reopened_reason(*reason)?,
        }
        .into(),
        ActivityIntent::DueChanged {
            change,
            temporal_kind,
            ..
        } => koru_events::TaskDueChanged {
            task,
            change: attr_change(*change),
            temporal_kind: temporal(*temporal_kind)?,
        }
        .into(),
        ActivityIntent::PriorityChanged {
            change, priority, ..
        } => koru_events::TaskPriorityChanged {
            task,
            change: attr_change(*change),
            priority: priority.map(priority_token),
        }
        .into(),
        ActivityIntent::ChildAdded {
            child,
            class,
            reopened,
            ..
        } => koru_events::TaskChildAdded {
            task,
            child: child.as_uuid(),
            child_class: child_class(*class),
            reopened: *reopened,
        }
        .into(),
        ActivityIntent::ChildMoved {
            child,
            class,
            reopened,
            ..
        } => koru_events::TaskChildMoved {
            task,
            child: child.as_uuid(),
            child_class: child_class(*class),
            reopened: *reopened,
        }
        .into(),
        ActivityIntent::ChildRemoved { child, .. } => koru_events::TaskChildRemoved {
            task,
            child: child.as_uuid(),
        }
        .into(),
        ActivityIntent::ChildCompleted {
            child,
            child_is_task,
            ..
        } => koru_events::TaskChildCompleted {
            task,
            child: child.as_uuid(),
            child_is_task: *child_is_task,
        }
        .into(),
        ActivityIntent::ChildReopened {
            child,
            child_is_task,
            reopened_parent,
            ..
        } => koru_events::TaskChildReopened {
            task,
            child: child.as_uuid(),
            child_is_task: *child_is_task,
            reopened_parent: *reopened_parent,
        }
        .into(),
    };
    // The attribution and the chain: the whole of what a desktop wrapper adds.
    // `in_chain` rather than `caused_by` because the command minted its
    // causation id before any event existed, and its events are a *set* sharing
    // one chain root rather than a causal chain among themselves.
    Some(draft.actor(Actor::user()).in_chain(EventId(causation.0)))
}

/// The triple behind the `task:<workspace>:<document>:<node>` spelling.
///
/// An intent always files itself under the `Task` variant; anything else is a
/// domain bug rather than something to translate, and answering `None` counts
/// it instead of publishing a subject nobody can query.
fn subject(intent: &ActivityIntent) -> Option<koru_events::TaskSubject> {
    task_subject(intent.subject())
}

/// The same conversion for a reference that came from no intent —
/// [`TaskHost::content_changed`]'s, since `task.content.changed` belongs to no
/// command and therefore to no [`ActivityIntent`].
fn task_subject(reference: &EntityRef) -> Option<koru_events::TaskSubject> {
    if reference.kind() != "task" {
        return None;
    }
    Some(koru_events::TaskSubject {
        workspace: reference.workspace_id().as_uuid(),
        document: reference.document_id().as_uuid(),
        node: reference.node_id().as_uuid(),
    })
}

fn created_reason(reason: TaskReason) -> koru_events::TaskCreatedReason {
    match reason {
        TaskReason::Explicit => koru_events::TaskCreatedReason::Explicit,
        TaskReason::Due => koru_events::TaskCreatedReason::Due,
        TaskReason::TasksView => koru_events::TaskCreatedReason::TasksView,
    }
}

/// `None` for a kind that cannot carry task state — the closed set is
/// `tasks::TASK_CAPABLE_KINDS` and the event vocabulary mirrors it, so the two
/// cannot drift into a token nothing accepts.
fn node_kind_of(kind: Kind) -> Option<koru_events::TaskNodeKind> {
    Some(match kind {
        Kind::Paragraph => koru_events::TaskNodeKind::Paragraph,
        Kind::Task => koru_events::TaskNodeKind::Task,
        Kind::Document => koru_events::TaskNodeKind::Document,
        Kind::Image => koru_events::TaskNodeKind::Image,
        Kind::File => koru_events::TaskNodeKind::File,
        _ => return None,
    })
}

/// The three reasons that complete. A reopen reason here is a domain bug, and
/// `LifecycleReason::completes` is what tells them apart — so the two event
/// vocabularies stay derived from the enum rather than typed twice.
fn completed_reason(reason: LifecycleReason) -> Option<koru_events::TaskCompletedReason> {
    Some(match reason {
        LifecycleReason::ExplicitComplete => koru_events::TaskCompletedReason::ExplicitComplete,
        LifecycleReason::CheckboxCompleted => koru_events::TaskCompletedReason::CheckboxCompleted,
        LifecycleReason::CascadeFromAncestor => {
            koru_events::TaskCompletedReason::CascadeFromAncestor
        }
        _ => return None,
    })
}

fn reopened_reason(reason: LifecycleReason) -> Option<koru_events::TaskReopenedReason> {
    Some(match reason {
        LifecycleReason::ExplicitReopen => koru_events::TaskReopenedReason::ExplicitReopen,
        LifecycleReason::CheckboxReopened => koru_events::TaskReopenedReason::CheckboxReopened,
        LifecycleReason::UnfinishedChildAdded => {
            koru_events::TaskReopenedReason::UnfinishedChildAdded
        }
        _ => return None,
    })
}

fn attr_change(change: AttrChange) -> koru_events::TaskAttributeChange {
    match change {
        AttrChange::Set => koru_events::TaskAttributeChange::Set,
        AttrChange::Changed => koru_events::TaskAttributeChange::Changed,
        AttrChange::Cleared => koru_events::TaskAttributeChange::Cleared,
    }
}

/// The *shape* a deadline now has, never its value. `None` once it has been
/// cleared; a **floating** wall clock has no token here because `koru.due` is
/// shared and refuses one, so an intent carrying it never came from `set_due`.
fn temporal(kind: Option<TemporalKind>) -> Option<koru_events::TaskTemporalKind> {
    Some(match kind {
        None => koru_events::TaskTemporalKind::None,
        Some(TemporalKind::Date) => koru_events::TaskTemporalKind::Date,
        Some(TemporalKind::Zoned) => koru_events::TaskTemporalKind::DatetimeZoned,
        Some(TemporalKind::Floating) => return None,
    })
}

fn priority_token(priority: Priority) -> koru_events::TaskPriority {
    match priority {
        Priority::Low => koru_events::TaskPriority::Low,
        Priority::Medium => koru_events::TaskPriority::Medium,
        Priority::High => koru_events::TaskPriority::High,
        Priority::Urgent => koru_events::TaskPriority::Urgent,
    }
}

fn child_class(class: ChildClass) -> koru_events::TaskChildClass {
    match class {
        ChildClass::UnfinishedStructural => koru_events::TaskChildClass::UnfinishedStructural,
        ChildClass::FinishedStructural => koru_events::TaskChildClass::FinishedStructural,
        ChildClass::Passive => koru_events::TaskChildClass::Passive,
    }
}

// -------------------------------------------------- commands a view asks for

/// **A task change a view asked for but cannot make.**
///
/// A Tasks surface lists tasks from every document in the workspace, so ticking
/// a row is a write into a document nobody has open — which needs a `&mut
/// Workspace` (to park it, join its room and publish), and a view holds one
/// `&mut WorkspaceDoc` at most. This is the same seam
/// [`View::take_wants_create_list`] already uses for the list table, and for
/// the identical reason.
///
/// Ids rather than a resolved node: the view read them off a query answer, and
/// nothing between here and the command needs to open a document to carry them.
///
/// [`View::take_wants_create_list`]: crate::View::take_wants_create_list
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum TaskCommand {
    /// Tick or untick, through the canonical lifecycle commands.
    Complete {
        document: koru_core::DocumentId,
        node: koru_core::NodeId,
        completed: bool,
    },
    /// Put a task on Today by hand, or take it off again.
    ///
    /// **One variant for both directions**, on [`Complete`](Self::Complete)'s
    /// precedent: `koru.today` is one membership with two ends, not two
    /// commands.
    ///
    /// It *is* a canonical task command — `pin_to_today` and
    /// `unpin_from_today`, the seventh and its mirror — because which queue a
    /// task appears on is a classification, and the key is command-only for
    /// that reason.
    PinToday {
        document: koru_core::DocumentId,
        node: koru_core::NodeId,
        pinned: bool,
    },
    /// Clear the marker, the lifecycle and the due date. **The row itself
    /// stays** — this takes a paragraph off the Tasks list and destroys no
    /// content, which is what its confirm says in as many words.
    Remove {
        document: koru_core::DocumentId,
        node: koru_core::NodeId,
    },
    /// SPEC §8's native task: a row in the workspace's own tasks document.
    Create {
        title: String,
        due: Option<koru_core::time::Temporal>,
        list: Option<koru_core::ids::ListId>,
        /// The level the row was captured with — `/priority` on the input.
        priority: Option<Priority>,
        /// Its place in a list page's dragged order (`koru.rank`): the top, for
        /// a task typed into a list ordered by hand. `None` everywhere else.
        rank: Option<i64>,
    },
    /// Put a task in a list, or — with `None` — take it out of whatever list it
    /// is in.
    ///
    /// **One variant for both directions**, on [`Complete`](Self::Complete)'s
    /// precedent: membership is one `koru.list` value with cardinality one, so
    /// setting and clearing are two ends of one dimension rather than two
    /// commands.
    ///
    /// **It is not a canonical task command and mints no activity** (SPEC §9):
    /// `koru.list` takes the ordinary attribute route, which is what
    /// `route_attribute` answers and what
    /// `tags_color_custom_attributes_and_list_membership_publish_nothing`
    /// pins. It travels in this enum anyway because it needs exactly what the
    /// other three need — a `&mut Workspace`, to park the task's own document
    /// and publish it — and a second channel for one attribute would be a
    /// second answer to *how does a Tasks surface write*.
    SetList {
        document: koru_core::DocumentId,
        node: koru_core::NodeId,
        list: Option<koru_core::ids::ListId>,
    },
    /// Set a task's due, or — with `None` — clear it, **from the Tasks page**.
    ///
    /// [`SetList`](Self::SetList)'s shape: one variant for both directions of
    /// one dimension. Unlike a `/due` on a content surface this **refuses a
    /// node that is no longer a task** rather than making it one again:
    /// `set_due` creates tasks by design (a deadline alone makes a task, SPEC
    /// §1), and a page listing tasks must not resurrect one somebody removed
    /// while the picker was open. The guard runs in the task's own document,
    /// before the write, through the resolver the whole-row open uses.
    SetDue {
        document: koru_core::DocumentId,
        node: koru_core::NodeId,
        due: Option<koru_core::time::Temporal>,
    },
    /// Set a task's priority, or clear it, from the Tasks page.
    ///
    /// **Not a canonical command** ([ADR-286]): priority keeps its routed
    /// attribute write, `koru_core::tasks::routed::set_attribute`, which owes
    /// a `PriorityChanged` intent on a task and nothing on a plain node. The
    /// same guard as [`SetDue`](Self::SetDue), and **one undo step**: the
    /// routed write has no boundary of its own, so it runs inside
    /// `tasks::compose`, as the editor's does — a priority set from the page on
    /// a task in a document just typed into takes back nothing else.
    ///
    /// [ADR-286]: ../../../docs/decisions/ADR-276-300.md#adr-286-taskboundary--six-canonical-commands-own-task-state-and-every-host-is-a-thin-wrapper-around-them
    SetPriority {
        document: koru_core::DocumentId,
        node: koru_core::NodeId,
        priority: Option<Priority>,
    },
    /// The task line's colour (`koru.color`) from the page — a chip pressed
    /// on its row. `None` clears it. The routed write, bracketed and guarded
    /// exactly as [`SetPriority`](Self::SetPriority) is: a colour is a value
    /// on the line, as a priority is.
    SetColor {
        document: koru_core::DocumentId,
        node: koru_core::NodeId,
        /// An accent token (`koru_core::attributes::AccentColor`).
        color: Option<String>,
    },
    /// **Where the task sits in its list** (`koru.rank`), from a drag on a
    /// list page. `None` clears it. The colour's road: guarded, routed, one
    /// undo step per task; a drag of several is a run of these.
    SetRank {
        document: koru_core::DocumentId,
        node: koru_core::NodeId,
        rank: Option<i64>,
    },
    /// **The task's words, renamed on its row** — a double click on the
    /// Tasks page. The line's text is set by a minimal diff (`set_text`), so
    /// marks on the words that did not change stay, and a peer editing the
    /// same line elsewhere keeps what it typed. Guarded and routed like the
    /// colour: one undo step, refused for a line that stopped being a task.
    Retitle {
        document: koru_core::DocumentId,
        node: koru_core::NodeId,
        title: String,
    },
    /// **Give a page-made task a document of its own** — *Create document* on
    /// its row. The workspace makes a page named
    /// [`home_title`](koru_workspace::tasks::home_title) at its root with the
    /// task as the first paragraph and takes the original out of the tasks
    /// document; the receipt names the new page and the planted node, which
    /// is what the container opens with the caret on that paragraph.
    CreateHome {
        document: koru_core::DocumentId,
        node: koru_core::NodeId,
    },
}

/// **What a person reads when a Tasks-page gesture names a task that is no
/// longer one.** Written once: the navigation leg's refusal and the two
/// in-place writes' guard say the same sentence, and the domain's own error
/// text never reaches a toast on this path.
pub const TASK_GONE: &str = "This task doesn't exist or was removed";

impl TaskCommand {
    /// A closed token for this command, for a diagnostic.
    ///
    /// **The verb and nothing else.** `KORU_TASKS_DEBUG` is declared as carrying
    /// "closed tokens, counts and a causation id; never a title, a due date or
    /// row text" (`ENV.md`), so a capture's words and a task's deadline cannot
    /// leave through here — only which of the four ran, and for the two that
    /// have a direction, which way.
    pub fn token(&self) -> &'static str {
        match self {
            Self::Complete {
                completed: true, ..
            } => "complete",
            Self::Complete {
                completed: false, ..
            } => "reopen",
            Self::PinToday { pinned: true, .. } => "pin_today",
            Self::PinToday { pinned: false, .. } => "unpin_today",
            Self::Remove { .. } => "remove",
            Self::Create { .. } => "create",
            Self::SetList { list: Some(_), .. } => "set_list",
            Self::SetList { list: None, .. } => "clear_list",
            Self::SetDue { due: Some(_), .. } => "set_due",
            Self::SetDue { due: None, .. } => "clear_due",
            Self::CreateHome { .. } => "create_home",
            Self::SetPriority {
                priority: Some(_), ..
            } => "set_priority",
            Self::SetPriority { priority: None, .. } => "clear_priority",
            Self::SetColor { color: Some(_), .. } => "set_color",
            Self::SetColor { color: None, .. } => "clear_color",
            Self::SetRank { rank: Some(_), .. } => "set_rank",
            Self::SetRank { rank: None, .. } => "clear_rank",
            Self::Retitle { .. } => "retitle",
        }
    }
}

/// Whether `node` still names a task in `doc` — the Tasks page's guard.
///
/// The page's row is up to half a minute old and the picker adds however long
/// it was open, so the question is asked in the task's own document, at the
/// moment of the write, through the resolver the whole-row open already goes
/// through. `task_ref` mints the reference *from the document*; nothing here
/// needs a workspace id it does not have.
fn still_a_task(doc: &WorkspaceDoc, node: koru_core::NodeId) -> bool {
    use koru_core::tasks::refs::{TaskRefResolution, resolve_task_ref, task_ref};
    task_ref(doc, node).is_some_and(|reference| {
        matches!(
            resolve_task_ref(doc, &reference),
            TaskRefResolution::Found(_)
        )
    })
}

/// **A change to the list *registry*** a view asked for and cannot make.
///
/// [`TaskCommand`]'s sibling, one document along: a list lives in the
/// workspace's **system document** (ADR-176), which no view can see, so the
/// rail reports the gesture and the shell writes it — the seam
/// [`View::take_wants_create_list`] already opened for making one.
///
/// It is a separate enum rather than three more `TaskCommand` arms because
/// nothing here touches a task: no canonical command, no `TaskOutcome`, and no
/// document at all except on the delete, where the sweep is the *tidy-up* and
/// not the deletion. A `Vec` for `take_wants_create_list`'s reason turned
/// around — a rename and a recolour can both have happened between two turns.
///
/// [`View::take_wants_create_list`]: crate::View::take_wants_create_list
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ListEdit {
    Rename {
        id: koru_core::ids::ListId,
        name: String,
    },
    /// Set or clear the swatch. `None` is a list wearing no colour, which the
    /// rail draws as the dim default — [`SystemDoc::set_list_color`]'s contract.
    ///
    /// [`SystemDoc::set_list_color`]: koru_core::system::SystemDoc::set_list_color
    Recolor {
        id: koru_core::ids::ListId,
        color: Option<String>,
    },
    /// Tombstone the registry record and clear the memberships. **No task is
    /// deleted** (SPEC §8) — which is what the confirmation that raises this
    /// says in as many words, and what `serve_list_edit` keeps true.
    Delete(koru_core::ids::ListId),
    /// The Lists section, in the order a drag left it. **Every list, ranked
    /// densely from zero**, rather than one row's new number: a rank is only
    /// meaningful against its neighbours', and writing one of them leaves the
    /// others to be guessed at — including the unranked ones a peer made,
    /// which sort last until somebody orders them.
    Reorder(Vec<koru_core::ids::ListId>),
    /// Show or hide a page's note inline — the page's `+` menu's *Inline
    /// description*. On the page's record, so every device agrees.
    NoteShown {
        page: koru_core::ids::PageKey,
        shown: bool,
    },
    /// Attach workspace files to a page as context, in the order chosen.
    Attach {
        page: koru_core::ids::PageKey,
        files: Vec<koru_core::DocumentId>,
    },
    /// Take a file off a page. The reference goes; the file stays.
    Detach {
        page: koru_core::ids::PageKey,
        file: koru_core::DocumentId,
    },
}

/// Run one [`ListEdit`]. Answers whether the open document was written to, so
/// the caller knows whether it owes an adopt.
///
/// **The sweep is here and nowhere else.** Deleting a list tombstones a record
/// in one document and leaves a dead `koru.list` value on tasks in every other
/// one; `WorkspaceDoc::clear_list_membership` takes those values off, and it can
/// only reach the document this window has open. That is the whole of what is
/// wired, deliberately: D-4's note says a document swept later is not a
/// different answer, only a later one, and until a lazy sweep on open exists
/// the stale value resolves to no list rather than to the wrong one.
pub fn serve_list_edit(workspace: &mut koru_workspace::Workspace, edit: &ListEdit) -> bool {
    match edit {
        ListEdit::Rename { id, name } => {
            workspace.rename_list(*id, name);
            false
        }
        ListEdit::Recolor { id, color } => {
            workspace.set_list_color(*id, color.as_deref());
            false
        }
        ListEdit::Reorder(order) => {
            for (rank, id) in order.iter().enumerate() {
                workspace.set_list_order(*id, Some(rank as i64));
            }
            false
        }
        ListEdit::Delete(id) => {
            // The registry first: the row leaves the rail on every peer whether
            // or not this window happens to hold a task that was in it.
            if !workspace.delete_list(*id) {
                return false;
            }
            let cleared = workspace.ws.clear_list_membership(*id).unwrap_or(0);
            if cleared > 0 {
                workspace.touched();
            }
            cleared > 0
        }
        ListEdit::NoteShown { page, shown } => {
            workspace.set_page_note_shown(*page, *shown);
            false
        }
        ListEdit::Attach { page, files } => {
            workspace.attach_files_to_page(*page, files);
            false
        }
        ListEdit::Detach { page, file } => {
            workspace.detach_file_from_page(*page, *file);
            false
        }
    }
}

/// Run one [`TaskCommand`] and attempt whatever activity it minted.
///
/// **The whole of the host's half**, in one function, so `App`'s per-turn sweep
/// is four lines rather than a second copy of ADR-273's ordering. `Create` runs
/// two commands and therefore attempts two batches, in outcome order and never
/// merged — each carries its own causation id, and inventing a third would make
/// the desktop's grouping differ from MCP's for the same pair.
///
/// The error is a sentence for a person: a capture that failed has taken
/// somebody's typing with it, so the caller is given something to say and
/// something to put back.
pub fn serve(
    workspace: &mut koru_workspace::Workspace,
    command: &TaskCommand,
) -> std::result::Result<Landed, String> {
    serve_with(workspace, command, &mut TaskHost::default())
}

/// [`serve`], attempting through the host it is handed — the shipping one
/// from `serve`, a recording one from a test that asserts on what a verb
/// owed rather than on a process-global counter other tests are moving.
pub fn serve_with(
    workspace: &mut koru_workspace::Workspace,
    command: &TaskCommand,
    host: &mut TaskHost,
) -> std::result::Result<Landed, String> {
    let mut owed = |outcome: &TaskOutcome| host.attempt(outcome.causation, &outcome.activity);
    // **The receipt is the return value**, and every arm produces one: the
    // write it made, at the version it landed, so the surface that asked can
    // name it to the read model and wait for it — bounded — rather than draw
    // the first answer back. Failure produces none; the caller already tells
    // the two apart.
    let landed = match command {
        TaskCommand::Complete {
            document,
            node,
            completed,
        } => {
            let (document, node, completed) = (*document, *node, *completed);
            let (outcome, landed) = workspace
                .run_task_command(document, |doc| {
                    if completed {
                        koru_core::tasks::complete_task(
                            doc,
                            node,
                            koru_core::tasks::LifecycleReason::ExplicitComplete,
                            koru_core::time::DateOnly::new(chrono::Local::now().date_naive()),
                        )
                    } else {
                        koru_core::tasks::reopen_task(
                            doc,
                            node,
                            koru_core::tasks::LifecycleReason::ExplicitReopen,
                        )
                    }
                })
                .map_err(|e| e.to_string())?;
            owed(&outcome);
            landed
        }
        TaskCommand::PinToday {
            document,
            node,
            pinned,
        } => {
            let (document, node, pinned) = (*document, *node, *pinned);
            let (outcome, landed) = workspace
                .run_task_command(document, |doc| {
                    if pinned {
                        koru_core::tasks::pin_to_today(doc, node)
                    } else {
                        koru_core::tasks::unpin_from_today(doc, node)
                    }
                })
                .map_err(|e| e.to_string())?;
            owed(&outcome);
            landed
        }
        TaskCommand::Remove { document, node } => {
            let (document, node) = (*document, *node);
            let (outcome, landed) = workspace
                .run_task_command(document, |doc| {
                    koru_core::tasks::remove_from_tasks(doc, node)
                })
                .map_err(|e| e.to_string())?;
            owed(&outcome);
            landed
        }
        // Nothing is owed: the task is the same task with a home, and the
        // page's own writes happen through the document doors that already
        // report. The receipt is the new document's.
        TaskCommand::CreateHome { document, node } => {
            workspace
                .home_native_task(
                    *document,
                    *node,
                    koru_core::time::DateOnly::new(chrono::Local::now().date_naive()),
                )
                .map_err(|e| e.to_string())?
                .landed
        }
        TaskCommand::Create {
            title,
            due,
            list,
            priority,
            rank,
        } => {
            let created = workspace
                .create_native_task_ranked(title, due.clone(), *list, *priority, *rank)
                .map_err(|e| e.to_string())?;
            owed(&created.created);
            if let Some(dated) = &created.dated {
                owed(dated);
            }
            created.landed
        }
        // **Nothing is owed.** A list change is an ordinary attribute write —
        // no canonical command, no `TaskOutcome`, and SPEC §9 registers no
        // event for it — so this arm runs the document door and stops. The
        // `set_list`/`clear_list` pair is the door rather than a bare
        // `set_attribute`: it validates the value is a `ListId` and it is the
        // one place membership is written. It is deliberately **not**
        // `set_attribute_on_all`, which is the *in-group* form D-4 needed
        // inside a `compose` bracket; this gesture is its own undo step, which
        // is what the checkpointing door gives it.
        TaskCommand::SetList {
            document,
            node,
            list,
        } => {
            let (document, node, list) = (*document, *node, *list);
            let ((), landed) = workspace
                .run_task_command(document, |doc| match list {
                    Some(list) => doc.set_list(node, list).map(|_| ()),
                    None => doc.clear_list(node).map(|_| ()),
                })
                .map_err(|e| e.to_string())?;
            landed
        }
        // **Both in-place writes resolve the task first**, inside the closure
        // and before the write, and a node that has stopped being a task
        // answers `None` — a refusal spelled as the one sentence, never as
        // the domain's error text. `set_due` would have recreated the task;
        // that is right on a content surface and wrong on a page of tasks.
        TaskCommand::SetDue {
            document,
            node,
            due,
        } => {
            let (document, node, due) = (*document, *node, due.clone());
            let (outcome, landed) = workspace
                .run_task_command(document, |doc| {
                    if !still_a_task(doc, node) {
                        return Ok(None);
                    }
                    match due {
                        Some(due) => koru_core::tasks::set_due(doc, node, due).map(Some),
                        None => koru_core::tasks::clear_due(doc, node).map(Some),
                    }
                })
                .map_err(|e| e.to_string())?;
            let Some(outcome) = outcome else {
                return Err(TASK_GONE.to_owned());
            };
            owed(&outcome);
            landed
        }
        // The routed write, bracketed as the editor brackets it: `compose`
        // closes whatever typing group is open so the priority is its own
        // undo step. What it owes comes back through the generic road — the
        // closure returns the sink — and is attempted here; no second sink.
        TaskCommand::SetPriority {
            document,
            node,
            priority,
        } => {
            let (document, node, priority) = (*document, *node, *priority);
            let (sink, landed) = workspace
                .run_task_command(document, |doc| {
                    if !still_a_task(doc, node) {
                        return Ok(None);
                    }
                    let mut sink = koru_core::tasks::routed::ActivitySink::new();
                    koru_core::tasks::compose(doc, |doc| match priority {
                        Some(level) => koru_core::tasks::routed::set_attribute(
                            doc,
                            &[node],
                            koru_core::attributes::KEY_PRIORITY,
                            koru_core::node::AttrValue::Text(level.as_str().to_owned()),
                            &mut sink,
                        ),
                        None => koru_core::tasks::routed::clear_attribute(
                            doc,
                            &[node],
                            koru_core::attributes::KEY_PRIORITY,
                            &mut sink,
                        ),
                    })?;
                    Ok(Some(sink))
                })
                .map_err(|e| e.to_string())?;
            let Some(sink) = sink else {
                return Err(TASK_GONE.to_owned());
            };
            for activity in sink {
                host.attempt(activity.causation, &activity.intents);
            }
            landed
        }
        // The rank, by the colour's road.
        TaskCommand::SetRank {
            document,
            node,
            rank,
        } => {
            let (document, node, rank) = (*document, *node, *rank);
            let (sink, landed) = workspace
                .run_task_command(document, |doc| {
                    if !still_a_task(doc, node) {
                        return Ok(None);
                    }
                    let mut sink = koru_core::tasks::routed::ActivitySink::new();
                    koru_core::tasks::compose(doc, |doc| match rank {
                        Some(rank) => koru_core::tasks::routed::set_attribute(
                            doc,
                            &[node],
                            koru_core::attributes::KEY_RANK,
                            koru_core::node::AttrValue::Number(rank as f64),
                            &mut sink,
                        ),
                        None => koru_core::tasks::routed::clear_attribute(
                            doc,
                            &[node],
                            koru_core::attributes::KEY_RANK,
                            &mut sink,
                        ),
                    })?;
                    Ok(Some(sink))
                })
                .map_err(|e| e.to_string())?;
            let Some(sink) = sink else {
                return Err(TASK_GONE.to_owned());
            };
            for activity in sink {
                host.attempt(activity.causation, &activity.intents);
            }
            landed
        }
        // The words, by the colour's road: guarded, one undo step.
        TaskCommand::Retitle {
            document,
            node,
            title,
        } => {
            let (document, node, title) = (*document, *node, title.clone());
            let (renamed, landed) = workspace
                .run_task_command(document, |doc| {
                    if !still_a_task(doc, node) {
                        return Ok(false);
                    }
                    koru_core::tasks::compose(doc, |doc| doc.set_text(node, &title))?;
                    Ok(true)
                })
                .map_err(|e| e.to_string())?;
            if !renamed {
                return Err(TASK_GONE.to_owned());
            }
            landed
        }
        // The colour, by the priority's road: guarded, routed, one undo step.
        TaskCommand::SetColor {
            document,
            node,
            color,
        } => {
            let (document, node, color) = (*document, *node, color.clone());
            let (sink, landed) = workspace
                .run_task_command(document, |doc| {
                    if !still_a_task(doc, node) {
                        return Ok(None);
                    }
                    let mut sink = koru_core::tasks::routed::ActivitySink::new();
                    koru_core::tasks::compose(doc, |doc| match &color {
                        Some(token) => koru_core::tasks::routed::set_attribute(
                            doc,
                            &[node],
                            koru_core::attributes::KEY_COLOR,
                            koru_core::node::AttrValue::Text(token.clone()),
                            &mut sink,
                        ),
                        None => koru_core::tasks::routed::clear_attribute(
                            doc,
                            &[node],
                            koru_core::attributes::KEY_COLOR,
                            &mut sink,
                        ),
                    })?;
                    Ok(Some(sink))
                })
                .map_err(|e| e.to_string())?;
            let Some(sink) = sink else {
                return Err(TASK_GONE.to_owned());
            };
            for activity in sink {
                host.attempt(activity.causation, &activity.intents);
            }
            landed
        }
    };
    workspace.touched();
    Ok(landed)
}
