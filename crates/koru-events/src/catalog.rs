// SPDX-License-Identifier: MPL-2.0

//! Building an event: the [`Draft`], the emitting process's [`EmitContext`],
//! and one typed payload per first-party event.
//!
//! The typed payloads are how "unregistered publish" becomes a compile error
//! for our own code: each implements the sealed [`EventPayload`] beside its
//! registry row, so a payload cannot exist without a spec. The [`Draft`]
//! builder is the escape hatch for names known only at runtime — those are
//! refused by [`crate::validate`] at the boundary instead.

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use uuid::Uuid;

use crate::envelope::{
    Actor, AttrValue, DeviceId, Envelope, EventId, EventName, Origin, ProfileId, Scope, SessionId,
    Subject, WorkspaceId,
};
use crate::registry;

/// What the emitting process knows once and every event carries: minted at
/// client construction, never per event.
#[derive(Clone, Debug)]
pub struct EmitContext {
    pub session_id: SessionId,
    pub device_id: Option<DeviceId>,
    pub profile_id: Option<ProfileId>,
    /// The actor an event carries when the draft names none.
    pub actor: Actor,
}

impl EmitContext {
    /// A fresh per-run context for a window acting on a person's behalf.
    pub fn user_session() -> Self {
        Self {
            session_id: SessionId::mint(),
            device_id: None,
            profile_id: None,
            actor: Actor::user(),
        }
    }

    /// A context for a system component (the store's feed, an importer host).
    pub fn system(component: &str) -> Self {
        Self {
            session_id: SessionId::mint(),
            device_id: None,
            profile_id: None,
            actor: Actor::system(component),
        }
    }
}

/// An unsent event under construction. It has no id and no position yet;
/// [`Draft::into_envelope`] mints the id, which is why a *retry* must re-send
/// the same envelope rather than rebuild the draft.
#[derive(Clone, Debug)]
pub struct Draft {
    name: EventName,
    version: u16,
    actor: Option<Actor>,
    workspace_id: Option<WorkspaceId>,
    profile_id: Option<ProfileId>,
    subject: Option<Subject>,
    correlation_id: Option<EventId>,
    causation_id: Option<EventId>,
    attrs: BTreeMap<String, AttrValue>,
}

impl Draft {
    /// The version defaults from the registry row; an unregistered name gets
    /// version 1 and is refused at the boundary.
    pub fn new(name: EventName) -> Self {
        let version = registry::spec(name.as_str())
            .map(|spec| spec.version)
            .unwrap_or(1);
        Self {
            name,
            version,
            actor: None,
            workspace_id: None,
            profile_id: None,
            subject: None,
            correlation_id: None,
            causation_id: None,
            attrs: BTreeMap::new(),
        }
    }

    pub fn attr(mut self, key: &str, value: AttrValue) -> Self {
        self.attrs.insert(key.to_string(), value);
        self
    }

    pub fn subject(mut self, subject: Subject) -> Self {
        self.subject = Some(subject);
        self
    }

    pub fn actor(mut self, actor: Actor) -> Self {
        self.actor = Some(actor);
        self
    }

    pub fn workspace(mut self, workspace: WorkspaceId) -> Self {
        self.workspace_id = Some(workspace);
        self
    }

    pub fn profile(mut self, profile: ProfileId) -> Self {
        self.profile_id = Some(profile);
        self
    }

    /// The correlation rule, applied rather than remembered: a reaction to
    /// `cause` carries `causation_id = cause.event_id` and joins the cause's
    /// chain (`cause.chain_root()`).
    pub fn caused_by(mut self, cause: &Envelope) -> Self {
        self.causation_id = Some(cause.event_id);
        self.correlation_id = Some(cause.chain_root());
        self
    }

    /// Join a chain whose root is not an envelope this process holds — a
    /// canonical task command mints its causation id before any event exists,
    /// so there is nothing to hand [`Draft::caused_by`].
    ///
    /// **It sets the correlation and deliberately no `causation_id`.** One
    /// command's events are a *set* sharing one chain root, not a causal chain
    /// among themselves: none of them caused another, the command caused all
    /// of them, and `causation_id` keeps its documented meaning — a reaction
    /// to a specific observed event ([`Envelope::chain_root`] therefore
    /// answers the same id for every one, and grouping a cascade is one
    /// `WHERE` on the correlation).
    pub fn in_chain(mut self, root: EventId) -> Self {
        self.correlation_id = Some(root);
        self
    }

    /// Deterministic assembly — the id and the clock are parameters, which is
    /// what makes the serialization vectors and every test reproducible.
    pub fn build(self, ctx: &EmitContext, event_id: EventId, occurred_at_ms: i64) -> Envelope {
        Envelope {
            event_id,
            name: self.name,
            version: self.version,
            occurred_at_ms,
            actor: self.actor.unwrap_or_else(|| ctx.actor.clone()),
            origin: Origin {
                device_id: ctx.device_id,
                session_id: ctx.session_id,
            },
            scope: Scope {
                profile_id: self.profile_id.or(ctx.profile_id),
                workspace_id: self.workspace_id,
            },
            subject: self.subject,
            correlation_id: self.correlation_id,
            causation_id: self.causation_id,
            attrs: self.attrs,
        }
    }

    /// Mint the id and stamp the wall clock — the shipping path.
    pub fn into_envelope(self, ctx: &EmitContext) -> Envelope {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        self.build(ctx, EventId::mint(), now)
    }
}

mod sealed {
    pub trait Sealed {}
}

/// A first-party event, typed. Implemented only in this module, beside the
/// registry rows the types mirror — which is what ties every typed publish to
/// a declared spec at compile time.
pub trait EventPayload: sealed::Sealed {
    const NAME: &'static str;
    fn draft(self) -> Draft;
}

impl<P: EventPayload> From<P> for Draft {
    fn from(payload: P) -> Draft {
        payload.draft()
    }
}

macro_rules! payload {
    ($type:ty, $name:literal, |$self_:ident| $body:expr) => {
        impl sealed::Sealed for $type {}
        impl EventPayload for $type {
            const NAME: &'static str = $name;
            fn draft($self_) -> Draft {
                let _ = crate::event_name!($name); // ties the payload to a registry row at compile time
                $body
            }
        }
    };
}

/// How a workspace came to be — `workspace.created`'s `source` vocabulary.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OnboardingEntryChoice {
    Offline,
    Google,
    Apple,
}

impl OnboardingEntryChoice {
    pub fn token(self) -> &'static str {
        match self {
            Self::Offline => "offline",
            Self::Google => "google",
            Self::Apple => "apple",
        }
    }
}

pub struct OnboardingEntrySelected {
    pub choice: OnboardingEntryChoice,
}

payload!(
    OnboardingEntrySelected,
    "app.onboarding.entry_selected",
    |self| {
        Draft::new(EventName::from_static(Self::NAME))
            .attr("choice", AttrValue::Token(self.choice.token().to_owned()))
    }
);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ProfilePurpose {
    Work,
    School,
    Personal,
}

