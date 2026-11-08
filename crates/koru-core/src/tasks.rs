// SPDX-License-Identifier: MPL-2.0

//! Tasks: the six canonical commands, and the model they act on.
//!
//! **The node is the task** ([ADR-285]). There is no second object: task
//! metadata lives in the attribute registry beside the node's content — the
//! [`KEY_TASK`] marker recording *how* the node became a task, the canonical
//! [`KEY_TASK_STATE`] lifecycle, and the [`KEY_DUE`] this crate already had.
//! `is_task = marker OR due`, and [`is_task`] is the only place that is
//! written down.
//!
//! **Free functions, not methods on [`WorkspaceDoc`]**, for two reasons
//! ([ADR-286]). `koru_core::tasks::complete_task(` is greppable at every call
//! site in the tree, which is exactly what the "every surface uses the
//! canonical commands" invariant is tested by; and `tasks` is a *policy* layer
//! above the store operations, so putting it beside `set_attr` on a type every
//! surface already holds would make the boundary invisible at the moment
//! somebody is looking for a shortcut.
//!
//! **Validate first.** Every expected failure happens before the first write.
//! A command reads, validates, and builds the exact list of mutations it will
//! perform; then it opens one undo group and performs only
//! `set_one_attr_raw` / `remove_attr` / `set_done` over records preflight
//! already read. There is no staging, no rollback and no compensating
//! operation, because there is nothing left that can fail — the proof, step by
//! step, is in [ADR-285]'s *Validate-first* section, and the tests that inject
//! a failure at every preflight point and assert the document unchanged are in
//! `tasks/tests.rs`.
//!
//! **This module emits nothing.** A command returns a [`TaskOutcome`] carrying
//! [`ActivityIntent`]s — references and closed tokens only, never a title, a
//! date or a line of text — and the host wrapper decides what to do with them.
//! `koru-core` is the crate everything on the startup path depends on, and it
//! is not acquiring `koru-events` to say that something was ticked.
//!
//! [`KEY_TASK`]: crate::attributes::KEY_TASK
//! [`KEY_TASK_STATE`]: crate::attributes::KEY_TASK_STATE
//! [`KEY_DUE`]: crate::attributes::KEY_DUE
//! [ADR-285]: ../../../docs/decisions/ADR-276-300.md#adr-285-taskmodel--a-task-is-a-marked-node-and-the-marker-or-a-due-date-is-the-whole-derivation
//! [ADR-286]: ../../../docs/decisions/ADR-276-300.md#adr-286-taskboundary--six-canonical-commands-own-task-state-and-every-host-is-a-thin-wrapper-around-them

use crate::attributes::{
    AttributeDefinition, KEY_COMPLETED_AT, KEY_DUE, KEY_PRIORITY, KEY_TASK, KEY_TASK_STATE,
    KEY_TODAY, LEGACY_DUE_ROLE, Lifecycle, Priority, TaskReason, is_command_only_key, is_due_key,
    system_definition, validate,
};
use crate::ids::{AttrId, EntityRef, NodeId};
use crate::node::{Attr, AttrValue, Kind, ListStyle, Node};
use crate::schema::{Parent, Result, StoreError, WorkspaceDoc};
use crate::time::{DateOnly, Temporal, TemporalKind, ZoneRules};

pub mod faithful_import;
pub mod refs;
pub mod routed;

/// The golden cases — the data-only table `koru-editor` and `koru-mcp-server`
/// replay through their own wrappers to prove they reach the domain's answers
/// (F-3). `any(test, feature = "golden")` rather than `cfg(test)` because **a
/// dependency is never compiled with `cfg(test)`**, so a `cfg(test)` module is
/// invisible to exactly the crates that need this one — the same shape as
/// `koru-search-ui`'s `no-reader`.
#[cfg(any(test, feature = "golden"))]
pub mod golden;

#[cfg(test)]
mod tests;

// ----------------------------------------------------------------- refusals

/// Why a canonical command refused. A closed vocabulary with stable tokens, so
/// MCP maps it and a test asserts on it.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum TaskRefusal {
    /// SPEC §4: the kind cannot carry task state.
    KindNotCapable,
    /// The target is not a task, and this command does not create one.
    NotATask,
}

impl TaskRefusal {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::KindNotCapable => "kind_not_capable",
            Self::NotATask => "not_a_task",
        }
    }
}

fn refuse(node: NodeId, why: TaskRefusal) -> StoreError {
    StoreError::TaskRejected { node, why }
}

// -------------------------------------------------------------- composition

thread_local! {
    /// Whether a [`compose`] call is open on this thread.
    ///
    /// A property of the *call stack*, not of a document, which is why it is
    /// not a field: `compose` brackets a run of commands, and the commands it
    /// brackets are the ones between its two braces.
    static COMPOSING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Run several canonical commands as **one undo step**.
///
/// Every command opens its own group, deliberately: a checkbox click is its own
/// Ctrl+Z and must not fold into whatever was typed a moment earlier. But a
/// *gesture* is sometimes several commands — a due applied over a three-row
/// selection is three `set_due`s, and taking the typed `/due friday` back out
/// is a fourth edit beside them — and those have always been one undo step
/// ([the multi-select contract](../../../docs/features/tasks/SPEC.md)). Without this a
/// host would have to choose between the canonical commands and that contract.
///
/// Inside the closure a command skips only the `checkpoint()` that would *end*
/// this group; its `atomic` nests as it already does, so nothing else about it
/// changes. Nesting is handled: an inner `compose` joins the outer one.
///
/// It brackets undo grouping and nothing else. It is not a transaction, it
/// does not stage, and a command that refuses inside one leaves the commands
/// before it applied — exactly as it would outside.
pub fn compose<T>(ws: &mut WorkspaceDoc, f: impl FnOnce(&mut WorkspaceDoc) -> T) -> T {
    let outer = COMPOSING.replace(true);
    if !outer {
        ws.checkpoint();
        ws.begin_group();
    }
    let out = f(ws);
    COMPOSING.set(outer);
    if !outer {
        ws.checkpoint();
    }
    out
}

/// The `checkpoint()` every command makes before it writes: close whatever was
/// typing so the command is its own undo step — **unless** a [`compose`] call
/// has said the caller owns the boundary.
fn command_checkpoint(ws: &mut WorkspaceDoc) {
    if COMPOSING.with(std::cell::Cell::get) {
        return;
    }
    ws.checkpoint();
}

// -------------------------------------------------------- capable kinds

/// SPEC §4's closed set, as a list — the read projection builds its SQL from
/// this the way `priority_rank_sql` is built from `Priority::ALL`, so the
/// predicate and the query cannot drift.
///
/// [`Kind::Paragraph`] covers both text rows and checklist items: a checklist
/// item is a paragraph under a `ListBlock` whose style is a checkbox, not a
/// kind of its own. [`Kind::Document`] is how a page is promoted, because the
/// title row *is* the page. [`Kind::Task`] is the legacy outline kind, reused
/// — capable, but a task and a checklist item are not the same thing, and it
/// carries no dedicated UI element of its own.
pub const TASK_CAPABLE_KINDS: [Kind; 5] = [
    Kind::Paragraph,
    Kind::Task,
    Kind::Document,
    Kind::Image,
    Kind::File,
];

/// **The only authority** on which kinds may carry task state — the editor,
/// the commands, API/MCP validation, the projection and the views all ask this
/// and none of them keeps a second list.
///
/// Excluded, and *rejected* rather than merely hidden: quotes, list
/// containers, tables, code, canvases, contacts, dividers, and the hidden
/// trash root. The escape hatch is a nearby checklist item ("Review …") made
/// the task instead.
pub fn task_capable(kind: Kind) -> bool {
    TASK_CAPABLE_KINDS.contains(&kind)
}

// ------------------------------------------------------------ node parsing

/// The task-only fields a node carries — and **exactly** the set
/// [`remove_from_tasks`] clears, except [`done`](Self::done), which is content.
///
/// Priority is deliberately not here: it is a general content attribute and
/// survives removal (SPEC §6).
#[derive(Clone, PartialEq, Default, Debug)]
pub struct TaskFields {
    /// Present once the marker has been written. Absent on a task that exists
    /// only because it carries a due.
    pub marker: Option<TaskReason>,
    pub due: Option<Temporal>,
    /// The canonical lifecycle. **Absence is [`Lifecycle::Open`]**, so a
    /// due-derived task is open without anybody writing a value.
    pub lifecycle: Lifecycle,
    /// The day it was finished on, when it is finished — `None` on an open
    /// task, and on one completed by a build that did not record it.
    pub completed_at: Option<crate::time::DateOnly>,
    /// The `NodeMeta::done` mirror, read back so a caller can *see* a
    /// divergence rather than having to look for one. SPEC §5 lets the two
    /// disagree on a task with children, where the box means less than
    /// completion; nothing derives one from the other, at merge time or at
    /// read time.
    pub done: bool,
    /// **Put on Today by hand**, whatever the due says.
    ///
    /// An extra membership, never a relocation: [`bucket`] does not read it and
    /// still names the one natural bucket, and the Today *queue* is the natural
    /// Today plus the pinned. A task due Friday and pinned is on both, which is
    /// the whole difference between "I am working on this today" and "this is
    /// owed today".
    pub pinned_today: bool,
}

impl TaskFields {
    /// Parsed from a node that has already been read. **Pure** — no document
    /// access, so a caller holding the [`Node`] pays nothing.
    ///
    /// Each key is read from the **lowest [`AttrId`]** under it, because
    /// `read_attrs` sorts by `(role, id)` and the cardinality-one write folds
    /// convergence duplicates into that same record: two peers marking one node
    /// concurrently converge on one answer without talking, and reads follow
    /// the same rule.
    pub fn of(node: &Node) -> Self {
        Self {
            marker: node
                .attr(KEY_TASK)
                .and_then(Attr::as_text)
                .and_then(TaskReason::parse),
            due: node.due(),
            lifecycle: node
                .attr(KEY_TASK_STATE)
                .and_then(Attr::as_text)
                .and_then(Lifecycle::parse)
                .unwrap_or_default(),
            completed_at: node.attr(KEY_COMPLETED_AT).and_then(Attr::as_date),
            done: node.meta.done,
            // Presence is the whole of it: the command writes `true` and
            // removes the record to unpin, so a stored `false` cannot arise —
            // and if one ever did, from a peer on an older build, it reads as
            // unpinned rather than as pinned-with-a-false.
            pinned_today: node
                .attr(KEY_TODAY)
                .and_then(Attr::as_bool)
                .unwrap_or(false),
        }
    }
}

/// `is_task = marker OR koru.due`. **The whole derivation**, and the only one:
/// nothing else in the tree may decide what a task is.
///
/// The due is read through [`Node::due`], which already falls back to the
/// pre-registry bare `due` role — so a legacy due-dated row is a task with no
/// migration.
pub fn is_task(fields: &TaskFields) -> bool {
    fields.marker.is_some() || fields.due.is_some()
}

/// The same question with a document in hand.
pub fn is_task_node(ws: &WorkspaceDoc, node: NodeId) -> Result<bool> {
    Ok(is_task(&TaskFields::of(&ws.read(node)?)))
}

// ------------------------------------------------------------ classification

/// What a node placed under a task counts as (SPEC §5). **Pure**: it looks at
/// the node and its own state, never up the tree.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum ChildClass {
    /// Work that is not done. It reopens a completed ancestor task.
    UnfinishedStructural,
    /// Structural, and already finished. No reopen.
    FinishedStructural,
    /// Notes, references, plain images and files. Never reopens anything.
    Passive,
}