impl ProfilePurpose {
    pub fn token(self) -> &'static str {
        match self {
            Self::Work => "work",
            Self::School => "school",
            Self::Personal => "personal",
        }
    }
}

pub struct OnboardingProfilePurposeSelected {
    pub purpose: ProfilePurpose,
}

payload!(
    OnboardingProfilePurposeSelected,
    "app.onboarding.profile_purpose_selected",
    |self| Draft::new(EventName::from_static(Self::NAME))
        .attr("purpose", AttrValue::Token(self.purpose.token().to_owned()),)
);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WorkspaceSource {
    Onboarding,
    Manual,
    Import,
}

impl WorkspaceSource {
    pub fn token(self) -> &'static str {
        match self {
            WorkspaceSource::Onboarding => "onboarding",
            WorkspaceSource::Manual => "manual",
            WorkspaceSource::Import => "import",
        }
    }
}

pub struct WorkspaceCreated {
    pub workspace_id: Uuid,
    pub source: WorkspaceSource,
}

payload!(WorkspaceCreated, "workspace.created", |self| {
    Draft::new(EventName::from_static(Self::NAME))
        .workspace(WorkspaceId(self.workspace_id))
        .subject(Subject::workspace(self.workspace_id.to_string()))
        .attr("source", AttrValue::Token(self.source.token().to_string()))
});

pub struct WorkspaceDeleted {
    pub workspace_id: Uuid,
}

payload!(WorkspaceDeleted, "workspace.deleted", |self| {
    Draft::new(EventName::from_static(Self::NAME))
        .workspace(WorkspaceId(self.workspace_id))
        .subject(Subject::workspace(self.workspace_id.to_string()))
});

// --------------------------------------------------------------- task activity
//
// ADR-273's twelve, one payload each. They all carry the same subject and the
// same scope, which is why that is a shared type rather than three fields
// repeated twelve times: a row that set only two of the three would be a
// silent hole in the per-subject query the whole feature reads through.
//
// **A payload names no chain.** The correlation is the *wrapper's* — the
// command mints one `CausationId` and the host puts it on every envelope with
// [`Draft::in_chain`] — so these types stay the shape of the fact and nothing
// here can group a cascade differently from the other host.

/// Which task an activity event is about: the triple behind the
/// `task:<workspace>:<document>:<node>` spelling.
///
/// Ids are `Uuid` rather than the domain's newtypes because this crate is a
/// leaf and links no node model; the caller converts once, at the wrapper.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TaskSubject {
    pub workspace: Uuid,
    pub document: Uuid,
    pub node: Uuid,
}

impl TaskSubject {
    pub fn subject(self) -> Subject {
        Subject::task(self.workspace, self.document, self.node)
    }
}

/// The scope-and-subject preamble every task event shares.
fn task_draft(name: &'static str, task: TaskSubject) -> Draft {
    Draft::new(EventName::from_static(name))
        .workspace(WorkspaceId(task.workspace))
        .subject(task.subject())
}

/// Why a task exists — `task.created`'s `reason`, and the value SPEC §3's
/// `koru.task` marker records.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TaskCreatedReason {
    Explicit,
    Due,
    TasksView,
}

impl TaskCreatedReason {
    pub fn token(self) -> &'static str {
        match self {
            TaskCreatedReason::Explicit => "explicit",
            TaskCreatedReason::Due => "due",
            TaskCreatedReason::TasksView => "tasks_view",
        }
    }
}

/// What the node was when it became a task — SPEC §4's closed task-capable
/// set. `Task` is the native task kind, not "it was already a task".
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TaskNodeKind {
    Paragraph,
    Task,
    Document,
    Image,
    File,
}

impl TaskNodeKind {
    pub fn token(self) -> &'static str {
        match self {
            TaskNodeKind::Paragraph => "paragraph",
            TaskNodeKind::Task => "task",
            TaskNodeKind::Document => "document",
            TaskNodeKind::Image => "image",
            TaskNodeKind::File => "file",
        }
    }
}

/// Which lifecycle reason completed a task. The tokens are
/// `koru_core::LifecycleReason`'s, spelled here because this crate may not
/// link the domain — F-3's golden cases are what hold the two spellings
/// together.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TaskCompletedReason {
    ExplicitComplete,
    CheckboxCompleted,
    CascadeFromAncestor,
}

impl TaskCompletedReason {
    pub fn token(self) -> &'static str {
        match self {
            TaskCompletedReason::ExplicitComplete => "explicit_complete",
            TaskCompletedReason::CheckboxCompleted => "checkbox_completed",
            TaskCompletedReason::CascadeFromAncestor => "cascade_from_ancestor",
        }
    }
}

/// Which lifecycle reason reopened a task. `UnfinishedChildAdded` is SPEC
/// §5's auto-reopen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TaskReopenedReason {
    ExplicitReopen,
    CheckboxReopened,
    UnfinishedChildAdded,
}

impl TaskReopenedReason {
    pub fn token(self) -> &'static str {
        match self {
            TaskReopenedReason::ExplicitReopen => "explicit_reopen",
            TaskReopenedReason::CheckboxReopened => "checkbox_reopened",
            TaskReopenedReason::UnfinishedChildAdded => "unfinished_child_added",
        }
    }
}

/// What a Due or Priority change did to the value — never what the value is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TaskAttributeChange {
    Set,
    Changed,
    Cleared,
}

impl TaskAttributeChange {
    pub fn token(self) -> &'static str {
        match self {
            TaskAttributeChange::Set => "set",
            TaskAttributeChange::Changed => "changed",
            TaskAttributeChange::Cleared => "cleared",
        }
    }
}

/// The *shape* a deadline now has. `None` is a cleared deadline; the date
/// itself never appears in an event, in any spelling.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TaskTemporalKind {
    Date,
    DatetimeZoned,
    None,
}

impl TaskTemporalKind {
    pub fn token(self) -> &'static str {
        match self {
            TaskTemporalKind::Date => "date",
            TaskTemporalKind::DatetimeZoned => "datetime_zoned",
            TaskTemporalKind::None => "none",
        }
    }
}

/// SPEC §3's priority ladder. There is no Importance attribute.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TaskPriority {
    Low,
    Medium,
    High,
    Urgent,
}

impl TaskPriority {
    pub fn token(self) -> &'static str {
        match self {
            TaskPriority::Low => "low",
            TaskPriority::Medium => "medium",
            TaskPriority::High => "high",
            TaskPriority::Urgent => "urgent",
        }
    }
}

/// What the classification pass made of a child. Only an unfinished
/// *structural* child can reopen an ancestor.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TaskChildClass {
    UnfinishedStructural,
    FinishedStructural,
    Passive,
}

impl TaskChildClass {
    pub fn token(self) -> &'static str {
        match self {
            TaskChildClass::UnfinishedStructural => "unfinished_structural",
            TaskChildClass::FinishedStructural => "finished_structural",
            TaskChildClass::Passive => "passive",
        }
    }
}