impl ChildClass {
    /// The stable token an activity payload carries.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnfinishedStructural => "unfinished_structural",
            Self::FinishedStructural => "finished_structural",
            Self::Passive => "passive",
        }
    }

    /// Whether this class is structural at all — either of the two that are.
    pub fn is_structural(self) -> bool {
        !matches!(self, Self::Passive)
    }
}

/// Classify one node as a child (SPEC §5), in four rules and no others:
///
/// 1. a task whose lifecycle is open → `UnfinishedStructural` (an open nested
///    task, including an image or file that is itself an open task);
/// 2. a task whose lifecycle is completed → `FinishedStructural`;
/// 3. a checkbox-styled list item → its `done` decides, unchecked being
///    unfinished. **A bare `Kind::Task` row is not checkable by kind** — a task
///    and a checklist item are not the same thing, and a legacy row that is
///    neither marked nor checkbox-styled falls to rule 4;
/// 4. anything else → `Passive`.
///
/// SPEC's other two cases — "a descendant becoming an open task", "a completed
/// structural child turning open" — are *transitions* rather than classes, and
/// they are handled by calling [`reclassify`] at the moment they happen.
pub fn classify_child(ws: &WorkspaceDoc, child: NodeId) -> Result<ChildClass> {
    let node = ws.read(child)?;
    let fields = TaskFields::of(&node);
    if is_task(&fields) {
        return Ok(match fields.lifecycle {
            Lifecycle::Open => ChildClass::UnfinishedStructural,
            Lifecycle::Completed => ChildClass::FinishedStructural,
        });
    }
    if in_checkbox_list(ws, child)? {
        return Ok(if node.meta.done {
            ChildClass::FinishedStructural
        } else {
            ChildClass::UnfinishedStructural
        });
    }
    Ok(ChildClass::Passive)
}

/// Whether this node is an item of a checkbox-styled list — the one predicate
/// that makes a row *checkable* without being a task.
fn in_checkbox_list(ws: &WorkspaceDoc, node: NodeId) -> Result<bool> {
    let Parent::Node(parent) = ws.parent(node)? else {
        return Ok(false);
    };
    let parent = ws.read(parent)?;
    Ok(parent.kind == Kind::ListBlock && parent.meta.list_style == Some(ListStyle::Checkbox))
}

/// Whether completing this node means ticking a box: a checkbox-styled list
/// item, or a task-marked node. **Never bare `done` on an arbitrary kind, and
/// never bare `Kind::Task` by kind alone** — the cascade's rule, and the one
/// thing that keeps it from writing `done` onto tables and quotes.
fn checkable(ws: &WorkspaceDoc, node: &Node) -> Result<bool> {
    if is_task(&TaskFields::of(node)) {
        return Ok(true);
    }
    in_checkbox_list(ws, node.id)
}

/// Whether anything structural hangs below `parent`.
///
/// **A list block is transparent here**, and that is the whole subtlety: a
/// checklist under a task is a `ListBlock` holding the items, so the task's
/// direct child is the container and its *items* are the structural children.
/// Classifying the container instead would answer `Passive` and make every
/// checklist-bearing task look childless — which is the difference between a
/// checkbox that completes a task and one that is only a box.
fn any_structural_child(ws: &WorkspaceDoc, parent: NodeId) -> Result<bool> {
    any_child_where(ws, parent, &ChildClass::is_structural)
}

/// Whether this node is holding work that is **not done** — at least one direct
/// child classifying [`ChildClass::UnfinishedStructural`].
///
/// On a *completed* task that is SPEC §5's **concurrent residue**: reopen runs
/// command-time on the device that performed the add, so when one offline peer
/// completes a task while another puts unfinished work under it, the merge stays
/// completed and nothing rewrites the lifecycle. A surface that shows completion
/// therefore has to be able to say *and it still has unfinished items* — which is
/// what this answers, and the only thing it is for. **Lifecycle is never derived
/// from it**: it is a question a view asks, never a value anybody stores.
///
/// It shares [`classify_child`] and the list-block transparency with
/// [`any_structural_child`], so the residue and the checkbox-completes rule
/// cannot drift into two opinions about what a child is.
///
/// **Cost.** One `children` per node plus a `read` per child, one level deep —
/// no ancestry, no descent past a list block's items. A caller that runs it per
/// row must gate it on the row already being a completed task, or it has turned
/// a document walk into a document walk squared.
pub fn has_unfinished_structural_child(ws: &WorkspaceDoc, parent: NodeId) -> Result<bool> {
    any_child_where(ws, parent, &|class| {
        class == ChildClass::UnfinishedStructural
    })
}

/// The shared walk behind both predicates above.
fn any_child_where(
    ws: &WorkspaceDoc,
    parent: NodeId,
    want: &dyn Fn(ChildClass) -> bool,
) -> Result<bool> {
    for child in ws.children(Parent::Node(parent))? {
        if ws.kind_of(child) == Some(Kind::ListBlock) {
            if any_child_where(ws, child, want)? {
                return Ok(true);
            }
            continue;
        }
        if want(classify_child(ws, child)?) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// A node's task situation **in the document that holds it** — everything
/// [`TaskFields`] cannot answer alone, because it needs ancestry.
///
/// Two types rather than one: `TaskFields` is a pure parse of a node a caller
/// already read, and this is a walk. Anything that costs a walk should say so
/// in its name.
#[derive(Clone, PartialEq, Debug)]
pub struct TaskState {
    pub node: NodeId,
    pub kind: Kind,
    pub fields: TaskFields,
    /// [`is_task`] over [`fields`](Self::fields), carried so a caller does not
    /// re-derive it.
    pub is_task: bool,
    /// How this node counts as a child of whatever holds it.
    pub class: ChildClass,
    /// Whether any direct child is structural. SPEC §5's "a task with no
    /// structural children" — the condition under which the checkbox and the
    /// lifecycle are one synced state.
    pub has_structural_children: bool,
    /// Completed task ancestors, **nearest first** — the ones a reopen
    /// propagates to.
    pub completed_task_ancestors: Vec<NodeId>,
    /// The nearest ancestor that is a task at all, completed or not — the
    /// subject a `task.child.*` event is filed under.
    pub nearest_task_ancestor: Option<NodeId>,
}

impl TaskState {
    pub fn of(ws: &WorkspaceDoc, node: NodeId) -> Result<Self> {
        let read = ws.read(node)?;
        let fields = TaskFields::of(&read);
        let has_structural_children = any_structural_child(ws, node)?;
        let mut completed_task_ancestors = Vec::new();
        let mut nearest_task_ancestor = None;
        for ancestor in ws.ancestors(node)? {
            let ancestor_fields = TaskFields::of(&ws.read(ancestor)?);
            if !is_task(&ancestor_fields) {
                continue;
            }
            if nearest_task_ancestor.is_none() {
                nearest_task_ancestor = Some(ancestor);
            }
            if ancestor_fields.lifecycle == Lifecycle::Completed {
                completed_task_ancestors.push(ancestor);
            }
        }
        Ok(Self {
            node,
            kind: read.kind,
            is_task: is_task(&fields),
            class: classify_child(ws, node)?,
            fields,
            has_structural_children,
            completed_task_ancestors,
            nearest_task_ancestor,
        })
    }
}

/// `true` when ticking this node's box **is** completion: it is a task with no
/// structural children.
///
/// The editor calls `complete_task`/`reopen_task` when this is true and
/// [`WorkspaceDoc::set_checkbox`] when it is false, and decides nothing itself.
/// A plain checklist item answers `false` and keeps its ordinary check path.
pub fn checkbox_completes(ws: &WorkspaceDoc, node: NodeId) -> Result<bool> {
    let state = TaskState::of(ws, node)?;
    Ok(state.is_task && !state.has_structural_children)
}

// ------------------------------------------------------------------ outcomes

/// The id every event of one command shares. A v4 UUID minted by the command
/// **before any event exists**; a wrapper carries it as `correlation_id` on
/// every envelope it produces, which is what makes desktop and MCP group a
/// cascade identically.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct CausationId(pub uuid::Uuid);

impl CausationId {
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

impl Default for CausationId {
    fn default() -> Self {
        Self::new()
    }
}

/// **The single source** for commands, activity, the API, tests and docs
/// (roadmap §3.10). Every lifecycle change names exactly one.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum LifecycleReason {
    ExplicitComplete,
    CheckboxCompleted,
    CascadeFromAncestor,
    ExplicitReopen,
    CheckboxReopened,
    /// SPEC §5's auto-reopen, spelled `UNFINISHED_CHILD_ADDED` there; the
    /// storage and wire token is the lowercase form.
    UnfinishedChildAdded,
}

impl LifecycleReason {
    pub const ALL: [Self; 6] = [
        Self::ExplicitComplete,
        Self::CheckboxCompleted,
        Self::CascadeFromAncestor,
        Self::ExplicitReopen,
        Self::CheckboxReopened,
        Self::UnfinishedChildAdded,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::ExplicitComplete => "explicit_complete",
            Self::CheckboxCompleted => "checkbox_completed",
            Self::CascadeFromAncestor => "cascade_from_ancestor",
            Self::ExplicitReopen => "explicit_reopen",
            Self::CheckboxReopened => "checkbox_reopened",
            Self::UnfinishedChildAdded => "unfinished_child_added",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.as_str() == s)
    }

    /// Whether this reason completes, as opposed to reopens — so the two event
    /// vocabularies are derived from the enum rather than typed twice.
    pub fn completes(self) -> bool {
        matches!(
            self,
            Self::ExplicitComplete | Self::CheckboxCompleted | Self::CascadeFromAncestor
        )
    }
}

/// What a canonical command did.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum TaskEffect {
    /// A guarded no-op. **A real answer, never an error** — a repeated gesture
    /// leaves no phantom Ctrl+Z.
    Unchanged,
    Created,
    Removed,
    Completed,
    Reopened,
    DueChanged,
    DueCleared,
    PinnedToday,
    UnpinnedToday,
}

impl TaskEffect {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unchanged => "unchanged",
            Self::Created => "created",
            Self::Removed => "removed",
            Self::Completed => "completed",
            Self::Reopened => "reopened",
            Self::DueChanged => "due_changed",
            Self::DueCleared => "due_cleared",
            Self::PinnedToday => "pinned_today",
            Self::UnpinnedToday => "unpinned_today",
        }
    }
}

/// Whether an attribute was set for the first time, changed, or cleared —
/// the `change` token `task.due.changed` and `task.priority.changed` carry.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum AttrChange {
    Set,
    Changed,
    Cleared,
}

impl AttrChange {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Set => "set",
            Self::Changed => "changed",
            Self::Cleared => "cleared",
        }
    }
}

/// What a host should **attempt** to append (SPEC §9: best effort — the task
/// mutation commits first, and a failed journal append never rolls it back).
///
/// Carries references and closed tokens only. There is no free string here and
/// there is not going to be one: no title, no text, no date, no tag name, no
/// color and no custom value.
///
/// `task.content.changed` is deliberately absent. It belongs to no command —
/// the domain does not know what an editing burst is — and its coalescing owner
/// is the desktop host wrapper (ADR-273).
#[derive(Clone, PartialEq, Debug)]
pub enum ActivityIntent {
    Created {
        task: EntityRef,
        reason: TaskReason,
        node_kind: Kind,
    },
    Removed {
        task: EntityRef,
    },
    Completed {
        task: EntityRef,
        reason: LifecycleReason,
    },
    Reopened {
        task: EntityRef,
        reason: LifecycleReason,
    },
    DueChanged {
        task: EntityRef,
        change: AttrChange,
        /// The *shape* the deadline now has, never its value. `None` once it
        /// has been cleared.
        temporal_kind: Option<TemporalKind>,
    },
    PriorityChanged {
        task: EntityRef,
        change: AttrChange,
        priority: Option<Priority>,
    },
    ChildAdded {
        task: EntityRef,
        child: NodeId,
        class: ChildClass,
        reopened: bool,
    },
    ChildMoved {
        task: EntityRef,
        child: NodeId,
        class: ChildClass,
        reopened: bool,
    },
    ChildRemoved {
        task: EntityRef,
        child: NodeId,
    },
    ChildCompleted {
        task: EntityRef,
        child: NodeId,
        child_is_task: bool,
    },
    ChildReopened {
        task: EntityRef,
        child: NodeId,
        child_is_task: bool,
        reopened_parent: bool,
    },
}

impl ActivityIntent {
    /// The registry name of the event this intent asks for — ADR-273's table,
    /// so a wrapper maps rather than decides.
    pub fn event(&self) -> &'static str {
        match self {
            Self::Created { .. } => "task.created",
            Self::Removed { .. } => "task.removed",
            Self::Completed { .. } => "task.completed",
            Self::Reopened { .. } => "task.reopened",
            Self::DueChanged { .. } => "task.due.changed",
            Self::PriorityChanged { .. } => "task.priority.changed",
            Self::ChildAdded { .. } => "task.child.added",
            Self::ChildMoved { .. } => "task.child.moved",
            Self::ChildRemoved { .. } => "task.child.removed",
            Self::ChildCompleted { .. } => "task.child.completed",
            Self::ChildReopened { .. } => "task.child.reopened",
        }
    }

    /// The task this event is filed under — always the `Task` spelling.
    pub fn subject(&self) -> &EntityRef {
        match self {
            Self::Created { task, .. }
            | Self::Removed { task }
            | Self::Completed { task, .. }
            | Self::Reopened { task, .. }
            | Self::DueChanged { task, .. }
            | Self::PriorityChanged { task, .. }
            | Self::ChildAdded { task, .. }
            | Self::ChildMoved { task, .. }
            | Self::ChildRemoved { task, .. }
            | Self::ChildCompleted { task, .. }
            | Self::ChildReopened { task, .. } => task,
        }
    }

    /// The same intent, told as a **move** rather than an insert. The two
    /// events differ only in which structural gesture produced them, and
    /// [`reclassify`] cannot know which; a move call site says so here rather
    /// than the domain guessing.
    pub fn as_moved(self) -> Self {
        match self {
            Self::ChildAdded {
                task,
                child,
                class,
                reopened,
            } => Self::ChildMoved {
                task,
                child,
                class,
                reopened,
            },
            other => other,
        }
    }
}

/// What a canonical command did — the one value every host consumes.
#[derive(Clone, PartialEq, Debug)]
pub struct TaskOutcome {
    /// The task this command names. Always the [`EntityRef::Task`] variant.
    pub task: EntityRef,
    pub effect: TaskEffect,
    /// Set when this command brought the task into existence — `set_due` on a
    /// plain row both creates and dates it, and a host shows one success.
    pub created_with_reason: Option<TaskReason>,
    /// The lifecycle reason, when the effect carries one. Also set when
    /// [`ensure_task`] adopted an already-checked box, which starts the task
    /// completed.
    pub reason: Option<LifecycleReason>,
    /// One id shared by every event this command produces, cascade included.
    pub causation: CausationId,
    /// Tasks the cascade completed, in document order, excluding
    /// [`task`](Self::task).
    pub cascaded: Vec<EntityRef>,
    /// Completed tasks the reopen chain reopened above [`task`](Self::task),
    /// nearest first.
    pub reopened_ancestors: Vec<EntityRef>,
    /// What the host should attempt to append, in order.
    pub activity: Vec<ActivityIntent>,
}

impl TaskOutcome {
    fn new(task: EntityRef, effect: TaskEffect) -> Self {
        Self {
            task,
            effect,
            created_with_reason: None,
            reason: None,
            causation: CausationId::new(),
            cascaded: Vec::new(),
            reopened_ancestors: Vec::new(),
            activity: Vec::new(),
        }
    }

    /// The guarded no-op: nothing to do, and that is an answer rather than an
    /// error.
    fn unchanged(task: EntityRef) -> Self {
        Self::new(task, TaskEffect::Unchanged)
    }

    /// Whether this command changed the document at all.
    pub fn changed(&self) -> bool {
        self.effect != TaskEffect::Unchanged
    }