pub struct TaskCreated {
    pub task: TaskSubject,
    pub reason: TaskCreatedReason,
    pub node_kind: TaskNodeKind,
}

payload!(TaskCreated, "task.created", |self| {
    task_draft(Self::NAME, self.task)
        .attr("reason", AttrValue::Token(self.reason.token().to_string()))
        .attr(
            "node_kind",
            AttrValue::Token(self.node_kind.token().to_string()),
        )
});

pub struct TaskRemoved {
    pub task: TaskSubject,
}

payload!(TaskRemoved, "task.removed", |self| {
    task_draft(Self::NAME, self.task)
});

pub struct TaskCompleted {
    pub task: TaskSubject,
    pub reason: TaskCompletedReason,
}

payload!(TaskCompleted, "task.completed", |self| {
    task_draft(Self::NAME, self.task)
        .attr("reason", AttrValue::Token(self.reason.token().to_string()))
});

pub struct TaskReopened {
    pub task: TaskSubject,
    pub reason: TaskReopenedReason,
}

payload!(TaskReopened, "task.reopened", |self| {
    task_draft(Self::NAME, self.task)
        .attr("reason", AttrValue::Token(self.reason.token().to_string()))
});

pub struct TaskDueChanged {
    pub task: TaskSubject,
    pub change: TaskAttributeChange,
    pub temporal_kind: TaskTemporalKind,
}

payload!(TaskDueChanged, "task.due.changed", |self| {
    task_draft(Self::NAME, self.task)
        .attr("change", AttrValue::Token(self.change.token().to_string()))
        .attr(
            "temporal_kind",
            AttrValue::Token(self.temporal_kind.token().to_string()),
        )
});

pub struct TaskPriorityChanged {
    pub task: TaskSubject,
    pub change: TaskAttributeChange,
    /// Absent when the change cleared it — absence means removed, never `Low`.
    pub priority: Option<TaskPriority>,
}

payload!(TaskPriorityChanged, "task.priority.changed", |self| {
    let draft = task_draft(Self::NAME, self.task)
        .attr("change", AttrValue::Token(self.change.token().to_string()));
    match self.priority {
        Some(priority) => draft.attr("priority", AttrValue::Token(priority.token().to_string())),
        None => draft,
    }
});

pub struct TaskChildAdded {
    pub task: TaskSubject,
    pub child: Uuid,
    pub child_class: TaskChildClass,
    pub reopened: bool,
}

payload!(TaskChildAdded, "task.child.added", |self| {
    task_draft(Self::NAME, self.task)
        .attr("child", AttrValue::Id(self.child))
        .attr(
            "child_class",
            AttrValue::Token(self.child_class.token().to_string()),
        )
        .attr("reopened", AttrValue::Flag(self.reopened))
});

pub struct TaskChildMoved {
    pub task: TaskSubject,
    pub child: Uuid,
    pub child_class: TaskChildClass,
    pub reopened: bool,
}

payload!(TaskChildMoved, "task.child.moved", |self| {
    task_draft(Self::NAME, self.task)
        .attr("child", AttrValue::Id(self.child))
        .attr(
            "child_class",
            AttrValue::Token(self.child_class.token().to_string()),
        )
        .attr("reopened", AttrValue::Flag(self.reopened))
});

pub struct TaskChildRemoved {
    pub task: TaskSubject,
    pub child: Uuid,
}

payload!(TaskChildRemoved, "task.child.removed", |self| {
    task_draft(Self::NAME, self.task).attr("child", AttrValue::Id(self.child))
});

pub struct TaskChildCompleted {
    pub task: TaskSubject,
    pub child: Uuid,
    pub child_is_task: bool,
}

payload!(TaskChildCompleted, "task.child.completed", |self| {
    task_draft(Self::NAME, self.task)
        .attr("child", AttrValue::Id(self.child))
        .attr("child_is_task", AttrValue::Flag(self.child_is_task))
});

pub struct TaskChildReopened {
    pub task: TaskSubject,
    pub child: Uuid,
    pub child_is_task: bool,
    pub reopened_parent: bool,
}

payload!(TaskChildReopened, "task.child.reopened", |self| {
    task_draft(Self::NAME, self.task)
        .attr("child", AttrValue::Id(self.child))
        .attr("child_is_task", AttrValue::Flag(self.child_is_task))
        .attr("reopened_parent", AttrValue::Flag(self.reopened_parent))
});

/// The coalesced one. It has no attrs by construction — the text is what it
/// must never carry — and it belongs to no command, so it is its own chain
/// root: neither `in_chain` nor `caused_by` is called on it.
pub struct TaskContentChanged {
    pub task: TaskSubject,
}

payload!(TaskContentChanged, "task.content.changed", |self| {
    task_draft(Self::NAME, self.task)
});
macro_rules! profile_wake {
    ($type:ident, $name:literal) => {
        pub struct $type {
            pub profile_id: Uuid,
        }
        payload!($type, $name, |self| {
            Draft::new(EventName::from_static(Self::NAME)).profile(ProfileId(self.profile_id))
        });
    };
}

profile_wake!(ReminderRuleChanged, "reminder.rule.changed");
profile_wake!(ReminderActionAppended, "reminder.action.appended");
profile_wake!(ReminderHistoryCleared, "reminder.history.cleared");
profile_wake!(ReminderSettingsChanged, "reminder.settings.changed");
profile_wake!(ProfileDictionaryChanged, "profile.dictionary.changed");

pub struct DueChanged {
    pub workspace_id: Uuid,
    pub document_id: Uuid,
    pub entity_id: Uuid,
}

payload!(DueChanged, "due.changed", |self| {
    Draft::new(EventName::from_static(Self::NAME))
        .workspace(WorkspaceId(self.workspace_id))
        .subject(Subject::document(self.document_id.to_string()))
        .attr("entity_id", AttrValue::Id(self.entity_id))
});

pub struct EntityRemoved {
    pub workspace_id: Uuid,
    pub document_id: Uuid,
    pub entity_id: Uuid,
}

payload!(EntityRemoved, "entity.removed", |self| {
    Draft::new(EventName::from_static(Self::NAME))
        .workspace(WorkspaceId(self.workspace_id))
        .subject(Subject::document(self.document_id.to_string()))
        .attr("entity_id", AttrValue::Id(self.entity_id))
});

// ------------------------------------------- the Store's catalog facts
//
// One producer for all of these: `koru-store-api`'s handler, after the
// mutation it reports has committed. See the block comment above the matching
// registry rows for why they sit beside `workspace.created`/`observed`/
// `deleted` rather than replacing any of them.
//
// **A workspace names itself in the envelope** — `scope.workspace_id` plus a
// `Subject::workspace`, exactly as `workspace.created` does. A profile cannot:
// `scope.profile_id` is the *emitting process's* profile, so a profile record's
// id travels as a `LocalReference` attr instead. Neither carries a name, an
// icon, a colour or a path: a surface invalidates and refetches.

pub struct ProfileCreated {
    pub profile_id: Uuid,
}