    /// One line naming what happened — the shape a host prints under
    /// `KORU_TASKS_DEBUG`. Tokens, counts and a causation id; **no content**,
    /// for the same reason the intents carry none.
    pub fn describe(&self) -> String {
        format!(
            "task {} effect={} reason={} created={} cascaded={} reopened_ancestors={} activity={} causation={}",
            self.task.to_storage(),
            self.effect.as_str(),
            self.reason.map(|r| r.as_str()).unwrap_or("-"),
            self.created_with_reason.map(|r| r.as_str()).unwrap_or("-"),
            self.cascaded.len(),
            self.reopened_ancestors.len(),
            self.activity.len(),
            self.causation.0,
        )
    }
}

// ------------------------------------------------------------- the preflight

/// Everything a command reads about its target before it writes anything:
/// steps 0–4 of both of ADR-272's preflight tables, in that order.
struct Target {
    node: NodeId,
    entity: EntityRef,
    read: Node,
    fields: TaskFields,
}

impl Target {
    fn resolve(ws: &WorkspaceDoc, node: NodeId) -> Result<Self> {
        // 0. document identity — the one fallible read that is not the node's.
        let workspace = ws.id().ok_or(StoreError::Malformed(
            "the document carries no workspace id",
        ))?;
        // 1. container lookup — infallible. **The document this `WorkspaceDoc`
        //    is**, not an id derived from the node: two tasks on one page carry
        //    one document segment (TECHDEBT 72, closed).
        let document = refs::document_segment(ws);
        // 2. the target read, which is what resolves the id.
        let read = ws.read(node)?;
        // 3-4. kind and attribute parse — pure.
        let fields = TaskFields::of(&read);
        Ok(Self {
            node,
            entity: EntityRef::task(workspace, document, node),
            read,
            fields,
        })
    }

    fn require_capable(&self) -> Result<()> {
        if !task_capable(self.read.kind) {
            return Err(refuse(self.node, TaskRefusal::KindNotCapable));
        }
        Ok(())
    }

    fn require_task(&self) -> Result<()> {
        if !is_task(&self.fields) {
            return Err(refuse(self.node, TaskRefusal::NotATask));
        }
        Ok(())
    }

    fn is_task(&self) -> bool {
        is_task(&self.fields)
    }

    /// The attrs already on the node under one role, in `(role, id)` order —
    /// read once in preflight and handed straight to `set_one_attr_raw`, so the
    /// mutation phase never looks anything up.
    fn existing(&self, role: &str) -> Vec<Attr> {
        self.read
            .content
            .attrs()
            .iter()
            .filter(|a| a.role == role)
            .cloned()
            .collect()
    }
}

fn marker_def() -> AttributeDefinition {
    system_definition(KEY_TASK).expect("koru.task is a registered system attribute")
}

fn lifecycle_def() -> AttributeDefinition {
    system_definition(KEY_TASK_STATE).expect("koru.task_state is a registered system attribute")
}

fn due_def() -> AttributeDefinition {
    system_definition(KEY_DUE).expect("koru.due is a registered system attribute")
}

fn completed_at_def() -> AttributeDefinition {
    system_definition(KEY_COMPLETED_AT).expect("koru.completed_at is a registered system attribute")
}

fn today_def() -> AttributeDefinition {
    system_definition(KEY_TODAY).expect("koru.today is a registered system attribute")
}

/// A token this build produced, validated against its own registry before it is
/// written. Unreachable by construction — the token comes from the enum the
/// definition's options were built from — and it is here so the registry stays
/// the one authority rather than a thing the commands agree with informally.
fn checked_token(def: &AttributeDefinition, token: &str) -> Result<AttrValue> {
    let value = AttrValue::Text(token.to_owned());
    validate(def, &value).map_err(|why| StoreError::AttributeRejected {
        key: def.key.clone(),
        why,
    })?;
    Ok(value)
}

/// **The completed task ancestors a reopen has to carry**, each paired with the
/// lifecycle attributes already on it — read *before* the write, because
/// `set_one_attr_raw` replaces the records it is handed and a chain read
/// afterwards would be reading its own result.
///
/// Nearest first, which is [`TaskState::completed_task_ancestors`]'s order and
/// the order the intents are filed in: the nearest completed ancestor is the
/// task a `task.child.added` is about, and the rest of the chain is
/// [`TaskOutcome::reopened_ancestors`].
///
/// Three commands share it, and they are exactly the three that can make a node
/// count as unfinished structural work: [`reopen_task`], [`ensure_task`] and
/// [`set_due`]. [`reclassify`] does the same read inline because it also needs
/// the classification the other three already know the answer to.
fn completed_ancestor_work(
    ws: &WorkspaceDoc,
    state: &TaskState,
) -> Result<Vec<(NodeId, Vec<Attr>, Vec<AttrId>)>> {
    state
        .completed_task_ancestors
        .iter()
        .map(|&id| {
            ws.read(id).map(|read| {
                let of = |role: &str| {
                    read.content
                        .attrs()
                        .iter()
                        .filter(|a| a.role == role)
                        .cloned()
                        .collect::<Vec<_>>()
                };
                let days = of(KEY_COMPLETED_AT).into_iter().map(|a| a.id).collect();
                (id, of(KEY_TASK_STATE), days)
            })
        })
        .collect()
}

// ----------------------------------------------------------- the six commands

/// Make this node a task, and say why it became one.
///
/// **Idempotent.** Already marked ⇒ [`TaskEffect::Unchanged`]. Not a task yet
/// ⇒ marked with the caller's `reason`. Due-derived (a due, no marker) ⇒ the
/// marker is normalized in with [`TaskReason::Due`], *not* the caller's: the
/// reason records how the task came to exist, and it came from the deadline.
///
/// **It adopts the box.** Promoting a node with no structural children whose
/// checkbox is already checked starts the task completed, in the same atomic
/// group — a childless checklist-backed task and its box are one synced state,
/// in both directions, from the first moment.
///
/// **And it reopens what it landed under.** SPEC §5(d)'s second case is *a
/// descendant becoming an open task*, which is what this command does when the
/// node is not a task yet: an unfinished structural child has appeared under
/// every completed task above it, so every one of them reopens with reason
/// `UNFINISHED_CHILD_ADDED` — the same chain [`reopen_task`] walks, in the same
/// atomic group and under the same causation id, because a command that makes a
/// node open owns the chain above it rather than leaving it to a call site.
/// **The classification lives here rather than at `/task`'s four surfaces**: the
/// editor's slash command, MCP's `set_task`, the image/file context rows and
/// D-3's native task all become correct at once, and a fifth surface cannot
/// forget. Adopting the box is the exception that proves the rule — a promotion
/// that starts *completed* is finished structural work and reopens nothing.
pub fn ensure_task(ws: &mut WorkspaceDoc, node: NodeId, reason: TaskReason) -> Result<TaskOutcome> {
    let target = Target::resolve(ws, node)?;
    target.require_capable()?;
    if target.fields.marker.is_some() {
        return Ok(TaskOutcome::unchanged(target.entity));
    }
    // A due with no marker is already a task; the marker records the deadline
    // as the reason, whatever the caller said.
    let created = !target.is_task();
    let reason = if created { reason } else { TaskReason::Due };
    let marker_def = marker_def();
    let marker_value = checked_token(&marker_def, reason.as_str())?;
    let existing_marker = target.existing(KEY_TASK);

    // Adopting the box: only on the way in, only when nothing structural hangs
    // below, and only when the two would otherwise disagree.
    let state = TaskState::of(ws, node)?;
    let adopt = target.read.meta.done
        && !state.has_structural_children
        && target.fields.lifecycle != Lifecycle::Completed;
    let lifecycle_def = lifecycle_def();
    let lifecycle_value = checked_token(&lifecycle_def, Lifecycle::Completed.as_str())?;
    let existing_lifecycle = target.existing(KEY_TASK_STATE);

    // **Only a node that was not a task before, and that starts open.** A
    // due-derived task being normalized was already open work when the deadline
    // was set — its ancestors were asked then, and asking again would rewrite
    // SPEC §5's concurrent residue, which is a remote peer's edit and not this
    // gesture's. A promotion that adopts a checked box starts completed and is
    // finished structural work.
    let reopens = created && !adopt && target.fields.lifecycle == Lifecycle::Open;
    let open = checked_token(&lifecycle_def, Lifecycle::Open.as_str())?;
    let ancestors = if reopens {
        completed_ancestor_work(ws, &state)?
    } else {
        Vec::new()
    };
    let reopened_ancestors: Vec<EntityRef> = ancestors
        .iter()
        .map(|(id, ..)| task_ref(target.entity, *id))
        .collect();

    command_checkpoint(ws);
    ws.atomic(|ws| {
        ws.set_one_attr_raw(node, &marker_def, existing_marker, marker_value)?;
        if adopt {
            ws.set_one_attr_raw(node, &lifecycle_def, existing_lifecycle, lifecycle_value)?;
        }
        for (id, existing, days) in ancestors {
            ws.set_one_attr_raw(id, &lifecycle_def, existing, open.clone())?;
            for day in days {
                ws.remove_attr(id, day)?;
            }
            ws.set_done(id, false)?;
        }
        Ok(())
    })?;

    let mut outcome = TaskOutcome::new(target.entity, TaskEffect::Created);
    outcome.created_with_reason = Some(reason);
    if adopt {
        outcome.reason = Some(LifecycleReason::CheckboxCompleted);
    }
    outcome.activity.push(ActivityIntent::Created {
        task: target.entity,
        reason,
        node_kind: target.read.kind,
    });
    push_ancestor_reopen(&mut outcome, &reopened_ancestors, node);
    Ok(outcome)
}

/// The activity a creation owes the chain it reopened, and the field that
/// carries the chain to the caller.
///
/// One shape for the two creating commands, because the difference between a
/// `/task` and a `/due` on the same row under the same completed parent is the
/// reason the *task* was created and nothing at all about what the parent was
/// told. Nothing is pushed when the chain is empty, which is every ordinary
/// creation.
fn push_ancestor_reopen(outcome: &mut TaskOutcome, chain: &[EntityRef], child: NodeId) {
    let Some(&nearest) = chain.first() else {
        return;
    };
    for task in chain {
        outcome.activity.push(ActivityIntent::Reopened {
            task: *task,
            reason: LifecycleReason::UnfinishedChildAdded,
        });
    }
    outcome.activity.push(ActivityIntent::ChildAdded {
        task: nearest,
        child,
        class: ChildClass::UnfinishedStructural,
        reopened: true,
    });
    outcome.reopened_ancestors = chain.to_vec();
}

/// Give this node a deadline, which is one of the two ways a task is created.
///
/// The value is validated against the shipped `koru.due` definition — which
/// refuses a floating wall clock, because a shared deadline carries the zone it
/// was meant in — and then, in one atomic group, the due is written and, when
/// the node was not yet a task, the marker with [`TaskReason::Due`].
///
/// **A due that creates a task reopens what it landed under**, exactly as
/// [`ensure_task`] does and for the same sentence of SPEC §5(d): a deadline
/// alone makes a task (SPEC §1), so `/due friday` on a paragraph under a
/// completed task is *a descendant becoming an open task* as surely as `/task`
/// on the same row is. The two creating commands cannot answer this differently
/// without the answer depending on which door was used.
pub fn set_due(ws: &mut WorkspaceDoc, node: NodeId, due: Temporal) -> Result<TaskOutcome> {
    let target = Target::resolve(ws, node)?;
    target.require_capable()?;
    let due_def = due_def();
    let value = AttrValue::from(due.clone());
    validate(&due_def, &value).map_err(|why| StoreError::AttributeRejected {
        key: due_def.key.clone(),
        why,
    })?;
    let existing_due = target.existing(KEY_DUE);
    // A legacy bare-role due is folded away in the same group: two records
    // under two roles would be two answers to "when is this due".
    let legacy: Vec<AttrId> = target
        .existing(LEGACY_DUE_ROLE)
        .into_iter()
        .map(|a| a.id)
        .collect();
    let had = target.fields.due.clone();
    if had.as_ref() == Some(&due) && legacy.is_empty() && target.fields.marker.is_some() {
        return Ok(TaskOutcome::unchanged(target.entity));
    }
    let created = !target.is_task();
    let marker_def = marker_def();
    let marker_value = checked_token(&marker_def, TaskReason::Due.as_str())?;
    let existing_marker = target.existing(KEY_TASK);
    let needs_marker = target.fields.marker.is_none();

    // The walk is paid for only by the creating case — a due *changed* on a row
    // that was already a task classified the same way before and after, so there
    // is nothing here for an ordinary date edit.
    let reopens = created && target.fields.lifecycle == Lifecycle::Open;
    let (lifecycle_def, open, ancestors) = if reopens {
        let def = lifecycle_def();
        let open = checked_token(&def, Lifecycle::Open.as_str())?;
        let work = completed_ancestor_work(ws, &TaskState::of(ws, node)?)?;
        (Some(def), Some(open), work)
    } else {
        (None, None, Vec::new())
    };
    let reopened_ancestors: Vec<EntityRef> = ancestors
        .iter()
        .map(|(id, ..)| task_ref(target.entity, *id))
        .collect();

    command_checkpoint(ws);
    ws.atomic(|ws| {
        ws.set_one_attr_raw(node, &due_def, existing_due, value)?;
        for id in legacy {
            ws.remove_attr(node, id)?;
        }
        if needs_marker {
            ws.set_one_attr_raw(node, &marker_def, existing_marker, marker_value)?;
        }
        if let (Some(def), Some(open)) = (&lifecycle_def, &open) {
            for (id, existing, days) in ancestors {
                ws.set_one_attr_raw(id, def, existing, open.clone())?;
                for day in days {
                    ws.remove_attr(id, day)?;
                }
                ws.set_done(id, false)?;
            }
        }
        Ok(())
    })?;

    let mut outcome = TaskOutcome::new(target.entity, TaskEffect::DueChanged);
    if created {
        outcome.created_with_reason = Some(TaskReason::Due);
        outcome.activity.push(ActivityIntent::Created {
            task: target.entity,
            reason: TaskReason::Due,
            node_kind: target.read.kind,
        });
    }
    outcome.activity.push(ActivityIntent::DueChanged {
        task: target.entity,
        change: if had.is_some() {
            AttrChange::Changed
        } else {
            AttrChange::Set
        },
        temporal_kind: Some(due.kind()),
    });
    push_ancestor_reopen(&mut outcome, &reopened_ancestors, node);
    Ok(outcome)
}

/// Take the deadline away and **keep the task**.
///
/// It marks *first*, inside the same undo group, so a task that existed only
/// because of its due does not evaporate when the due goes (SPEC §3).
pub fn clear_due(ws: &mut WorkspaceDoc, node: NodeId) -> Result<TaskOutcome> {
    let target = Target::resolve(ws, node)?;
    target.require_capable()?;
    if target.fields.due.is_none() {
        return Ok(TaskOutcome::unchanged(target.entity));
    }
    let doomed: Vec<AttrId> = target
        .existing(KEY_DUE)
        .into_iter()
        .chain(target.existing(LEGACY_DUE_ROLE))
        .map(|a| a.id)
        .collect();
    let marker_def = marker_def();
    let marker_value = checked_token(&marker_def, TaskReason::Due.as_str())?;
    let existing_marker = target.existing(KEY_TASK);
    let needs_marker = target.fields.marker.is_none();

    command_checkpoint(ws);
    ws.atomic(|ws| {
        if needs_marker {
            ws.set_one_attr_raw(node, &marker_def, existing_marker, marker_value)?;
        }
        for id in doomed {
            ws.remove_attr(node, id)?;
        }
        Ok(())
    })?;

    let mut outcome = TaskOutcome::new(target.entity, TaskEffect::DueCleared);
    outcome.activity.push(ActivityIntent::DueChanged {
        task: target.entity,
        change: AttrChange::Cleared,
        temporal_kind: None,
    });
    Ok(outcome)
}

/// **Put this task on Today by hand**, whatever its due date says.
///
/// The seventh canonical command, and the first one added since ADR-286 named
/// six. It earns the boundary for the same reason the other six do: which queue
/// a task appears on is a *classification*, and `koru.today` is therefore
/// command-only — a generic attribute write would be a second authority on it.
///
/// **It adds a membership; it does not move the task.** [`bucket`] is untouched
/// and still names the one natural bucket from the due, the lifecycle and the
/// priority. A task due Friday and pinned is on Today *and* on Upcoming,
/// because the pin says "I am working on this today" and the due still says
/// when it is owed. Moving it instead would have made a pin a silent edit of
/// somebody's deadline.
///
/// Refuses a node that is not already a task, rather than making one: pinning
/// is about where a task shows, and a gesture that quietly created tasks would
/// be `ensure_task` wearing another name.
pub fn pin_to_today(ws: &mut WorkspaceDoc, node: NodeId) -> Result<TaskOutcome> {
    let target = Target::resolve(ws, node)?;
    target.require_capable()?;
    target.require_task()?;
    if target.fields.pinned_today {
        return Ok(TaskOutcome::unchanged(target.entity));
    }
    let def = today_def();
    let value = AttrValue::Bool(true);
    validate(&def, &value).map_err(|why| StoreError::AttributeRejected {
        key: def.key.clone(),
        why,
    })?;
    let existing = target.existing(KEY_TODAY);

    command_checkpoint(ws);
    ws.atomic(|ws| ws.set_one_attr_raw(node, &def, existing, value))?;

    Ok(TaskOutcome::new(target.entity, TaskEffect::PinnedToday))
}

/// Take it off Today again. [`pin_to_today`]'s mirror.
///
/// **The record is removed, never set to `false`.** Absence is the whole of
/// "not pinned" — the same rule `koru.task_state`'s absence follows for *open*
/// — so there is one representation of unpinned rather than two that a query
/// would have to know about.
pub fn unpin_from_today(ws: &mut WorkspaceDoc, node: NodeId) -> Result<TaskOutcome> {
    let target = Target::resolve(ws, node)?;
    target.require_capable()?;
    if !target.fields.pinned_today {
        return Ok(TaskOutcome::unchanged(target.entity));
    }
    let doomed: Vec<AttrId> = target
        .existing(KEY_TODAY)
        .into_iter()
        .map(|a| a.id)
        .collect();

    command_checkpoint(ws);
    ws.atomic(|ws| {
        for id in doomed {
            ws.remove_attr(node, id)?;
        }
        Ok(())
    })?;

    Ok(TaskOutcome::new(target.entity, TaskEffect::UnpinnedToday))
}