payload!(ProfileCreated, "profile.created", |self| {
    Draft::new(EventName::from_static(Self::NAME)).attr("profile", AttrValue::Id(self.profile_id))
});

pub struct ProfileRenamed {
    pub profile_id: Uuid,
}

payload!(ProfileRenamed, "profile.renamed", |self| {
    Draft::new(EventName::from_static(Self::NAME)).attr("profile", AttrValue::Id(self.profile_id))
});

pub struct ProfileAppearanceChanged {
    pub profile_id: Uuid,
}

payload!(
    ProfileAppearanceChanged,
    "profile.appearance_changed",
    |self| {
        Draft::new(EventName::from_static(Self::NAME))
            .attr("profile", AttrValue::Id(self.profile_id))
    }
);

/// A reader's private document state changed. The reader travels as an
/// attr, like the profile rows above; the document is the subject.
pub struct DocumentPrivateChanged {
    pub profile_id: Uuid,
    pub workspace_id: Uuid,
    pub document_id: Uuid,
}

payload!(
    DocumentPrivateChanged,
    "document.private.changed",
    |self| {
        Draft::new(EventName::from_static(Self::NAME))
            .workspace(WorkspaceId(self.workspace_id))
            .subject(Subject::document(self.document_id.to_string()))
            .attr("profile", AttrValue::Id(self.profile_id))
    }
);

/// The one shape the workspace-catalog rows share: a name, and the workspace
/// the fact is about. Written once rather than nine times, so a later row
/// cannot forget the subject and quietly publish an event nobody can route.
fn workspace_catalog(name: &'static str, workspace_id: Uuid) -> Draft {
    Draft::new(EventName::from_static(name))
        .workspace(WorkspaceId(workspace_id))
        .subject(Subject::workspace(workspace_id.to_string()))
}

pub struct WorkspaceCatalogCreated {
    pub workspace_id: Uuid,
}

payload!(
    WorkspaceCatalogCreated,
    "workspace.catalog.created",
    |self| workspace_catalog(Self::NAME, self.workspace_id)
);

pub struct WorkspaceCatalogRenamed {
    pub workspace_id: Uuid,
}

payload!(
    WorkspaceCatalogRenamed,
    "workspace.catalog.renamed",
    |self| workspace_catalog(Self::NAME, self.workspace_id)
);

pub struct WorkspaceCatalogAppearanceChanged {
    pub workspace_id: Uuid,
}

payload!(
    WorkspaceCatalogAppearanceChanged,
    "workspace.catalog.appearance_changed",
    |self| workspace_catalog(Self::NAME, self.workspace_id)
);

pub struct WorkspaceCatalogArchived {
    pub workspace_id: Uuid,
}

payload!(
    WorkspaceCatalogArchived,
    "workspace.catalog.archived",
    |self| workspace_catalog(Self::NAME, self.workspace_id)
);

pub struct WorkspaceCatalogUnarchived {
    pub workspace_id: Uuid,
}

payload!(
    WorkspaceCatalogUnarchived,
    "workspace.catalog.unarchived",
    |self| workspace_catalog(Self::NAME, self.workspace_id)
);

pub struct WorkspaceCatalogDeleted {
    pub workspace_id: Uuid,
}

payload!(
    WorkspaceCatalogDeleted,
    "workspace.catalog.deleted",
    |self| workspace_catalog(Self::NAME, self.workspace_id)
);

pub struct WorkspaceCatalogRestored {
    pub workspace_id: Uuid,
}

payload!(
    WorkspaceCatalogRestored,
    "workspace.catalog.restored",
    |self| workspace_catalog(Self::NAME, self.workspace_id)
);

/// **Both ends of the move, because only one of them is in the envelope.**
///
/// The workspace names itself as the subject; neither profile can, for the
/// reason in the block comment above — `scope.profile_id` is the emitting
/// process's profile, not the record that changed. So the owner it left and the
/// owner it arrived at both travel as `LocalReference` attrs, and a consumer
/// repairing state keyed on the old owner can read which key stopped being true
/// without refetching the row it can no longer find under that key.
pub struct WorkspaceCatalogMoved {
    pub workspace_id: Uuid,
    pub from_profile: Uuid,
    pub to_profile: Uuid,
}

payload!(WorkspaceCatalogMoved, "workspace.catalog.moved", |self| {
    workspace_catalog(Self::NAME, self.workspace_id)
        .attr("from_profile", AttrValue::Id(self.from_profile))
        .attr("to_profile", AttrValue::Id(self.to_profile))
});

/// Which of the two import modes ran — `workspace.catalog.imported`'s `mode`
/// vocabulary. There is deliberately no default on the route, so there is no
/// third token here either.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WorkspaceImportMode {
    Restore,
    Duplicate,
}

impl WorkspaceImportMode {
    pub fn token(self) -> &'static str {
        match self {
            WorkspaceImportMode::Restore => "restore",
            WorkspaceImportMode::Duplicate => "duplicate",
        }
    }
}

pub struct WorkspaceCatalogImported {
    pub workspace_id: Uuid,
    pub mode: WorkspaceImportMode,
}

payload!(
    WorkspaceCatalogImported,
    "workspace.catalog.imported",
    |self| {
        workspace_catalog(Self::NAME, self.workspace_id)
            .attr("mode", AttrValue::Token(self.mode.token().to_string()))
    }
);

/// No workspace subject on purpose: every workspace the profile owns may have
/// moved, so naming one of them would be a lie about what changed.
pub struct WorkspaceCatalogReordered {
    pub profile_id: Uuid,
}

payload!(
    WorkspaceCatalogReordered,
    "workspace.catalog.reordered",
    |self| {
        Draft::new(EventName::from_static(Self::NAME))
            .attr("profile", AttrValue::Id(self.profile_id))
    }
);

// ------------------------------------------ the settings authority's facts
//
// One producer: `koru-settings`, through the sink the Gateway hands it, after
// the atomic write has returned. Each carries its section token and nothing
// else — see the registry rows for why a settings fact never carries a value.

/// Which settings section a change fact is about.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SettingsSection {
    Global,
    Ai,
    Terminal,
}

impl SettingsSection {
    pub fn token(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Ai => "ai",
            Self::Terminal => "terminal",
        }
    }

    /// The registry row this section's change fact is published under.
    pub fn event_name(self) -> &'static str {
        match self {
            Self::Global => SettingsGlobalChanged::NAME,
            Self::Ai => SettingsAiChanged::NAME,
            Self::Terminal => SettingsTerminalChanged::NAME,
        }
    }
}

/// The one shape the three settings rows share, written once so a later row
/// cannot forget the token that lets a handler read all three uniformly.
fn settings_changed(name: &'static str, section: SettingsSection) -> Draft {
    Draft::new(EventName::from_static(name))
        .attr("section", AttrValue::Token(section.token().to_owned()))
}

pub struct SettingsGlobalChanged;

payload!(SettingsGlobalChanged, "settings.global_changed", |self| {
    let _ = self;
    settings_changed(Self::NAME, SettingsSection::Global)
});

pub struct SettingsAiChanged;

payload!(SettingsAiChanged, "settings.ai_changed", |self| {
    let _ = self;
    settings_changed(Self::NAME, SettingsSection::Ai)
});

pub struct SettingsTerminalChanged;

payload!(
    SettingsTerminalChanged,
    "settings.terminal_changed",
    |self| {
        let _ = self;
        settings_changed(Self::NAME, SettingsSection::Terminal)
    }
);

/// How the previous run of this app ended — the crash signal, reported on the
/// next start because a crashing process cannot report its own.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PreviousSession {
    Clean,
    Unclean,
    Unknown,
}

impl PreviousSession {
    pub fn token(self) -> &'static str {
        match self {
            PreviousSession::Clean => "clean",
            PreviousSession::Unclean => "unclean",
            PreviousSession::Unknown => "unknown",
        }
    }
}

pub struct AppInstalled {
    /// `build_info().build_number`; 0 for a dev build.
    pub build: u64,
}

payload!(AppInstalled, "app.installed", |self| {
    Draft::new(EventName::from_static(Self::NAME)).attr("build", AttrValue::Count(self.build))
});

pub struct AppUpdateAvailable {
    pub to_build: u64,
}

payload!(AppUpdateAvailable, "app.update.available", |self| {
    Draft::new(EventName::from_static(Self::NAME)).attr("to_build", AttrValue::Count(self.to_build))
});

pub struct AppUpdateDownloaded {
    pub to_build: u64,
}

payload!(AppUpdateDownloaded, "app.update.downloaded", |self| {
    Draft::new(EventName::from_static(Self::NAME)).attr("to_build", AttrValue::Count(self.to_build))
});

pub struct AppUpdateAccepted {
    pub to_build: u64,
}

payload!(AppUpdateAccepted, "app.update.accepted", |self| {
    Draft::new(EventName::from_static(Self::NAME)).attr("to_build", AttrValue::Count(self.to_build))
});

pub struct AppUpdated {
    /// The build now running.
    pub build: u64,
}

payload!(AppUpdated, "app.updated", |self| {
    Draft::new(EventName::from_static(Self::NAME)).attr("build", AttrValue::Count(self.build))
});

/// The machine and its settings, once per run. Every field is `Option`
/// because a platform that did not answer says nothing rather than zero —
/// absence is a value here, and the registry marks these attrs optional.
///
/// Tokens are pre-mapped by the collector to the registry's vocabularies
/// (an unknown language becomes `other` on the machine, never on the wire),
/// which is why they are plain `&'static str`: the collector chooses from a
/// closed set and the boundary refuses anything else.
#[derive(Default)]
pub struct AppEnvironment {
    pub os: &'static str,
    pub os_version: Option<String>,
    pub app_build: u64,
    pub system_language: &'static str,
    pub app_language: &'static str,
    pub utc_offset_minutes: Option<i64>,
    pub display_width: Option<u64>,
    pub display_height: Option<u64>,
    pub display_scale_percent: Option<u64>,
    pub monitor_count: Option<u64>,
    pub window_width: Option<u64>,
    pub window_height: Option<u64>,
    pub ram_gb_bucket: Option<u32>,
    pub disk_free_gb_bucket: Option<u32>,
    pub keyboard_layout: Option<&'static str>,
    pub cpu_arch: &'static str,
    pub cpu_count: Option<u64>,
    pub gpu_vendor: Option<&'static str>,
    pub gpu_backend: Option<&'static str>,
    pub touch: Option<bool>,
    pub theme: Option<&'static str>,
    pub reduced_motion: Option<bool>,
    pub high_contrast: Option<bool>,
    pub text_scale_percent: Option<u64>,
    pub first_day_of_week: Option<&'static str>,
    pub clock: Option<&'static str>,
    pub network: Option<&'static str>,
}

payload!(AppEnvironment, "app.environment", |self| {
    let mut draft = Draft::new(EventName::from_static(Self::NAME))
        .attr("os", AttrValue::Token(self.os.to_string()))
        .attr("app_build", AttrValue::Count(self.app_build))
        .attr(
            "system_language",
            AttrValue::Token(self.system_language.to_string()),
        )
        .attr(
            "app_language",
            AttrValue::Token(self.app_language.to_string()),
        )
        .attr("cpu_arch", AttrValue::Token(self.cpu_arch.to_string()));
    let token = |draft: Draft, key: &str, value: Option<&'static str>| match value {
        Some(value) => draft.attr(key, AttrValue::Token(value.to_string())),
        None => draft,
    };
    draft = token(draft, "keyboard_layout", self.keyboard_layout);
    draft = token(draft, "gpu_vendor", self.gpu_vendor);
    draft = token(draft, "gpu_backend", self.gpu_backend);
    draft = token(draft, "theme", self.theme);
    draft = token(draft, "first_day_of_week", self.first_day_of_week);
    draft = token(draft, "clock", self.clock);
    draft = token(draft, "network", self.network);
    if let Some(version) = self.os_version {
        draft = draft.attr("os_version", AttrValue::Version(version));
    }
    if let Some(offset) = self.utc_offset_minutes {
        draft = draft.attr("utc_offset_minutes", AttrValue::Int(offset));
    }
    for (key, value) in [
        ("display_width", self.display_width),
        ("display_height", self.display_height),
        ("display_scale_percent", self.display_scale_percent),
        ("monitor_count", self.monitor_count),
        ("window_width", self.window_width),
        ("window_height", self.window_height),
        ("cpu_count", self.cpu_count),
        ("text_scale_percent", self.text_scale_percent),
    ] {
        if let Some(value) = value {
            draft = draft.attr(key, AttrValue::Count(value));
        }
    }
    for (key, value) in [
        ("ram_gb_bucket", self.ram_gb_bucket),
        ("disk_free_gb_bucket", self.disk_free_gb_bucket),
    ] {
        if let Some(value) = value {
            draft = draft.attr(key, AttrValue::Bucket(value));
        }
    }
    for (key, value) in [
        ("touch", self.touch),
        ("reduced_motion", self.reduced_motion),
        ("high_contrast", self.high_contrast),
    ] {
        if let Some(value) = value {
            draft = draft.attr(key, AttrValue::Flag(value));
        }
    }
    draft
});

pub struct AppSessionStarted {
    pub previous_session_clean: PreviousSession,
}

payload!(AppSessionStarted, "app.session.started", |self| {
    Draft::new(EventName::from_static(Self::NAME)).attr(
        "previous_session_clean",
        AttrValue::Token(self.previous_session_clean.token().to_string()),
    )
});

pub struct AppSessionEnded {
    /// Already bucketed — see [`crate::bucket`].
    pub duration_bucket: u32,
}

payload!(AppSessionEnded, "app.session.ended", |self| {
    Draft::new(EventName::from_static(Self::NAME))
        .attr("duration_bucket", AttrValue::Bucket(self.duration_bucket))
});