/// Complete a task, and everything it contains.
///
/// The cascade checks every contained *checkable* — a checkbox-styled list item
/// or a task-marked node, never bare `done` on an arbitrary kind — and
/// completes every nested task below it, as one atomic change sharing one
/// causation id. Checking all of a task's children never auto-completes it;
/// this is the only thing that completes a task with children.
pub fn complete_task(
    ws: &mut WorkspaceDoc,
    node: NodeId,
    reason: LifecycleReason,
    on: crate::time::DateOnly,
) -> Result<TaskOutcome> {
    let target = Target::resolve(ws, node)?;
    target.require_capable()?;
    target.require_task()?;

    let lifecycle_def = lifecycle_def();
    let completed = checked_token(&lifecycle_def, Lifecycle::Completed.as_str())?;
    // **The day, written beside the lifecycle and in the same group**, so a
    // task is never completed-without-a-day or dated-without-being-done. One
    // day for the whole cascade: it is one gesture at one moment.
    let completed_at_def = completed_at_def();
    let completed_on = AttrValue::from(crate::time::Temporal::Date(on));
    validate(&completed_at_def, &completed_on).map_err(|why| StoreError::AttributeRejected {
        key: completed_at_def.key.clone(),
        why,
    })?;

    // The target itself.
    let mut tasks: Vec<(NodeId, Vec<Attr>)> = Vec::new();
    let mut days: Vec<(NodeId, Vec<Attr>)> = Vec::new();
    let mut cascaded: Vec<EntityRef> = Vec::new();
    let mut boxes: Vec<NodeId> = Vec::new();
    let target_lifecycle_moves = target.fields.lifecycle != Lifecycle::Completed;
    if target_lifecycle_moves {
        tasks.push((node, target.existing(KEY_TASK_STATE)));
        days.push((node, target.existing(KEY_COMPLETED_AT)));
    }
    if !target.read.meta.done {
        boxes.push(node);
    }

    // Its subtree, in document order.
    for (descendant, _) in ws.walk_under(node)? {
        let read = ws.read(descendant)?;
        let fields = TaskFields::of(&read);
        let is_a_task = is_task(&fields);
        if is_a_task && fields.lifecycle != Lifecycle::Completed {
            tasks.push((
                descendant,
                read.content
                    .attrs()
                    .iter()
                    .filter(|a| a.role == KEY_TASK_STATE)
                    .cloned()
                    .collect(),
            ));
            days.push((
                descendant,
                read.content
                    .attrs()
                    .iter()
                    .filter(|a| a.role == KEY_COMPLETED_AT)
                    .cloned()
                    .collect(),
            ));
            cascaded.push(task_ref(target.entity, descendant));
        }
        if checkable(ws, &read)? && !read.meta.done {
            boxes.push(descendant);
        }
    }

    if tasks.is_empty() && boxes.is_empty() {
        return Ok(TaskOutcome::unchanged(target.entity));
    }

    // Who hears about it: the nearest task ancestor, if there is one. Read
    // now, with everything else — the mutation phase looks nothing up.
    let parent_task = TaskState::of(ws, node)?
        .nearest_task_ancestor
        .map(|parent| task_ref(target.entity, parent));

    command_checkpoint(ws);
    ws.atomic(|ws| {
        for (id, existing) in tasks {
            ws.set_one_attr_raw(id, &lifecycle_def, existing, completed.clone())?;
        }
        for (id, existing) in days {
            ws.set_one_attr_raw(id, &completed_at_def, existing, completed_on.clone())?;
        }
        for id in boxes {
            ws.set_done(id, true)?;
        }
        Ok(())
    })?;

    let mut outcome = TaskOutcome::new(target.entity, TaskEffect::Completed);
    outcome.reason = Some(reason);
    if target_lifecycle_moves {
        outcome.activity.push(ActivityIntent::Completed {
            task: target.entity,
            reason,
        });
    }
    for task in &cascaded {
        outcome.activity.push(ActivityIntent::Completed {
            task: *task,
            reason: LifecycleReason::CascadeFromAncestor,
        });
    }
    if let Some(parent) = parent_task {
        outcome.activity.push(ActivityIntent::ChildCompleted {
            task: parent,
            child: node,
            child_is_task: true,
        });
    }
    outcome.cascaded = cascaded;
    Ok(outcome)
}

/// Reopen a task. **Reopen never cascades downward** — nothing below it is
/// un-completed — and it runs the reopen classification on itself afterwards,
/// so a task turning open reopens every completed task above it, inside the
/// same group and under the same causation id.
pub fn reopen_task(
    ws: &mut WorkspaceDoc,
    node: NodeId,
    reason: LifecycleReason,
) -> Result<TaskOutcome> {
    let target = Target::resolve(ws, node)?;
    target.require_capable()?;
    target.require_task()?;

    let lifecycle_def = lifecycle_def();
    // Written explicitly rather than deleted: an explicit `open` after a reopen
    // is what makes the transition a real attribute change that the projection
    // and the activity feed can both see.
    let open = checked_token(&lifecycle_def, Lifecycle::Open.as_str())?;
    let lifecycle_moves = target.fields.lifecycle == Lifecycle::Completed;
    let box_moves = target.read.meta.done;
    let existing_lifecycle = target.existing(KEY_TASK_STATE);
    // **The day goes with the lifecycle.** A task that is open again was not
    // finished on any day, and a record saying otherwise is one the Completed
    // window would still be counting.
    let doomed_days: Vec<AttrId> = target
        .existing(KEY_COMPLETED_AT)
        .into_iter()
        .map(|a| a.id)
        .collect();

    let state = TaskState::of(ws, node)?;
    let ancestors = completed_ancestor_work(ws, &state)?;

    if !lifecycle_moves && !box_moves && ancestors.is_empty() {
        return Ok(TaskOutcome::unchanged(target.entity));
    }

    let reopened_ancestors: Vec<EntityRef> = ancestors
        .iter()
        .map(|(id, ..)| task_ref(target.entity, *id))
        .collect();
    let parent_ref = state
        .nearest_task_ancestor
        .map(|parent| task_ref(target.entity, parent));

    command_checkpoint(ws);
    ws.atomic(|ws| {
        if lifecycle_moves {
            ws.set_one_attr_raw(node, &lifecycle_def, existing_lifecycle, open.clone())?;
        }
        for id in doomed_days {
            ws.remove_attr(node, id)?;
        }
        if box_moves {
            ws.set_done(node, false)?;
        }
        for (id, existing, days) in ancestors {
            ws.set_one_attr_raw(id, &lifecycle_def, existing, open.clone())?;
            for day in days {
                ws.remove_attr(id, day)?;
            }
            ws.set_done(id, false)?;
        }
        Ok(())
    })?;

    let mut outcome = TaskOutcome::new(target.entity, TaskEffect::Reopened);
    outcome.reason = Some(reason);
    outcome.activity.push(ActivityIntent::Reopened {
        task: target.entity,
        reason,
    });
    for task in &reopened_ancestors {
        outcome.activity.push(ActivityIntent::Reopened {
            task: *task,
            reason: LifecycleReason::UnfinishedChildAdded,
        });
    }
    if let Some(parent_ref) = parent_ref {
        outcome.activity.push(ActivityIntent::ChildReopened {
            task: parent_ref,
            child: node,
            child_is_task: true,
            reopened_parent: reopened_ancestors.iter().any(|r| *r == parent_ref),
        });
    }
    outcome.reopened_ancestors = reopened_ancestors;
    Ok(outcome)
}

/// Stop tracking this node as a task.
///
/// Clears the marker, the canonical lifecycle and the due — the task-owned
/// fields, and nothing else. **Priority stays**, because it is a general
/// content attribute, and **the checkbox keeps its state**, because check state
/// is content that predates task-ness and stopping tracking never unticks
/// somebody's item (SPEC §6). One atomic group, one undo step. It never leaves
/// a due-derived task behind, because the due goes in the same group.
pub fn remove_from_tasks(ws: &mut WorkspaceDoc, node: NodeId) -> Result<TaskOutcome> {
    let target = Target::resolve(ws, node)?;
    // The kind is *not* a refusal here: a node that cannot carry task state is
    // simply not a task, and the check below says so without an error.
    if !target.is_task() {
        return Ok(TaskOutcome::unchanged(target.entity));
    }
    let doomed: Vec<AttrId> = target
        .existing(KEY_TASK)
        .into_iter()
        .chain(target.existing(KEY_TASK_STATE))
        .chain(target.existing(KEY_COMPLETED_AT))
        .chain(target.existing(KEY_DUE))
        .chain(target.existing(LEGACY_DUE_ROLE))
        .map(|a| a.id)
        .collect();

    command_checkpoint(ws);
    ws.atomic(|ws| {
        for id in doomed {
            ws.remove_attr(node, id)?;
        }
        Ok(())
    })?;

    let mut outcome = TaskOutcome::new(target.entity, TaskEffect::Removed);
    outcome.activity.push(ActivityIntent::Removed {
        task: target.entity,
    });
    Ok(outcome)
}

// ------------------------------------------------------------- reclassifying

/// The one place a structural change asks whether it reopened something.
///
/// Called **after** the change has landed, naming the node that changed.
/// `Ok(None)` means nothing above it was completed, or `changed` is passive.
///
/// It is an explicit call and deliberately **not** a side effect of
/// [`WorkspaceDoc::insert`] or [`WorkspaceDoc::move_node`]: those primitives
/// are what every importer, paste, undo replay and structural op is built
/// from, and putting domain policy inside them would make a faithful restore
/// rewrite the history it was restoring. An importer's exemption is simply
/// that this function is not on its path, which is a thing a test can assert.
///
/// **Reopen propagates upward and never downward**: it reopens *every*
/// completed task ancestor, in one atomic group under one causation id, each
/// with reason [`LifecycleReason::UnfinishedChildAdded`]. Walking them all is
/// not a widening of SPEC — a task turning open is itself an unfinished
/// structural child of its own parent, so the chain is forced; one upward pass
/// is the same answer computed once.
pub fn reclassify(ws: &mut WorkspaceDoc, changed: NodeId) -> Result<Option<TaskOutcome>> {
    reclassify_inner(ws, changed, false)
}

/// [`reclassify`] for a **move**: the same decision, told as
/// `task.child.moved`. The classification cannot know which structural gesture
/// produced it, and guessing would file half the activity under the wrong
/// event.
pub fn reclassify_moved(ws: &mut WorkspaceDoc, changed: NodeId) -> Result<Option<TaskOutcome>> {
    reclassify_inner(ws, changed, true)
}

fn reclassify_inner(
    ws: &mut WorkspaceDoc,
    changed: NodeId,
    moved: bool,
) -> Result<Option<TaskOutcome>> {
    let workspace = ws.id().ok_or(StoreError::Malformed(
        "the document carries no workspace id",
    ))?;
    let class = classify_child(ws, changed)?;
    if class != ChildClass::UnfinishedStructural {
        return Ok(None);
    }
    let state = TaskState::of(ws, changed)?;
    if state.completed_task_ancestors.is_empty() {
        return Ok(None);
    }
    let lifecycle_def = lifecycle_def();
    let open = checked_token(&lifecycle_def, Lifecycle::Open.as_str())?;
    let mut work: Vec<(NodeId, Vec<Attr>)> = Vec::new();
    for &id in &state.completed_task_ancestors {
        let read = ws.read(id)?;
        work.push((
            id,
            read.content
                .attrs()
                .iter()
                .filter(|a| a.role == KEY_TASK_STATE)
                .cloned()
                .collect(),
        ));
    }
    // Read once and not per ancestor: the document segment is a fact about the
    // document, so asking it inside the map would be the same answer N times.
    let document = refs::document_segment(ws);
    let refs: Vec<EntityRef> = state
        .completed_task_ancestors
        .iter()
        .map(|&id| EntityRef::task(workspace, document, id))
        .collect();

    command_checkpoint(ws);
    ws.atomic(|ws| {
        for (id, existing) in work {
            ws.set_one_attr_raw(id, &lifecycle_def, existing, open.clone())?;
            ws.set_done(id, false)?;
        }
        Ok(())
    })?;

    // The nearest completed ancestor is the task this outcome is about; the
    // rest of the chain is `reopened_ancestors`.
    let nearest = refs[0];
    let mut outcome = TaskOutcome::new(nearest, TaskEffect::Reopened);
    outcome.reason = Some(LifecycleReason::UnfinishedChildAdded);
    for task in &refs {
        outcome.activity.push(ActivityIntent::Reopened {
            task: *task,
            reason: LifecycleReason::UnfinishedChildAdded,
        });
    }
    let child = ActivityIntent::ChildAdded {
        task: nearest,
        child: changed,
        class,
        reopened: true,
    };
    outcome
        .activity
        .push(if moved { child.as_moved() } else { child });
    outcome.reopened_ancestors = refs[1..].to_vec();
    Ok(Some(outcome))
}

/// A task reference for another node of the same document as `like`.
///
/// **Two of the three segments are carried rather than re-read**, and that is
/// the shape the containing-document convention bought: every node of one
/// `WorkspaceDoc` is in one document, so a sibling's reference differs from the
/// target's in its node segment and in nothing else. While the document segment
/// was derived per node this had to ask the document per call, and each answer
/// was different.
fn task_ref(like: EntityRef, node: NodeId) -> EntityRef {
    EntityRef::task(like.workspace_id(), like.document_id(), node)
}

// --------------------------------------------------------------- departing
//
// The other half of [`reclassify`], and the reason it is a separate shape: an
// arrival can be computed *after* the fact, because the ancestry a node landed
// in is still there to read. A departure cannot. The chain that answers "which
// task is this leaving" is exactly the chain the delete or the move is about to
// take away, so the question has to be asked first and the answer carried
// across the operation. [`Departing`] is that carried answer, and it is a value
// rather than two loose arguments so a call site cannot pair the wrong child
// with the wrong task.

/// The nearest ancestor of `node` that is a task at all, completed or not.
///
/// A second small walk beside [`TaskState`]'s, deliberately: `TaskState::of`
/// answers five questions and pays `any_structural_child` for one of them, and a
/// departure asks only this one — over every row of a multi-row delete, before
/// anything has happened yet.
fn nearest_task_ancestor(ws: &WorkspaceDoc, node: NodeId) -> Result<Option<NodeId>> {
    for ancestor in ws.ancestors(node)? {
        if is_task(&TaskFields::of(&ws.read(ancestor)?)) {
            return Ok(Some(ancestor));
        }
    }
    Ok(None)
}

/// **What one node is about to leave** — recorded before a structural delete or
/// move, spent after it.
///
/// Answered by [`departing`] and consumed by exactly one of [`deleted`] or
/// [`moved`], which is the whole of the type: the two endings differ, and a
/// caller that has recorded a departure has to say which one happened.
///
/// [`deleted`]: Self::deleted
/// [`moved`]: Self::moved
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Departing {
    /// The task the child was under when this was recorded.
    task: EntityRef,
    /// The node that is leaving.
    child: NodeId,
}

impl Departing {
    /// The task this departure would be filed under.
    pub fn task(&self) -> EntityRef {
        self.task
    }

    /// The node that is leaving.
    pub fn child(&self) -> NodeId {
        self.child
    }

    /// **The node was deleted**, so the departure is unconditional: there is no
    /// chain left to re-read and nothing the delete could have done except take
    /// it out of the task.
    pub fn deleted(self) -> ActivityIntent {
        ActivityIntent::ChildRemoved {
            task: self.task,
            child: self.child,
        }
    }

    /// **The node moved.** `None` when the recorded task is still one of its
    /// ancestors — an indent deeper inside the same task, a reorder among its
    /// own siblings, an outdent that stops short of leaving it. Only a move that
    /// actually took the child out of that task is a departure.
    ///
    /// Asked **after** the move has landed, on the same document, for the
    /// symmetric reason [`reclassify`] is: the answer is a fact about where the
    /// node ended up.
    pub fn moved(self, ws: &WorkspaceDoc) -> Result<Option<ActivityIntent>> {
        let still_there = ws
            .ancestors(self.child)
            .map(|chain| chain.contains(&self.task.node_id()))
            .unwrap_or(false);
        if still_there {
            return Ok(None);
        }
        Ok(Some(self.deleted()))
    }
}

/// **Ask, before a delete or a move, what task this node would leave.**
///
/// `None` — nothing is owed — in three cases, and they are the whole policy:
///
/// 1. the node has no task ancestor, so no task loses anything;
/// 2. the node is [`ChildClass::Passive`] — a note, a reference, a plain image.
///    `task.child.removed` is the departure half of `task.child.added`, and
///    that pair is about *work*: SPEC §5 gives passive content no standing
///    under a task on the way in, and giving it one on the way out would fill a
///    task's feed with the paragraphs somebody tidied out of it. **Structural
///    only** is therefore the reading, taken from the same [`classify_child`]
///    the arrival side uses so the two cannot drift into two opinions about
///    what a child is;
/// 3. the document carries no workspace identity — the same read every
///    command's preflight makes first, since a reference missing its first
///    segment is not one.
///
/// It reads and decides; it writes nothing, and it is **not** a side effect of
/// [`WorkspaceDoc::delete`] or [`WorkspaceDoc::move_node`] for the reason
/// [`reclassify`] is not a side effect of `insert`: those primitives are what
/// every importer, undo replay and internal container tidy-up is built from,
/// and a faithful restore must not mint the activity it is restoring. An
/// importer's exemption is that this function is not on its path.
///
/// **One departure per top.** A caller deleting a selection asks about the
/// selection's roots, not about everything under them — a node inside a deleted
/// subtree loses a task ancestor that is going with it, and an event filed
/// under a task that no longer exists is noise rather than history. That is the
/// same granularity the arrival side already keeps, where a moved root
/// classifies and its subtree does not.
pub fn departing(ws: &WorkspaceDoc, node: NodeId) -> Result<Option<Departing>> {
    if !classify_child(ws, node)?.is_structural() {
        return Ok(None);
    }
    if ws.id().is_none() {
        return Ok(None);
    }
    let Some(task) = nearest_task_ancestor(ws, node)? else {
        return Ok(None);
    };
    let Some(reference) = refs::task_ref(ws, task) else {
        return Ok(None);
    };
    Ok(Some(Departing {
        task: reference,
        child: node,
    }))
}