/// Which search surface was opened — `search.opened`'s vocabulary.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SearchSurface {
    /// The agent's global overlay.
    Overlay,
    /// The dedicated search window.
    Window,
    /// The in-app surface inside a Koru window.
    InApp,
}

impl SearchSurface {
    pub fn token(self) -> &'static str {
        match self {
            SearchSurface::Overlay => "overlay",
            SearchSurface::Window => "window",
            SearchSurface::InApp => "in_app",
        }
    }
}

pub struct SearchOpened {
    pub surface: SearchSurface,
}

payload!(SearchOpened, "search.opened", |self| {
    Draft::new(EventName::from_static(Self::NAME)).attr(
        "surface",
        AttrValue::Token(self.surface.token().to_string()),
    )
});

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ImportSource {
    Markdown,
    Textbundle,
    Onenote,
    LegacyWorkspace,
    Files,
    GoogleDocs,
    Notion,
    Evernote,
}

impl ImportSource {
    pub fn token(self) -> &'static str {
        match self {
            ImportSource::Markdown => "markdown",
            ImportSource::Textbundle => "textbundle",
            ImportSource::Onenote => "onenote",
            ImportSource::LegacyWorkspace => "legacy_workspace",
            ImportSource::Files => "files",
            ImportSource::GoogleDocs => "google_docs",
            ImportSource::Notion => "notion",
            ImportSource::Evernote => "evernote",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ImportOutcome {
    Ok,
    Partial,
    Failed,
}

impl ImportOutcome {
    pub fn token(self) -> &'static str {
        match self {
            ImportOutcome::Ok => "ok",
            ImportOutcome::Partial => "partial",
            ImportOutcome::Failed => "failed",
        }
    }
}

pub struct ImportCompleted {
    pub source: ImportSource,
    /// Already bucketed — see [`crate::bucket`].
    pub document_count_bucket: u32,
    pub outcome: ImportOutcome,
}

payload!(ImportCompleted, "import.completed", |self| {
    Draft::new(EventName::from_static(Self::NAME))
        .attr("source", AttrValue::Token(self.source.token().to_string()))
        .attr(
            "document_count_bucket",
            AttrValue::Bucket(self.document_count_bucket),
        )
        .attr(
            "outcome",
            AttrValue::Token(self.outcome.token().to_string()),
        )
});

// ------------------------------------------------------------------- voice
//
// **What is not here is the point.** There is no transcript field, no audio
// field and no confidence field on either payload, and there is no `AttrValue`
// variant that could carry one — so a future emitter cannot add the words back
// by widening a struct. See the two registry rows, ADR-166, and
// `apps/koru-voice/src/log.rs`, which holds the same boundary for logs.

/// How a dictation ended — `voice.dictation.completed`'s `outcome`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DictationOutcome {
    /// The provisional text became one logical edit and one undo step.
    Committed,
    /// The person ended it themselves and kept nothing.
    Cancelled,
    /// An interruption threw the provisional text away — focus lost, the
    /// window closed, the machine slept. Not the person's decision, which is
    /// why it is a different token from `cancelled`.
    Discarded,
    /// The engine or the capture device failed.
    Failed,
}

impl DictationOutcome {
    pub fn token(self) -> &'static str {
        match self {
            Self::Committed => "committed",
            Self::Cancelled => "cancelled",
            Self::Discarded => "discarded",
            Self::Failed => "failed",
        }
    }
}

/// Which kind of editable surface received the dictation — never which one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DictationSurface {
    Document,
    Field,
}

impl DictationSurface {
    pub fn token(self) -> &'static str {
        match self {
            Self::Document => "document",
            Self::Field => "field",
        }
    }
}

/// Which speech runtime ran. `Other` is the local mapping rule, not a gap: an
/// engine id this build does not recognise becomes `Other` **on this machine**,
/// so a raw engine string never leaves it. See
/// [`crate::registry::VOICE_ENGINE_TOKENS`] for how the set may change.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VoiceEngine {
    Sherpa,
    Whisper,
    Other,
}

impl VoiceEngine {
    pub fn token(self) -> &'static str {
        match self {
            Self::Sherpa => "sherpa",
            Self::Whisper => "whisper",
            Self::Other => "other",
        }
    }

    /// Map an engine id to the declared set. Deliberately a *total* function
    /// with no `Option`: the caller has an id and must end up with a token, and
    /// the only alternative to `Other` is a raw string on the wire.
    pub fn from_id(id: &str) -> Self {
        match id {
            "sherpa" => Self::Sherpa,
            "whisper" => Self::Whisper,
            _ => Self::Other,
        }
    }
}

/// The quality tier, mirroring `koru_voice_models::ModelTier`. Mirrored rather
/// than depended on: this crate is the event contract and links nothing of the
/// voice stack, and the two token sets are held together by
/// [`crate::registry::VOICE_TIER_TOKENS`] and the tier's own `as_str` test.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VoiceTier {
    Compact,
    HighAccuracy,
}

impl VoiceTier {
    pub fn token(self) -> &'static str {
        match self {
            Self::Compact => "compact",
            Self::HighAccuracy => "high_accuracy",
        }
    }
}

pub struct VoiceDictationCompleted {
    pub outcome: DictationOutcome,
    pub surface: DictationSurface,
    /// Already mapped to a registry language token or `other` — the
    /// [`AppEnvironment`] rule, for the same reason.
    pub language: &'static str,
    pub engine: VoiceEngine,
    /// Whole **seconds**, already bucketed — see [`bucket`] and the registry
    /// row, which says why not milliseconds.
    pub duration_bucket: u32,
}

payload!(
    VoiceDictationCompleted,
    "voice.dictation.completed",
    |self| {
        Draft::new(EventName::from_static(Self::NAME))
            .attr(
                "outcome",
                AttrValue::Token(self.outcome.token().to_string()),
            )
            .attr(
                "surface",
                AttrValue::Token(self.surface.token().to_string()),
            )
            .attr("language", AttrValue::Token(self.language.to_string()))
            .attr("engine", AttrValue::Token(self.engine.token().to_string()))
            .attr("duration_bucket", AttrValue::Bucket(self.duration_bucket))
    }
);

pub struct VoiceModelInstalled {
    pub tier: VoiceTier,
    pub engine: VoiceEngine,
    /// The descriptor's version, dotted digits.
    pub version: String,
    /// Megabytes, already bucketed — see [`bucket`].
    pub size_mb_bucket: u32,
}

payload!(VoiceModelInstalled, "voice.model.installed", |self| {
    Draft::new(EventName::from_static(Self::NAME))
        .attr("tier", AttrValue::Token(self.tier.token().to_string()))
        .attr("engine", AttrValue::Token(self.engine.token().to_string()))
        .attr("version", AttrValue::Version(self.version))
        .attr("size_mb_bucket", AttrValue::Bucket(self.size_mb_bucket))
});

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AccountClient {
    Web,
    Native,
    Service,
}

impl AccountClient {
    pub fn token(self) -> &'static str {
        match self {
            Self::Web => "web",
            Self::Native => "native",
            Self::Service => "service",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AccountProvider {
    Google,
    Email,
    Apple,
    None,
}

impl AccountProvider {
    pub fn token(self) -> &'static str {
        match self {
            Self::Google => "google",
            Self::Email => "email",
            Self::Apple => "apple",
            Self::None => "none",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AccountPlatform {
    Web,
    Windows,
    Macos,
    Ios,
    Android,
    Other,
    Unknown,
}

impl AccountPlatform {
    pub fn token(self) -> &'static str {
        match self {
            Self::Web => "web",
            Self::Windows => "windows",
            Self::Macos => "macos",
            Self::Ios => "ios",
            Self::Android => "android",
            Self::Other => "other",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AccountStage {
    AuthStart,
    AuthCallback,
    SessionExchange,
    SessionRefresh,
    SessionLogout,
    IdentityLink,
    ProfileCreate,
    ProfileAttach,
    ProfileRebind,
    ProfileDelete,
    ProfileRestore,
    DeviceRegister,
    Enrollment,
}

impl AccountStage {
    pub fn token(self) -> &'static str {
        match self {
            Self::AuthStart => "auth_start",
            Self::AuthCallback => "auth_callback",
            Self::SessionExchange => "session_exchange",
            Self::SessionRefresh => "session_refresh",
            Self::SessionLogout => "session_logout",
            Self::IdentityLink => "identity_link",
            Self::ProfileCreate => "profile_create",
            Self::ProfileAttach => "profile_attach",
            Self::ProfileRebind => "profile_rebind",
            Self::ProfileDelete => "profile_delete",
            Self::ProfileRestore => "profile_restore",
            Self::DeviceRegister => "device_register",
            Self::Enrollment => "enrollment",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AccountOutcome {
    Succeeded,
    Rejected,
    Failed,
}

impl AccountOutcome {
    pub fn token(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Rejected => "rejected",
            Self::Failed => "failed",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AccountFailure {
    None,
    InvalidRequest,
    Unauthenticated,
    PermissionDenied,
    CapabilityUnavailable,
    RateLimited,
    FlowExpired,
    FlowConsumed,
    ProviderRejected,
    AccountMismatch,
    IdentityCollision,
    ProfileAlreadyBound,
    DeviceAlreadyBound,
    Conflict,
    NotFound,
    ServerUnavailable,
    Timeout,
    Integrity,
    Other,
}

impl AccountFailure {
    pub fn token(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::InvalidRequest => "invalid_request",
            Self::Unauthenticated => "unauthenticated",
            Self::PermissionDenied => "permission_denied",
            Self::CapabilityUnavailable => "capability_unavailable",
            Self::RateLimited => "rate_limited",
            Self::FlowExpired => "flow_expired",
            Self::FlowConsumed => "flow_consumed",
            Self::ProviderRejected => "provider_rejected",
            Self::AccountMismatch => "account_mismatch",
            Self::IdentityCollision => "identity_collision",
            Self::ProfileAlreadyBound => "profile_already_bound",
            Self::DeviceAlreadyBound => "device_already_bound",
            Self::Conflict => "conflict",
            Self::NotFound => "not_found",
            Self::ServerUnavailable => "server_unavailable",
            Self::Timeout => "timeout",
            Self::Integrity => "integrity",
            Self::Other => "other",
        }
    }
}

pub struct AccountOperationCompleted {
    pub client: AccountClient,
    pub provider: AccountProvider,
    pub platform: AccountPlatform,
    pub stage: AccountStage,
    pub outcome: AccountOutcome,
    pub failure: AccountFailure,
    pub duration_ms_bucket: u32,
}

payload!(
    AccountOperationCompleted,
    "account.operation.completed",
    |self| {
        Draft::new(EventName::from_static(Self::NAME))
            .attr("client", AttrValue::Token(self.client.token().to_string()))
            .attr(
                "provider",
                AttrValue::Token(self.provider.token().to_string()),
            )
            .attr(
                "platform",
                AttrValue::Token(self.platform.token().to_string()),
            )
            .attr("stage", AttrValue::Token(self.stage.token().to_string()))
            .attr(
                "outcome",
                AttrValue::Token(self.outcome.token().to_string()),
            )
            .attr(
                "failure",
                AttrValue::Token(self.failure.token().to_string()),
            )
            .attr(
                "duration_ms_bucket",
                AttrValue::Bucket(self.duration_ms_bucket),
            )
    }
);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TerminalSurface {
    Terminal,
}

impl TerminalSurface {
    pub fn token(self) -> &'static str {
        match self {
            Self::Terminal => "terminal",
        }
    }
}

pub struct TerminalSurfaceOpened {
    pub surface: TerminalSurface,
}

payload!(TerminalSurfaceOpened, "terminal.surface.opened", |self| {
    Draft::new(EventName::from_static(Self::NAME)).attr(
        "surface",
        AttrValue::Token(self.surface.token().to_string()),
    )
});

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TerminalMode {
    Raw,
    Managed,
}

impl TerminalMode {
    pub fn token(self) -> &'static str {
        match self {
            Self::Raw => "raw",
            Self::Managed => "managed",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TerminalProfileKind {
    Shell,
    Agent,
    Custom,
}

impl TerminalProfileKind {
    pub fn token(self) -> &'static str {
        match self {
            Self::Shell => "shell",
            Self::Agent => "agent",
            Self::Custom => "custom",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TerminalProviderKind {
    None,
    Claude,
    Opencode,
    Codex,
    Grok,
    Kimi,
    Other,
}

impl TerminalProviderKind {
    pub fn token(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Claude => "claude",
            Self::Opencode => "opencode",
            Self::Codex => "codex",
            Self::Grok => "grok",
            Self::Kimi => "kimi",
            Self::Other => "other",
        }
    }
}

pub struct TerminalSessionStarted {
    pub mode: TerminalMode,
    pub profile_kind: TerminalProfileKind,
    pub provider_kind: TerminalProviderKind,
}

payload!(
    TerminalSessionStarted,
    "terminal.session.started",
    |self| {
        Draft::new(EventName::from_static(Self::NAME))
            .attr("mode", AttrValue::Token(self.mode.token().to_string()))
            .attr(
                "profile_kind",
                AttrValue::Token(self.profile_kind.token().to_string()),
            )
            .attr(
                "provider_kind",
                AttrValue::Token(self.provider_kind.token().to_string()),
            )
    }
);

pub struct TerminalSessionReattached;

payload!(
    TerminalSessionReattached,
    "terminal.session.reattached",
    |self| {
        let _ = self;
        Draft::new(EventName::from_static(Self::NAME))
    }
);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TerminalSessionOutcome {
    Exited,
    Failed,
    Terminated,
}

impl TerminalSessionOutcome {
    pub fn token(self) -> &'static str {
        match self {
            Self::Exited => "exited",
            Self::Failed => "failed",
            Self::Terminated => "terminated",
        }
    }
}

pub struct TerminalSessionEnded {
    pub outcome: TerminalSessionOutcome,
    pub duration_minutes_bucket: u32,
}

payload!(TerminalSessionEnded, "terminal.session.ended", |self| {
    Draft::new(EventName::from_static(Self::NAME))
        .attr(
            "outcome",
            AttrValue::Token(self.outcome.token().to_string()),
        )
        .attr(
            "duration_minutes_bucket",
            AttrValue::Bucket(self.duration_minutes_bucket),
        )
});

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TerminalInputKind {
    Text,
    Image,
    Mixed,
}

impl TerminalInputKind {
    pub fn token(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Image => "image",
            Self::Mixed => "mixed",
        }
    }
}

pub struct TerminalInputSubmitted {
    pub input_kind: TerminalInputKind,
}

payload!(
    TerminalInputSubmitted,
    "terminal.input.submitted",
    |self| {
        Draft::new(EventName::from_static(Self::NAME)).attr(
            "input_kind",
            AttrValue::Token(self.input_kind.token().to_string()),
        )
    }
);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TerminalDeliveryMode {
    Native,
    Path,
    Unsupported,
}

impl TerminalDeliveryMode {
    pub fn token(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Path => "path",
            Self::Unsupported => "unsupported",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TerminalDeliveryOutcome {
    Delivered,
    Failed,
    Unsupported,
}

impl TerminalDeliveryOutcome {
    pub fn token(self) -> &'static str {
        match self {
            Self::Delivered => "delivered",
            Self::Failed => "failed",
            Self::Unsupported => "unsupported",
        }
    }
}

pub struct TerminalAttachmentDelivered {
    pub delivery_mode: TerminalDeliveryMode,
    pub outcome: TerminalDeliveryOutcome,
}

payload!(
    TerminalAttachmentDelivered,
    "terminal.attachment.delivered",
    |self| {
        Draft::new(EventName::from_static(Self::NAME))
            .attr(
                "delivery_mode",
                AttrValue::Token(self.delivery_mode.token().to_string()),
            )
            .attr(
                "outcome",
                AttrValue::Token(self.outcome.token().to_string()),
            )
    }
);

#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct TerminalExperienceSummary {
    pub first_present_ms_bucket: Option<u32>,
    pub switch_present_ms_bucket: Option<u32>,
    pub delta_present_p95_ms_bucket: Option<u32>,
    pub render_cpu_p95_ms_bucket: Option<u32>,
    pub jank_per_thousand_bucket: Option<u32>,
    pub stream_gap_count_bucket: Option<u32>,
    pub sample_count_bucket: Option<u32>,
}

payload!(
    TerminalExperienceSummary,
    "terminal.experience.summary",
    |self| {
        let mut draft = Draft::new(EventName::from_static(Self::NAME));
        for (key, value) in [
            ("first_present_ms_bucket", self.first_present_ms_bucket),
            ("switch_present_ms_bucket", self.switch_present_ms_bucket),
            (
                "delta_present_p95_ms_bucket",
                self.delta_present_p95_ms_bucket,
            ),
            ("render_cpu_p95_ms_bucket", self.render_cpu_p95_ms_bucket),
            ("jank_per_thousand_bucket", self.jank_per_thousand_bucket),
            ("stream_gap_count_bucket", self.stream_gap_count_bucket),
            ("sample_count_bucket", self.sample_count_bucket),
        ] {
            if let Some(value) = value {
                draft = draft.attr(key, AttrValue::Bucket(value));
            }
        }
        draft
    }
);

/// Whether a project names a workspace — `terminal.project.*`'s `scope`
/// vocabulary. The workspace's identity deliberately does not travel with it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TerminalProjectScope {
    Workspace,
    Unscoped,
}

impl TerminalProjectScope {
    pub fn token(self) -> &'static str {
        match self {
            TerminalProjectScope::Workspace => "workspace",
            TerminalProjectScope::Unscoped => "unscoped",
        }
    }
}

/// Whether a project was created with a folder — `terminal.project.created`'s
/// `root` vocabulary. The folder itself never leaves the device.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TerminalProjectRoot {
    Set,
    Unset,
}

impl TerminalProjectRoot {
    pub fn token(self) -> &'static str {
        match self {
            TerminalProjectRoot::Set => "set",
            TerminalProjectRoot::Unset => "unset",
        }
    }
}

/// There is no `project_id` here, and no subject: a project id is a reference
/// to a device-local record that no consumer of this bus can resolve, so
/// carrying it would buy nothing and widen what the bus knows.
pub struct TerminalProjectCreated {
    pub scope: TerminalProjectScope,
    pub root: TerminalProjectRoot,
}

payload!(
    TerminalProjectCreated,
    "terminal.project.created",
    |self| {
        Draft::new(EventName::from_static(Self::NAME))
            .attr("scope", AttrValue::Token(self.scope.token().to_string()))
            .attr("root", AttrValue::Token(self.root.token().to_string()))
    }
);

pub struct TerminalProjectDeleted {
    pub scope: TerminalProjectScope,
}

payload!(
    TerminalProjectDeleted,
    "terminal.project.deleted",
    |self| {
        Draft::new(EventName::from_static(Self::NAME))
            .attr("scope", AttrValue::Token(self.scope.token().to_string()))
    }
);

/// Which kind of tab — `terminal.tab.opened`'s `kind` vocabulary.
///
/// Closed, and it is the same three the shell hosts (ADR-263). A Document kind
/// is deferred behind the host-defined tab element; when it arrives it is a
/// fourth token here and a version bump on the row, not a second event.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TerminalTabKind {
    Welcome,
    Session,
    Settings,
}

impl TerminalTabKind {
    pub fn token(self) -> &'static str {
        match self {
            TerminalTabKind::Welcome => "welcome",
            TerminalTabKind::Session => "session",
            TerminalTabKind::Settings => "settings",
        }
    }
}

/// **One token, and deliberately nothing else.** A tab's title is a session's
/// display name or a project's, a session id resolves to a live process, and a
/// tab key is meaningless outside the window that minted it — so none of the
/// three travels. What a consumer can learn is how the three kinds are used.
pub struct TerminalTabOpened {
    pub kind: TerminalTabKind,
}

payload!(TerminalTabOpened, "terminal.tab.opened", |self| {
    Draft::new(EventName::from_static(Self::NAME))
        .attr("kind", AttrValue::Token(self.kind.token().to_string()))
});

/// The one bucketing ladder, so every `Bucket` attr means the same thing:
/// the answer is the largest rung ≤ the value. Coarse on purpose — a bucket
/// is a cardinality bound, not a measurement.
pub const BUCKET_LADDER: &[u64] = &[
    0, 1, 2, 3, 5, 10, 25, 50, 100, 250, 500, 1_000, 2_500, 5_000, 10_000,
];

pub fn bucket(value: u64) -> u32 {
    let mut best = 0u64;
    for &rung in BUCKET_LADDER {
        if rung <= value {
            best = rung;
        } else {
            break;
        }
    }
    best.min(u32::MAX as u64) as u32
}