/// [`departing`] over a list of nodes, keeping only those that owe something.
///
/// The shape every call site actually wants: a delete and a structural move
/// both name a set of rows, and a `Vec` recorded before the operation is what
/// crosses it.
pub fn departing_all(ws: &WorkspaceDoc, nodes: &[NodeId]) -> Vec<Departing> {
    nodes
        .iter()
        .filter_map(|&node| departing(ws, node).ok().flatten())
        .collect()
}

// ------------------------------------------------------- the generic doors

/// What a generic attribute door should do with one key on one node
/// ([ADR-286]). One rule, asked in one place, so the editor, MCP and any
/// future surface route identically.
///
/// [ADR-286]: ../../../docs/decisions/ADR-276-300.md#adr-286-taskboundary--six-canonical-commands-own-task-state-and-every-host-is-a-thin-wrapper-around-them
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum AttributeRoute {
    /// A command-only key. The door refuses it; the caller runs a command.
    Refused,
    /// A due. Run [`set_due`] / [`clear_due`] rather than writing the value,
    /// so the marker is normalized in and the outcome exists.
    Due,
    /// Priority on a node that is already a task: the ordinary door, and then a
    /// `task.priority.changed` intent. Setting priority on a node that is not a
    /// task takes [`Ordinary`](Self::Ordinary) and creates nothing (SPEC §1).
    PriorityOnTask,
    /// The ordinary door, unchanged.
    Ordinary,
}

/// Route one attribute write. Refuses a due on a kind that cannot carry task
/// state, which is SPEC §4's exclusion enforced at the boundary rather than
/// only hidden in the UI.
pub fn route_attribute(ws: &WorkspaceDoc, node: NodeId, key: &str) -> Result<AttributeRoute> {
    if is_command_only_key(key) {
        return Ok(AttributeRoute::Refused);
    }
    if is_due_key(key) {
        let kind = ws.read(node)?.kind;
        if !task_capable(kind) {
            return Err(refuse(node, TaskRefusal::KindNotCapable));
        }
        return Ok(AttributeRoute::Due);
    }
    if key == KEY_PRIORITY && is_task_node(ws, node)? {
        return Ok(AttributeRoute::PriorityOnTask);
    }
    Ok(AttributeRoute::Ordinary)
}

// ------------------------------------------------------------------ buckets

/// SPEC §7's agenda bucket.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum Bucket {
    Today,
    Next,
    Upcoming,
    Later,
    Completed,
}

impl Bucket {
    pub const ALL: [Self; 5] = [
        Self::Today,
        Self::Next,
        Self::Upcoming,
        Self::Later,
        Self::Completed,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Today => "today",
            Self::Next => "next",
            Self::Upcoming => "upcoming",
            Self::Later => "later",
            Self::Completed => "completed",
        }
    }
}

/// The day that files an open task in `bucket`, or `None` for a bucket no date
/// can put it in.
///
/// **[`bucket`]'s inverse, written beside it and out of the same constants.**
/// A surface that wants a task to land where the reader is looking has to know
/// which date does that, and the only alternative is a second table of day
/// offsets somewhere in the UI — which is a second answer about the boundaries
/// the moment either moves.
///
/// - *Today* is today. Overdue is Today as well (`days <= 0`), but a capture
///   should not invent a date in the past.
/// - *Next* and *Upcoming* are the **first** day inside their window, so the
///   task sits at the near edge of the queue it was typed into rather than at an
///   arbitrary point in it.
/// - *Later* is `None`. A dateless task with no priority is already Later, and a
///   date would be a claim the person did not make. **This holds because a
///   capture sets no priority**: a dateless task at `High` or above is *Next*,
///   which is [`bucket`]'s dateless branch and is why that branch exists.
/// - *Completed* is `None`. No date reaches it — lifecycle does, and it is
///   answered before the due is read at all.
pub fn first_day_in(bucket: Bucket, today: DateOnly) -> Option<DateOnly> {
    let days = match bucket {
        Bucket::Today => 0,
        Bucket::Next => 1,
        Bucket::Upcoming => NEXT_DAYS + 1,
        Bucket::Later | Bucket::Completed => return None,
    };
    today
        .date()
        .checked_add_signed(chrono::Duration::days(days))
        .map(DateOnly::new)
}

/// A day within this many days of today is *Next*.
const NEXT_DAYS: i64 = 7;
/// …and within this many, *Upcoming*.
const UPCOMING_DAYS: i64 = 30;

/// SPEC §7's bucket: a pure function of five things and nothing else.
///
/// **Never persisted**, and re-evaluated at local midnight and on resume by
/// whoever displays it. The rules, in order:
///
/// 1. completed → `Completed`, whatever the due says;
/// 2. otherwise the calendar day is `None` for no due, the value itself for a
///    date, its date for a floating value, and its day *in `zone`* for a zoned
///    one;
/// 3. a day at or before today → `Today`. **Overdue lands in Today**, because
///    an overdue task is the answer to *what should I do next*;
/// 4. a day within seven days → `Next`;
/// 5. no day, and priority ≥ High → `Next`;
/// 6. a day within thirty days → `Upcoming`;
/// 7. everything else → `Later`.
///
/// `&dyn ZoneRules` rather than a generic, so a caller can hold one behind a
/// box and the function does not monomorphize per zone. **There is no default
/// and no `Option`**: this crate carries no timezone database, so a caller
/// always names the zone it is asking in, and the signature is where that is
/// enforced. Folding a zoned instant onto a day is legitimate for *placing* and
/// never for *selecting* — which is why `DateRange::contains_due` refuses to do
/// it and this does not.
pub fn bucket(
    due: Option<&Temporal>,
    lifecycle: Lifecycle,
    priority: Option<Priority>,
    today: DateOnly,
    zone: &dyn ZoneRules,
) -> Bucket {
    if lifecycle == Lifecycle::Completed {
        return Bucket::Completed;
    }
    let day = due.map(|value| match value {
        Temporal::Date(d) => *d,
        Temporal::Floating(f) => f.date(),
        Temporal::Zoned(z) => z.date_in(zone),
    });
    let Some(day) = day else {
        return if priority >= Some(Priority::High) {
            Bucket::Next
        } else {
            Bucket::Later
        };
    };
    let days = (day.date() - today.date()).num_days();
    if days <= 0 {
        Bucket::Today
    } else if days <= NEXT_DAYS {
        Bucket::Next
    } else if days <= UPCOMING_DAYS {
        Bucket::Upcoming
    } else {
        Bucket::Later
    }
}

// --------------------------------------------------------------- breadcrumb

/// The separator between breadcrumb segments.
const BREADCRUMB_JOIN: &str = " / ";
/// What replaces the middle when there are more segments than fit.
const BREADCRUMB_ELISION: &str = "…";
/// The longest one segment may be, in **characters** — never bytes, or the
/// rule behaves as three rules across eight languages.
const BREADCRUMB_SEGMENT_CHARS: usize = 60;
/// The most segments a breadcrumb shows.
const BREADCRUMB_SEGMENTS: usize = 8;

/// The breadcrumb a task shows, from segments the caller already holds.
///
/// **Pure and source-free.** The desktop passes ancestor titles it read from
/// the open document ([`WorkspaceDoc::ancestors`], then `koru_title::resolve_title`
/// per node); the projection passes titles it read from its own tables with a
/// recursive CTE. One separator, one elision rule, two independent sources —
/// and a command never reads a projection row to find out where it is, which is
/// the direction that matters.
///
/// The rule: non-empty segments only, root first, joined by `" / "`; each
/// segment trimmed, collapsed to a single line, and truncated to 60 characters;
/// at most 8 segments, with the middle replaced by `"…"` when there are more.
pub fn breadcrumb(segments: &[&str]) -> String {
    let kept: Vec<String> = segments
        .iter()
        .map(|segment| collapse(segment))
        .filter(|segment| !segment.is_empty())
        .collect();
    if kept.len() <= BREADCRUMB_SEGMENTS {
        return kept.join(BREADCRUMB_JOIN);
    }
    // First four, an elision, last three: eight items, with the middle gone.
    let head = 4;
    let tail = BREADCRUMB_SEGMENTS - head - 1;
    let mut out: Vec<&str> = kept[..head].iter().map(String::as_str).collect();
    out.push(BREADCRUMB_ELISION);
    out.extend(kept[kept.len() - tail..].iter().map(String::as_str));
    out.join(BREADCRUMB_JOIN)
}

/// One line, trimmed, no runs of whitespace, at most 60 characters.
fn collapse(segment: &str) -> String {
    let mut out = String::new();
    let mut space = false;
    for c in segment.trim().chars() {
        if c.is_whitespace() || c.is_control() {
            space = !out.is_empty();
            continue;
        }
        if space {
            out.push(' ');
            space = false;
        }
        if out.chars().count() >= BREADCRUMB_SEGMENT_CHARS {
            return out;
        }
        out.push(c);
    }
    out
}
